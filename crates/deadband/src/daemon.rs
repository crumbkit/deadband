//! The per-user daemon: it listens on the socket, samples the probe and the
//! disk that holds the home directory, and starts waiting jobs as
//! [`admit`](crate::admit) allows.
//!
//! The daemon runs one thread that samples the probe every
//! [`SAMPLE_EVERY`] and one thread per connection. Each thread that changes
//! the queue or the pressure starts every job that the admission rule then
//! lets start, under the one lock that guards the queue, and writes
//! [`Reply::Granted`] to that job's client. The daemon never starts, stops
//! or signals a job; each client runs its own job.

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use rustix::io::Errno;

use self::reading::{priority_active, read_machine};
use self::timeline::Ended;
use crate::admit::{Hold, JobId, Limits, Queue, Reading, disk_critical};
use crate::probe::{Pressure, Probe};
use crate::protocol::{Exit, Job, Jobs, Label, Reply, Request, read_line, write_line};
use crate::reclaim::{self, Reclaimed, Resource, Watch};

mod bind;
mod reading;
mod seat;
mod settings;
#[cfg(test)]
mod tests;
mod timeline;

pub use self::bind::{
    Bound, DAEMON_RUNNING, SOCKET_UNUSABLE, Unbound, bind, lock_path, raise_descriptor_limit,
};
pub use self::settings::{Settings, default_max_jobs};

/// How often the daemon samples the probe.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(1);

/// How long the daemon waits for a client to take a reply before it counts
/// the client as gone.
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// How long the daemon waits for a new connection's first line before it
/// closes the connection.
pub const FIRST_LINE_WITHIN: Duration = Duration::from_secs(2);

/// How long the daemon waits after a failed accept before it accepts
/// again, so a lasting failure such as running out of descriptors does not
/// spin a core.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// Where the daemon writes its log lines, which several threads share.
pub type Log = Arc<Mutex<dyn Write + Send>>;

/// A job that waits or runs, with the connection to its client.
#[derive(Debug)]
struct Waiter {
    /// The job as its client described it.
    job: Job,
    /// The connection that the daemon writes the grant to.
    stream: UnixStream,
    /// The job's label as `deadband top` shows it.
    label: Label,
    /// When the job asked to start.
    queued: Instant,
}

/// What the threads of the daemon share.
struct State {
    /// The waiting and running jobs.
    queue: Queue<Waiter>,
    /// The latest reading of the machine, or `None` when the probe failed.
    reading: Option<Reading>,
    /// Whether the priority file held an entry at the latest reading.
    reserve_lanes: bool,
    /// How the daemon admits jobs.
    settings: Settings,
    /// When the daemon last reclaimed and whether a reclaim runs.
    watch: Watch,
    /// The last reclaim, for `deadband status`.
    reclaimed: Option<Reclaimed>,
    /// When the daemon started.
    up: Instant,
    /// The jobs that have ended, the oldest first.
    ended: VecDeque<Ended>,
}

impl State {
    /// Returns the state of a daemon that starts now and has no job.
    fn new(settings: Settings, reading: Option<Reading>, reserve_lanes: bool) -> Self {
        Self {
            queue: Queue::default(),
            reading,
            reserve_lanes,
            settings,
            watch: Watch::default(),
            reclaimed: None,
            up: Instant::now(),
            ended: VecDeque::new(),
        }
    }

    /// Returns the limits that the daemon admits jobs within.
    const fn limits(&self) -> Limits {
        Limits {
            max_jobs: self.settings.max_jobs,
            cores: self.settings.cores,
            reserve_lanes: self.reserve_lanes,
        }
    }

    /// Starts every waiting job that the admission rule lets start, and
    /// writes the grant to each one's client. A client that cannot take
    /// the grant is gone, so its job leaves the queue and frees its place.
    fn grant(&mut self) {
        let limits = self.limits();
        loop {
            let started = self
                .queue
                .admit(self.reading.as_ref(), &limits, Instant::now());
            if started.is_empty() {
                return;
            }
            for (id, jobs) in started {
                let granted = Reply::Granted { jobs };
                let delivered = self
                    .queue
                    .running_job(id)
                    .is_some_and(|waiter| write_line(&mut &waiter.stream, &granted).is_ok());
                if !delivered {
                    self.record(id, None);
                }
            }
        }
    }

