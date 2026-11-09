//! Counts a job that an earlier daemon granted and that still runs.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use super::{Shared, State, Waiter, finish, lock};
use crate::admit::JobId;
use crate::protocol::{Job, Label};

/// Counts `job` as running since `ran_ms` ago on the connection `stream`,
/// and removes it when its client says it ended or closes the connection.
/// On Linux and macOS an `Instant` holds signed seconds, so subtracting any
/// `ran_ms` succeeds, and a claim of `u64::MAX` becomes a very large age that
/// only shows in `top`. A clock that cannot subtract the age makes
/// `checked_sub` return `None`, and the job then counts from now. The plain
/// subtraction `now - duration` panics in that case, and a client must not
/// be able to panic the daemon.
pub(super) fn seat(
    shared: &Shared,
    stream: UnixStream,
    reader: &mut BufReader<UnixStream>,
    job: Job,
    jobs: usize,
    ran_ms: u64,
) {
    let now = Instant::now();
    let at = now
        .checked_sub(Duration::from_millis(ran_ms))
        .unwrap_or(now);
    let id = lock(shared).seat(job, stream, jobs, at);
    finish(shared, reader, id);
}

impl State {
    /// Places `job` among the running jobs, started at `at`. The job has
    /// no record of how long it waited, so it counts as asking when it
    /// started.
    fn seat(&mut self, job: Job, stream: UnixStream, jobs: usize, at: Instant) -> JobId {
        let limits = self.limits();
        let lane = job.lane;
        let label = Label::new(&job.label);
        let waiter = Waiter {
            job,
            stream,
            label,
            queued: at,
        };
        self.queue.seat(waiter, lane, jobs, at, &limits)
    }
}
