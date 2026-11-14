//! Takes a job's connection into the queue: a job that asks to start, and a
//! job that an earlier daemon granted and that still runs. Each one's
//! connection thread reads the job's working directory before it takes the
//! queue lock, and then waits on the connection for the job's end.
//! [`connection`] reads each connection's first line and serves it.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Instant;

use super::reply::deliver;
use super::{Shared, State, WRITE_TIMEOUT, Waiter, lock};
use crate::admit::{JobId, Threads};
use crate::history::{Arrival, Tally};
use crate::probe::ChildUsage;
use crate::protocol::{Ago, Exit, Job, Reply, Request, Short, read_line, write_line};

/// Serves one client on `stream`. A client that sends no first line within
/// the settings' `first_line_within` is closed.
pub(super) fn connection(shared: &Shared, stream: UnixStream) {
    let first_line_within = lock(shared).settings.first_line_within;
    let timeouts = stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .and_then(|()| stream.set_read_timeout(Some(first_line_within)));
    if timeouts.is_err() {
        return;
    }
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    match read_line::<Request>(&mut reader) {
        Ok(Some(Request::Ask(job))) => {
            // A granted job may run for hours, so its connection waits for
            // the end without a deadline.
            if stream.set_read_timeout(None).is_ok() {
                ask(shared, stream, &mut reader, job);
            }
        }
        Ok(Some(Request::Status)) => {
            let jobs = lock(shared).jobs();
            let _ = write_line(&mut &stream, &Reply::Jobs(jobs));
        }
        Ok(Some(Request::Timeline)) => {
            // The write has its timeout and happens after the lock is
            // released, so a client that does not read holds up no job. The
            // fit also runs after the lock is released.
            let timeline = lock(shared).timeline(Instant::now());
            let _ = write_line(&mut &stream, &Reply::Timeline(timeline.fit()));
        }
        Ok(Some(Request::Seat { job, threads, ran })) => {
            if stream.set_read_timeout(None).is_ok() {
                seat(shared, stream, &mut reader, job, threads, ran);
            }
        }
        Ok(Some(Request::Done { .. }) | None) | Err(_) => {}
    }
}

/// Reads what the daemon learns from `job` when it arrives, and fills the
/// job's workspace and short command from it. The workspace that a client
/// names is kept, and the walk fills it only when the client named none.
/// The short command always comes from the job's command line.
///
/// Returns what the history line needs, or `None` when the daemon records
/// no job. A daemon that records no job reads no warm directory. The lock
/// is released before the disk is read, so the disk never holds the queue
/// up.
fn arrive(shared: &Shared, job: &mut Job, ran: Option<Ago>) -> Option<Arrival> {
    let (recording, warm_dirs, kind_tools) = {
        let state = lock(shared);
        let config = &state.settings.reclaim;
        let recording = state.recorder.is_some();
        let warm_dirs = if recording {
            config.warm_dirs.clone()
        } else {
            Vec::new()
        };
        (recording, warm_dirs, config.kind_tools.clone())
    };
    let mut arrival = Arrival::read(job, &warm_dirs, &kind_tools, ran);
    job.short = Some(Short::new(&arrival.kind));
    if job.workspace.is_none() {
        job.workspace = arrival.place.workspace.take();
    }
    recording.then_some(arrival)
}

/// Queues `job` for the client on `stream`, grants it when the admission
/// rule allows, and removes it when the client says it ended, closes the
/// connection, or sends a line the daemon cannot read.
pub(super) fn ask(
    shared: &Shared,
    stream: UnixStream,
    reader: &mut BufReader<UnixStream>,
    mut job: Job,
) {
    let stream = Arc::new(stream);
    let arrival = arrive(shared, &mut job, None);
    let queued = Instant::now();
    let (id, replies) = {
        let mut state = lock(shared);
        let lane = job.lane;
        let ahead = state.queue.ahead_in(lane);
        let id = state.queue.push(
            Waiter {
                job,
                stream: Arc::clone(&stream),
                queued,
                arrival,
                ahead,
            },
            lane,
        );
        let mut replies = state.grant();
        if let Some(ahead) = state.queue.ahead_of(id) {
            let held = state.queue.held();
            // A client that cannot take this reply has its connection shut
            // down, which the read in `finish` sees.
            replies.tell(stream, Reply::Queued { ahead, held });
        }
        (id, replies)
    };
    deliver(shared, replies);
    finish(shared, reader, id);
}

/// Counts `job` as running since `ran` ago on the connection `stream`,
/// and removes it when its client says it ended or closes the connection.
/// On Linux and macOS an `Instant` holds signed seconds, so subtracting any
/// age succeeds, and a claim of `u64::MAX` milliseconds becomes a very large
/// age that only shows in `top`. A clock that cannot subtract the age makes
/// `checked_sub` return `None`, and the job then counts from now. The plain
/// subtraction `now - duration` panics in that case, and a client must not
/// be able to panic the daemon.
pub(super) fn seat(
    shared: &Shared,
    stream: UnixStream,
    reader: &mut BufReader<UnixStream>,
    mut job: Job,
    threads: Threads,
    ran: Ago,
) {
    let arrival = arrive(shared, &mut job, Some(ran));
    let now = Instant::now();
    let at = now.checked_sub(ran.duration()).unwrap_or(now);
    let id = lock(shared).seat(job, stream, threads, at, arrival);
    finish(shared, reader, id);
}

/// Waits for the end of the job numbered `id` on its connection, removes
/// the job, and starts the jobs that its place lets start.
fn finish(shared: &Shared, reader: &mut BufReader<UnixStream>, id: JobId) {
    let (exit, usage) = read_end(reader);
    let replies = {
        let mut state = lock(shared);
        state.record(id, Some(exit), usage);
        state.grant()
    };
    deliver(shared, replies);
}

/// Reads the job's connection until its client says how the job ended. A
/// connection that closes or sends a line the daemon cannot read leaves the
/// end unknown, with no usage.
fn read_end(reader: &mut BufReader<UnixStream>) -> (Exit, Option<ChildUsage>) {
    while let Ok(Some(request)) = read_line::<Request>(reader) {
        if let Request::Done {
            exit_code,
            signal,
            usage,
        } = request
        {
            return (Exit::reported(exit_code, signal), usage);
        }
    }
    (Exit::Lost, None)
}

impl State {
    /// Places `job` among the running jobs, started at `at`. The job has
    /// no record of how long it waited, so it counts as asking when it
    /// started.
    fn seat(
        &mut self,
        job: Job,
        stream: UnixStream,
        threads: Threads,
        at: Instant,
        arrival: Option<Arrival>,
    ) -> JobId {
        let limits = self.limits();
        let lane = job.lane;
        let waiter = Waiter {
            job,
            stream: Arc::new(stream),
            queued: at,
            arrival,
            ahead: 0,
        };
        let id = self.queue.seat(waiter, lane, threads, at, &limits);
        let running = self.note_running();
        self.tallies.insert(id, Tally::seated(running));
        id
    }
}
