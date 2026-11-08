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

use std::fmt;
use std::io::{self, BufReader, Write};
use std::num::NonZeroUsize;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use rustix::io::Errno;

use self::reading::{priority_active, read_machine};
use crate::admit::{Hold, JobId, Limits, Queue, Reading, disk_critical};
use crate::probe::{Pressure, Probe};
use crate::protocol::{Job, Jobs, Reply, Request, read_line, write_line};
use crate::reclaim::{self, Reclaimed, Resource, Watch};

mod bind;
mod reading;
#[cfg(test)]
mod tests;

pub use self::bind::{
    Bound, DAEMON_RUNNING, SOCKET_UNUSABLE, Unbound, bind, lock_path, raise_descriptor_limit,
};

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

/// How the daemon admits jobs.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The most jobs that run at once.
    pub max_jobs: NonZeroUsize,
    /// The number of logical cores, or `None` when it is unknown.
    pub cores: Option<NonZeroUsize>,
    /// The home directory, whose disk the daemon reads, or `None` when it
    /// is unknown and the disk holds no job.
    pub home: Option<PathBuf>,
    /// The priority file. While it holds an entry, each lane leaves one
    /// place open for the other.
    pub priority_file: Option<PathBuf>,
    /// How often the daemon samples the probe.
    pub sample_every: Duration,
    /// How long the daemon waits for a new connection's first line.
    pub first_line_within: Duration,
    /// The reclaimers and when the daemon asks them.
    pub reclaim: reclaim::Config,
}

impl Settings {
    /// Returns the settings with `max_jobs`, `cores`, `home` and
    /// `priority_file`, the default sampling interval, [`SAMPLE_EVERY`],
    /// the default wait for a first line, [`FIRST_LINE_WITHIN`], and no
    /// reclaimer.
    #[must_use]
    pub fn new(
        max_jobs: NonZeroUsize,
        cores: Option<NonZeroUsize>,
        home: Option<PathBuf>,
        priority_file: Option<PathBuf>,
    ) -> Self {
        Self {
            max_jobs,
            cores,
            home,
            priority_file,
            sample_every: SAMPLE_EVERY,
            first_line_within: FIRST_LINE_WITHIN,
            reclaim: reclaim::Config::default(),
        }
    }
}

/// Returns the default maximum number of jobs: a quarter of the logical
/// cores that `cores` gives, rounded up, and at least 1. With 14 cores it
/// is 4 jobs of 3 threads each.
#[must_use]
pub fn default_max_jobs(cores: Option<NonZeroUsize>) -> NonZeroUsize {
    let quarter = cores.map_or(1, |cores| cores.get().div_ceil(4));
    NonZeroUsize::new(quarter).unwrap_or(NonZeroUsize::MIN)
}

/// A job that waits or runs, with the connection to its client.
#[derive(Debug)]
struct Waiter {
    /// The job as its client described it.
    job: Job,
    /// The connection that the daemon writes the grant to.
    stream: UnixStream,
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
}

impl State {
    /// Starts every waiting job that the admission rule lets start, and
    /// writes the grant to each one's client. A client that cannot take
    /// the grant is gone, so its job leaves the queue and frees its place.
    fn grant(&mut self) {
        let limits = Limits {
            max_jobs: self.settings.max_jobs,
            cores: self.settings.cores,
            reserve_lanes: self.reserve_lanes,
        };
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
                    self.queue.remove(id);
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
    let shared: Shared = Arc::new(Mutex::new(State {
        queue: Queue::default(),
        reading,
        reserve_lanes,
        settings,
        watch: Watch::default(),
        reclaimed: None,
    }));
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
    let id: JobId = {
        let mut state = lock(shared);
        let lane = job.lane;
        let id = state.queue.push(
            Waiter {
                job,
                stream: grant_stream,
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
    while let Ok(Some(request)) = read_line::<Request>(reader) {
        // `Done` carries the job's exit code, which the daemon does not use
        // yet; the ledger records it with the job's cost.
        if let Request::Done { .. } = request {
            break;
        }
    }
    let mut state = lock(shared);
    state.queue.remove(id);
    state.grant();
}