    /// Returns the running and waiting jobs.
    fn jobs(&self) -> Jobs {
        Jobs {
            max_jobs: self.settings.max_jobs.get(),
            running: self.queue.running().map(|each| each.job.clone()).collect(),
            waiting: self.queue.waiting().map(|each| each.job.clone()).collect(),
            held: self.queue.held(),
            reclaim: self.reclaimed.clone(),
        }
    }

    /// Feeds the latest reading and hold to the reclaim rule, and returns
    /// what to reclaim now. Memory counts as short while pressure is above
    /// normal or a job waits for memory.
    fn reclaim_now(&mut self) -> Option<Resource> {
        let reading = self.reading.as_ref();
        let memory_short = reading.is_some_and(|reading| reading.pressure != Pressure::Normal)
            || matches!(self.queue.held(), Some(Hold::Memory { .. }));
        let disk_short = reading
            .and_then(|reading| reading.disk)
            .is_some_and(|disk| disk_critical(&disk));
        self.watch.observe(
            &self.settings.reclaim,
            memory_short,
            disk_short,
            Instant::now(),
        )
    }
}

/// The state that every thread of the daemon locks.
type Shared = Arc<Mutex<State>>;

/// Locks `shared`. A thread that panicked while it held the lock left the
/// queue whole, because every change to it is one call, so the daemon
/// keeps using it.
fn lock(shared: &Shared) -> MutexGuard<'_, State> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Writes `line` to `log` after `deadband: `. A log that cannot be written
/// loses the line.
pub fn note(log: &Log, line: fmt::Arguments<'_>) {
    let mut log = log.lock().unwrap_or_else(PoisonError::into_inner);
    let _ = writeln!(log, "deadband: {line}");
}

/// Serves clients on `listener` until accepting a connection fails in a way
/// that another try cannot fix, and returns that error. It samples `probe`
/// every `settings.sample_every`.
///
/// A failed accept that can pass, such as running out of descriptors or a
/// client that gave up while it connected, is logged, and the daemon
/// accepts again after 100 ms.
pub fn serve<P: Probe + Send + 'static>(
    listener: &UnixListener,
    probe: P,
    settings: Settings,
    log: &Log,
) -> io::Error {
    let reading = read_machine(&probe, &settings, log, None);
    let reserve_lanes = priority_active(settings.priority_file.as_deref());
    let shared: Shared = Arc::new(Mutex::new(State::new(settings, reading, reserve_lanes)));
    // The sampler stops when `serve` returns and drops `_stop`.
    let (_stop, stopped) = mpsc::channel::<()>();
    let sampler = Arc::clone(&shared);
    let sampler_log = Arc::clone(log);
    thread::spawn(move || sample(&sampler, &probe, &stopped, &sampler_log));
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                let shared = Arc::clone(&shared);
                thread::spawn(move || connection(&shared, stream));
            }
            Err(error) if passing(&error) => {
                note(
                    log,
                    format_args!("accepting a connection failed: {error}; the daemon tries again"),
                );
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the backoff after a failed accept, whose failure has no event to wait on; ACCEPT_BACKOFF names it"
                )]
                thread::sleep(ACCEPT_BACKOFF);
            }
            Err(error) => return error,
        }
    }
}

/// Reports whether a failed accept can pass, so another try may succeed.
fn passing(error: &io::Error) -> bool {
    [
        Errno::MFILE,
        Errno::NFILE,
        Errno::NOBUFS,
        Errno::NOMEM,
        Errno::CONNABORTED,
        Errno::INTR,
        Errno::AGAIN,
        Errno::PROTO,
    ]
    .contains(&Errno::from_io_error(error).unwrap_or(Errno::INVAL))
}

