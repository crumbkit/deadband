//! The lease that a granted job holds, and the thread that keeps the job
//! counted when the daemon goes away.
//!
//! A lease holds one connection to the daemon. One detached thread blocks
//! reading that connection and passes each line about the job's activity to
//! the lease's [`Notices`]. When the read ends, the daemon has gone or
//! closed the connection, so the thread drops the connection, which frees
//! any place the old daemon still counts, and tries once per
//! [`RETRY_EVERY`] to connect and send [`Request::Seat`]. A new daemon then
//! counts the job at once.
//!
//! This is a soft limit. A new daemon can admit a job in the moment before
//! the running jobs have seated themselves, which lasts at most about one
//! [`RETRY_EVERY`] after it starts listening. Closing that gap would need
//! the daemon to delay every admission after it starts.
//!
//! Nothing here touches the job. The job's own thread never waits for the
//! watcher, and a watcher that cannot start leaves a lease that nothing
//! watches.

use std::fmt;
use std::io::{self, BufReader};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::connect;
use crate::protocol::{Job, Reply, Request, read_line, write_line};
use crate::stuck::{active_line, still_line, stuck_line};

/// How long the watcher waits before each try to reach a daemon. A daemon
/// that never returns costs one failed connection per this length of time.
const RETRY_EVERY: Duration = Duration::from_millis(100);

/// How long the watcher waits to write its seat. A daemon that takes no
/// line in this time counts as gone.
const WRITE_WITHIN: Duration = Duration::from_secs(2);

/// Where the lines that say a job is stuck, or active again, go. The
/// lease's thread calls it, so a line can arrive at any time while the job
/// runs.
#[derive(Clone)]
pub struct Notices(Arc<dyn Fn(&str) + Send + Sync>);

impl Notices {
    /// Returns the notices that pass each line to `tell`.
    pub fn new(tell: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self(Arc::new(tell))
    }

    /// Passes `line` on.
    fn tell(&self, line: &str) {
        (self.0)(line);
    }
}

impl fmt::Debug for Notices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Notices")
    }
}

/// A granted job's connection to the daemon.
#[derive(Debug)]
pub struct Lease {
    /// The connection, which the watcher replaces when the daemon goes.
    seat: Arc<Seat>,
    /// The threads the daemon gave the job.
    jobs: usize,
}

impl Lease {
    /// Returns the lease of `job`, which the daemon at `path` granted just
    /// now on `stream` with `jobs` threads, and starts its watcher.
    pub(super) fn watched(
        stream: UnixStream,
        jobs: usize,
        path: &Path,
        job: &Job,
        notices: Option<&Notices>,
    ) -> Self {
        let clone = stream.try_clone();
        let seat = Arc::new(Seat::new(stream));
        if let Ok(read) = clone {
            let watcher = Watcher {
                seat: Arc::clone(&seat),
                path: path.to_owned(),
                job: job.clone(),
                jobs,
                started: Instant::now(),
                notices: notices.cloned(),
            };
            // A thread that cannot start leaves the lease unwatched, and
            // the job runs as it would with no restart.
            let _ = thread::Builder::new()
                .name("deadband-lease".to_owned())
                .spawn(move || watcher.run(read));
        }
        Self { seat, jobs }
    }

    /// Returns the threads the daemon gave the job.
    #[must_use]
    pub const fn jobs(&self) -> usize {
        self.jobs
    }

    /// Tells the daemon that the job ended with `exit_code` and closes the
    /// connection. A daemon that cannot be told sees the connection close,
    /// which frees the job's place as well.
    pub fn end(self, exit_code: Option<i32>) {
        self.send_done(exit_code, None);
    }

    /// Tells the daemon how the job ended, the exit code or the signal, and
    /// closes the connection, as [`Lease::end`] does.
    pub fn end_status(self, status: ExitStatus) {
        self.send_done(status.code(), status.signal());
    }

    /// Writes the `Done` line to the current connection, if the lease has
    /// one. A failed write closes the connection all the same when `self`
    /// drops.
    fn send_done(self, exit_code: Option<i32>, signal: Option<i32>) {
        if let SeatState::Held(stream) = &*self.seat.lock() {
            let _ = write_line(&mut &*stream, &Request::Done { exit_code, signal });
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.seat.end();
    }
}

/// The lease's connection, which the job's thread and the watcher share.
#[derive(Debug)]
pub(super) struct Seat {
    /// Where the lease stands.
    state: Mutex<SeatState>,
    /// Wakes the watcher when the lease ends.
    changed: Condvar,
}

/// Where a lease stands.
#[derive(Debug)]
enum SeatState {
    /// The lease holds this connection.
    Held(UnixStream),
    /// The connection closed, and the watcher looks for a daemon.
    Lost,
    /// The job ended.
    Ended,
}

impl Seat {
    /// Returns a seat that holds `stream`.
    pub(super) const fn new(stream: UnixStream) -> Self {
        Self {
            state: Mutex::new(SeatState::Held(stream)),
            changed: Condvar::new(),
        }
    }

