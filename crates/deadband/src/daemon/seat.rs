//! Counts a job that an earlier daemon granted and that still runs.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use super::{Shared, State, Waiter, finish, lock};
use crate::admit::JobId;
use crate::protocol::{Job, Label};

/// Counts `job` as running since `ran_ms` ago on the connection `stream`,
/// and removes it when its client says it ended or closes the connection.
/// A `ran_ms` that the clock cannot subtract counts from now.
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
