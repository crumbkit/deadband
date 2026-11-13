//! The grant and queued replies that the daemon writes to jobs' clients.
//! A thread decides each reply under the queue lock and collects it in
//! [`Replies`], and [`deliver`] writes the replies after the thread has
//! released the lock, so a client that does not read holds up no other job.
//!
//! [`deliver`] polls all the replies' sockets together and writes each
//! reply when its socket can take it, so the clients that read get their
//! replies at once. A socket whose buffer is full, because its client does
//! not read, waits until [`WRITE_TIMEOUT`] after the round of delivery
//! began. Every such socket shares that one deadline, so any number of
//! clients that do not read delay the delivering thread by at most
//! [`WRITE_TIMEOUT`] a round, and delay no other thread. A reply that is not
//! whole by then has failed. A failed grant frees a place, and the jobs that
//! the place then starts get their grants in the next round.
//!
//! A failed grant frees its job's place, as a client that has gone does, and
//! the daemon shuts the failed reply's connection down, so its client sees
//! the daemon close and starts its job at once.

use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, poll};
use rustix::io::Errno;
use rustix::net::{SendFlags, send};

use super::alive::timespec;
use super::{Shared, State, WRITE_TIMEOUT, lock};
use crate::admit::JobId;
use crate::protocol::{Reply, line};

impl State {
    /// Starts every waiting job that the admission rule lets start, and
    /// returns their grants, which the caller writes with [`deliver`] after
    /// it releases the lock. A client that cannot take its grant is gone,
    /// so [`deliver`] removes its job, which frees its place.
    pub(super) fn grant(&mut self) -> Replies {
        let limits = self.limits();
        let mut replies = Replies::default();
        loop {
            let now = Instant::now();
            let reading = self.fresh_reading(now).copied();
            let started = self.queue.admit(reading.as_ref(), &limits, now);
            if started.is_empty() {
                return replies;
            }
            for (id, jobs) in started {
                self.count_started(id);
                let stream = self
                    .queue
                    .running_job(id)
                    .map(|job| Arc::clone(&job.stream));
                match stream {
                    Some(stream) => replies.grant(id, stream, &Reply::Granted { jobs }),
                    None => self.record(id, None, None),
                }
            }
        }
    }
}

/// The replies that a thread decided under the queue lock, which it sends
/// with [`deliver`] once it has released the lock.
#[derive(Debug, Default)]
#[must_use = "a client gets its reply only when the replies are delivered"]
pub(super) struct Replies(Vec<Outgoing>);

impl Replies {
    /// Adds the grant `reply` of the job numbered `id` for its client on
    /// `stream`. When the grant cannot be written, the job leaves the queue.
    pub(super) fn grant(&mut self, id: JobId, stream: Arc<UnixStream>, reply: &Reply) {
        self.0.push(Outgoing::new(stream, reply, Some(id)));
    }

    /// Adds `reply`, which tells a waiting job's client on `stream` where
    /// the job stands. When it cannot be written, the job's own connection
    /// thread sees the connection close and removes the job.
    pub(super) fn tell(&mut self, stream: Arc<UnixStream>, reply: &Reply) {
        self.0.push(Outgoing::new(stream, reply, None));
    }

    /// Writes every reply to its client, and returns the replies that were
    /// not whole by [`WRITE_TIMEOUT`] after the call began or whose write
    /// failed.
    fn send(self) -> Vec<Outgoing> {
        let deadline = Instant::now() + WRITE_TIMEOUT;
        let mut failed = Vec::new();
        let mut pending = self.0;
        while !pending.is_empty() {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(ready) = writable(&pending, left).filter(|_| !left.is_zero()) else {
                failed.append(&mut pending);
                break;
            };
            let mut waiting = Vec::new();
            for (mut outgoing, ready) in pending.into_iter().zip(ready) {
                let step = if ready {
                    outgoing.offer()
                } else {
                    Step::Waiting
                };
                match step {
                    Step::Sent => {}
                    Step::Waiting => waiting.push(outgoing),
                    Step::Failed => failed.push(outgoing),
                }
            }
            pending = waiting;
        }
        failed
    }
}

/// Waits until at least one socket of `pending` can take more of its line
/// or `within` passes, and returns whether each one can. An interrupted
/// wait reports that none can, so the caller waits again. A wait that fails
/// returns `None`.
fn writable(pending: &[Outgoing], within: Duration) -> Option<Vec<bool>> {
    let mut fds: Vec<PollFd<'_>> = pending
        .iter()
        .map(|outgoing| PollFd::new(&*outgoing.stream, PollFlags::OUT))
        .collect();
    match poll(&mut fds, Some(&timespec(within))) {
        Ok(_) | Err(Errno::INTR) => Some(fds.iter().map(|fd| !fd.revents().is_empty()).collect()),
        Err(_) => None,
    }
}

/// Writes `replies` with the queue lock of `shared` released. Each grant
/// that cannot be written frees its job's place, which may start further
/// jobs, and their grants are delivered the same way. The connection of
/// each failed reply is shut down after its job has left the queue, so the
/// job is recorded once, as having left.
pub(super) fn deliver(shared: &Shared, mut replies: Replies) {
    while !replies.0.is_empty() {
        let failed = replies.send();
        if failed.is_empty() {
            return;
        }
        let mut state = lock(shared);
        for id in failed.iter().filter_map(|outgoing| outgoing.granted) {
            state.record(id, None, None);
        }
        replies = state.grant();
        drop(state);
        for outgoing in &failed {
            // A connection that is already closed refuses the shutdown,
            // which leaves nothing to do.
            let _ = outgoing.stream.shutdown(Shutdown::Both);
        }
    }
}

/// One reply on its way to its client.
#[derive(Debug)]
struct Outgoing {
    /// The connection to the client.
    stream: Arc<UnixStream>,
    /// The reply as one JSON line, or `None` when JSON could not represent
    /// it, which counts as a failed write.
    line: Option<Vec<u8>>,
    /// How many bytes of the line the socket has taken.
    sent: usize,
    /// The job whose grant this is, or `None` for a queued reply.
    granted: Option<JobId>,
}

/// Where one reply stands after an offer to its socket.
enum Step {
    /// The socket took the whole line.
    Sent,
    /// The socket's buffer is full, so the rest of the line waits.
    Waiting,
    /// The write failed, or the line could not be made.
    Failed,
}

impl Outgoing {
    fn new(stream: Arc<UnixStream>, reply: &Reply, granted: Option<JobId>) -> Self {
        Self {
            stream,
            line: line(reply).ok(),
            sent: 0,
            granted,
        }
    }

    /// Offers the rest of the line to a socket that `poll` reported can
    /// take more. On Linux the send never waits. macOS ignores
    /// `MSG_DONTWAIT` on a Unix socket, so there the report keeps the send
    /// from waiting: a socket reported writable has room for its low-water
    /// mark, which is longer than a grant or queued line. A send that still
    /// finds too little room waits at most the connection's write timeout,
    /// which `connection` sets to [`WRITE_TIMEOUT`].
    fn offer(&mut self) -> Step {
        let Some(rest) = self.line.as_ref().and_then(|line| line.get(self.sent..)) else {
            return Step::Failed;
        };
        match send(&*self.stream, rest, SendFlags::DONTWAIT) {
            Ok(taken) if taken == rest.len() => Step::Sent,
            Ok(taken) => {
                self.sent += taken;
                Step::Waiting
            }
            Err(Errno::AGAIN | Errno::INTR) => Step::Waiting,
            Err(_) => Step::Failed,
        }
    }
}