/// Samples `probe` every `sample_every` and starts the jobs that the new
/// reading admits, until `stopped` receives or disconnects.
///
/// The probe has no event to wait on, so the sampler polls it. The wait
/// is a timed receive on the daemon's stop channel.
fn sample(shared: &Shared, probe: &impl Probe, stopped: &mpsc::Receiver<()>, log: &Log) {
    let every = lock(shared).settings.sample_every;
    while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
        let (before, settings) = {
            let state = lock(shared);
            (state.reading, state.settings.clone())
        };
        let reading = read_machine(probe, &settings, log, Some(before));
        let reserve_lanes = priority_active(settings.priority_file.as_deref());
        let mut state = lock(shared);
        state.reading = reading;
        state.reserve_lanes = reserve_lanes;
        state.grant();
        if let Some(resource) = state.reclaim_now() {
            let config = state.settings.reclaim.clone();
            let quiet = state.watch.quiet(resource);
            drop(state);
            start_reclaim(shared, config, resource, quiet, log);
        }
    }
}

/// Runs the reclaimers of `config` for `resource` on a thread of their
/// own, so admission never waits for them, and records each outcome for
/// `deadband status`. `quiet` says the last reclaim of `resource` freed
/// nothing.
fn start_reclaim(
    shared: &Shared,
    config: reclaim::Config,
    resource: Resource,
    quiet: bool,
    log: &Log,
) {
    let shared = Arc::clone(shared);
    let log = Arc::clone(log);
    thread::spawn(move || {
        let freed = reclaim::reclaim(&config, resource, quiet, &log, &mut |reclaimed| {
            lock(&shared).reclaimed = Some(reclaimed);
        });
        lock(&shared).watch.ended(freed, Instant::now());
    });
}

/// Serves one client on `stream`. A client that sends no first line within
/// the settings' `first_line_within` is closed.
fn connection(shared: &Shared, stream: UnixStream) {
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
            // released, so a client that does not read holds up no job.
            let timeline = lock(shared).timeline(Instant::now());
            let _ = write_line(&mut &stream, &Reply::Timeline(timeline));
        }
        Ok(Some(Request::Seat { job, jobs, ran_ms })) => {
            if stream.set_read_timeout(None).is_ok() {
                seat::seat(shared, stream, &mut reader, job, jobs, ran_ms);
            }
        }
        Ok(Some(Request::Done { .. }) | None) | Err(_) => {}
    }
}

/// Queues `job` for the client on `stream`, grants it when the admission
/// rule allows, and removes it when the client says it ended, closes the
/// connection, or sends a line the daemon cannot read.
fn ask(shared: &Shared, stream: UnixStream, reader: &mut BufReader<UnixStream>, job: Job) {
    let Ok(grant_stream) = stream.try_clone() else {
        return;
    };
    let queued = Instant::now();
    let label = Label::new(&job.label);
    let id: JobId = {
        let mut state = lock(shared);
        let lane = job.lane;
        let id = state.queue.push(
            Waiter {
                job,
                stream: grant_stream,
                label,
                queued,
            },
            lane,
        );
        state.grant();
        if let Some(ahead) = state.queue.ahead_of(id) {
            let held = state.queue.held();
            // A client that cannot take this reply also closes its
            // connection, which the read below sees.
            let _ = write_line(&mut &stream, &Reply::Queued { ahead, held });
        }
        id
    };
    finish(shared, reader, id);
}

/// Waits for the end of the job numbered `id` on its connection, removes
/// the job, and starts the jobs that its place lets start.
fn finish(shared: &Shared, reader: &mut BufReader<UnixStream>, id: JobId) {
    let exit = read_end(reader);
    let mut state = lock(shared);
    state.record(id, Some(exit));
    state.grant();
}

/// Reads the job's connection until its client says how the job ended. A
/// connection that closes or sends a line the daemon cannot read leaves the
/// end unknown.
fn read_end(reader: &mut BufReader<UnixStream>) -> Exit {
    while let Ok(Some(request)) = read_line::<Request>(reader) {
        if let Request::Done { exit_code, signal } = request {
            return Exit::reported(exit_code, signal);
        }
    }
    Exit::Lost
}