    /// Locks the state. A panic while the lock was held left a whole
    /// state, because every change to it is one assignment, and the job's
    /// thread must keep going.
    fn lock(&self) -> MutexGuard<'_, SeatState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Marks the job as ended, and wakes the watcher. Shutting the
    /// connection down also ends the watcher's blocked read, so the
    /// watcher's clone cannot keep the connection open.
    pub(super) fn end(&self) {
        let mut state = self.lock();
        if let SeatState::Held(stream) = &*state {
            let _ = stream.shutdown(Shutdown::Both);
        }
        *state = SeatState::Ended;
        self.changed.notify_all();
    }

    /// Drops the current connection unless the job has ended.
    fn lose(&self) {
        let mut state = self.lock();
        if matches!(*state, SeatState::Held(_)) {
            *state = SeatState::Lost;
        }
    }

    /// Waits for [`RETRY_EVERY`], and returns at once with `true` when the
    /// job has ended.
    fn ended_after_wait(&self) -> bool {
        let state = self
            .changed
            .wait_timeout_while(self.lock(), RETRY_EVERY, |state| {
                !matches!(state, SeatState::Ended)
            })
            .map_or_else(|poisoned| poisoned.into_inner().0, |(state, _)| state);
        matches!(*state, SeatState::Ended)
    }

    /// Makes `stream` the current connection and returns `true`, or closes
    /// it and returns `false` when the job has ended.
    pub(super) fn install(&self, stream: UnixStream) -> bool {
        let mut state = self.lock();
        if matches!(*state, SeatState::Ended) {
            return false;
        }
        *state = SeatState::Held(stream);
        true
    }
}

/// What the detached thread of one lease knows.
struct Watcher {
    /// The connection that it shares with the job's thread.
    seat: Arc<Seat>,
    /// The daemon's socket.
    path: PathBuf,
    /// The job as its client described it.
    job: Job,
    /// The threads of the job's grant.
    jobs: usize,
    /// When the daemon granted the job.
    started: Instant,
    /// Where the lines about the job's activity go.
    notices: Option<Notices>,
}

impl Watcher {
    /// Blocks on the connection until it closes, seats the job again on
    /// the next daemon, and repeats until the job ends.
    fn run(self, mut read: UnixStream) {
        loop {
            // The first answer's read timeout may still be set on this
            // connection. macOS refuses this call once the peer has
            // closed, and the read then returns at once.
            let _ = read.set_read_timeout(None);
            self.listen(read);
            // Both handles of the old connection close before this point, so
            // the old daemon has freed the place before a new connection
            // opens.
            self.seat.lose();
            match self.seat_again() {
                Some(next) => read = next,
                None => return,
            }
        }
    }

    /// Reads the daemon's lines until the connection closes, and passes the
    /// ones about the job's activity to the notices. A line that cannot be
    /// parsed is ignored, and only end of file or a read error counts as the
    /// connection closing.
    fn listen(&self, read: UnixStream) {
        let mut reader = BufReader::new(read);
        loop {
            match read_line::<Reply>(&mut reader) {
                Ok(Some(reply)) => self.tell(&reply),
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {}
                Ok(None) | Err(_) => return,
            }
        }
    }

    /// Passes the line that `reply` says to the notices, if it says one.
    fn tell(&self, reply: &Reply) {
        let line = match reply {
            Reply::Stuck {
                quiet,
                again: false,
                busiest,
            } => stuck_line(quiet.duration(), busiest.as_ref()),
            Reply::Stuck {
                quiet,
                again: true,
                busiest,
            } => still_line(quiet.duration(), busiest.as_ref()),
            Reply::Active { quiet } => active_line(quiet.duration()),
            _ => return,
        };
        if let Some(notices) = &self.notices {
            notices.tell(&line);
        }
    }

    /// Tries once per [`RETRY_EVERY`] to seat the job, and returns a handle
    /// of the new connection, or `None` when the job has ended.
    fn seat_again(&self) -> Option<UnixStream> {
        loop {
            if self.seat.ended_after_wait() {
                return None;
            }
            let Some((stream, read)) = self.try_seat() else {
                continue;
            };
            return self.seat.install(stream).then_some(read);
        }
    }

    /// Connects to the daemon and sends the seat, and returns two handles
    /// of the connection, or `None` when any step fails.
    fn try_seat(&self) -> Option<(UnixStream, UnixStream)> {
        let stream = connect(&self.path).ok()?;
        stream.set_write_timeout(Some(WRITE_WITHIN)).ok()?;
        let seat = Request::Seat {
            job: self.job.clone(),
            jobs: self.jobs,
            ran_ms: u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
        };
        write_line(&mut &stream, &seat).ok()?;
        let read = stream.try_clone().ok()?;
        Some((stream, read))
    }
}
