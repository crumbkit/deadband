//! The grant and queued replies that the daemon writes to jobs' clients.
//! A thread decides each reply under the queue lock and collects it in
//! [`Replies`], and [`deliver`] writes the replies after the thread has
//! released the lock, so a client that does not read never holds the lock.
//!
//! [`deliver`] writes the replies one after another. Each write waits at
//! most the connection's write timeout, which `connection` sets to
//! [`WRITE_TIMEOUT`](super::WRITE_TIMEOUT) (1 s), and a reply that is not
//! written by then has failed. A client that does not read therefore delays
//! the later replies of the same delivery by at most that second, and
//! delays no other thread.
//!
//! A failed grant frees its job's place, as a client that has gone does, and
//! the daemon then shuts the failed reply's connection down. A client that
//! had read no queued reply, or that reads a partial line, starts its job at
//! once. A client that had read a queued reply asks again every 100 ms for
//! up to 5 seconds, so a daemon that is still serving queues it again, and
//! it starts its job once no daemon has kept it for those 5 seconds.

use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Instant;

use super::{Shared, State, lock};
use crate::admit::JobId;
use crate::protocol::{Reply, write_line};

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
                    Some(stream) => replies.grant(id, stream, Reply::Granted { jobs }),
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
    pub(super) fn grant(&mut self, id: JobId, stream: Arc<UnixStream>, reply: Reply) {
        self.0.push(Outgoing {
            stream,
            reply,
            granted: Some(id),
        });
    }

    /// Adds `reply`, which tells a waiting job's client on `stream` where
    /// the job stands. When it cannot be written, the job's own connection
    /// thread sees the connection close and removes the job.
    pub(super) fn tell(&mut self, stream: Arc<UnixStream>, reply: Reply) {
        self.0.push(Outgoing {
            stream,
            reply,
            granted: None,
        });
    }

    /// Writes each reply to its client in turn, and returns the replies
    /// whose write failed or timed out.
    fn send(self) -> Vec<Outgoing> {
        self.0
            .into_iter()
            .filter(|outgoing| write_line(&mut &*outgoing.stream, &outgoing.reply).is_err())
            .collect()
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
    /// The reply.
    reply: Reply,
    /// The job whose grant this is, or `None` for a queued reply.
    granted: Option<JobId>,
}
