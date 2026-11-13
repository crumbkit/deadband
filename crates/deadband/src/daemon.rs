//! The per-user daemon: it listens on the socket, samples the probe and the
//! disk that holds the home directory, and starts waiting jobs as
//! [`admit`](crate::admit) allows.
//!
//! The daemon runs one thread that samples the probe every
//! [`SAMPLE_EVERY`], one thread per connection, and one thread in
//! the `watch` module that flags jobs with no activity. Each thread that changes
//! the queue or the pressure starts every job that the admission rule then
//! lets start, under the one lock that guards the queue, and writes
//! [`Reply::Granted`] to that job's client. The daemon never starts, stops
//! or signals a job; each client runs its own job.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::{self, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use rustix::io::Errno;

use self::alive::{Ending, stale_after, wait_for_connection};
use self::reading::{priority_active, read_machine};
use self::timeline::Ended;
use crate::admit::{Hold, JobId, Limits, Queue, Reading, disk_critical};
use crate::history::{Arrival, Recorder, Tally};
use crate::probe::{Pressure, Probe};
use crate::protocol::{Job, Label, Reply, Request, read_line, write_line};
use crate::reclaim::{self, Reclaimed, Resource, Watch};

mod alive;
mod bind;
mod reading;
mod seat;
mod settings;
#[cfg(test)]
mod tests;
mod timeline;
mod watch;

pub(crate) use self::bind::take_lock;
pub use self::bind::{
    Bound, DAEMON_RUNNING, SOCKET_UNUSABLE, Unbound, bind, lock_path, raise_descriptor_limit,
};
pub use self::settings::{Settings, Watching, default_max_jobs};

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
    /// What the history line needs from the job's arrival, or `None` when
    /// the daemon records no job.
    arrival: Option<Arrival>,
    /// How many jobs were ahead of it when it asked.
    ahead: usize,
}

/// What the threads of the daemon share.
struct State {
    /// The waiting and running jobs.
    queue: Queue<Waiter>,
    /// The latest reading of the machine, or `None` when the probe failed.
    reading: Option<Reading>,
    /// When the sampler stored `reading`.
    read_at: Instant,
    /// Whether the log has said that the reading went stale.
    stale_noted: bool,
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
    /// What the daemon knows of the watched jobs.
    stuck: watch::Watched,
    /// The handle on the writer of the history file, or `None` when the
    /// daemon records no job.
    recorder: Option<Recorder>,
    /// The machine's figures across each running job's run.
    tallies: HashMap<JobId, Tally>,
    /// When the daemon last wrote a machine line.
    machine_at: Instant,
}

impl State {
    /// Returns the state of a daemon that starts now and has no job.
    fn new(settings: Settings, reading: Option<Reading>, reserve_lanes: bool) -> Self {
        let now = Instant::now();
        Self {
            queue: Queue::default(),
            reading,
            read_at: now,
            stale_noted: false,
            reserve_lanes,
            settings,
            watch: Watch::default(),
            reclaimed: None,
            up: now,
            ended: VecDeque::new(),
            stuck: watch::Watched::default(),
            recorder: None,
            tallies: HashMap::new(),
            machine_at: now,
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

    /// Reports whether the sampler stored its reading longer ago than
    /// [`stale_after`] allows at `now`.
    fn stale(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.read_at) > stale_after(self.settings.sample_every)
    }

    /// Returns the reading that admission may use at `now`. A stale reading
    /// counts as a failed probe, which admits a job once a place is free.
    fn fresh_reading(&self, now: Instant) -> Option<&Reading> {
        self.reading.as_ref().filter(|_| !self.stale(now))
    }

    /// Starts every waiting job that the admission rule lets start, and
    /// writes the grant to each one's client. A client that cannot take
    /// the grant is gone, so its job leaves the queue and frees its place.
    fn grant(&mut self) {
        let limits = self.limits();
        loop {
            let now = Instant::now();
            let reading = self.fresh_reading(now).copied();
            let started = self.queue.admit(reading.as_ref(), &limits, now);
            if started.is_empty() {
                return;
            }
            for (id, jobs) in started {
                self.count_started(id);
                let granted = Reply::Granted { jobs };
                let delivered = self
                    .queue
                    .running_job(id)
                    .is_some_and(|waiter| write_line(&mut &waiter.stream, &granted).is_ok());
                if !delivered {
                    self.record(id, None, None);
                }
            }
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
/// that another try cannot fix, the socket file at the listener's path is
/// gone or replaced, or the sampler panics, and returns the reason as an
/// error. It samples `probe` every `settings.sample_every`, and checks the
/// socket file once every 60 samples. The caller ends its process then, so
/// that launchd starts a new daemon and the clients start their jobs at once.
///
/// A failed accept that can pass, such as running out of descriptors or a
/// client that gave up while it connected, is logged, and the daemon
/// accepts again after 100 ms.
pub fn serve<P: Probe + Clone + Send + 'static>(
    listener: &UnixListener,
    probe: P,
    settings: Settings,
    log: &Log,
) -> io::Error {
    let reading = read_machine(&probe, &settings, log, None);
    let reserve_lanes = priority_active(settings.priority_file.as_deref());
    let recorder = settings
        .history
        .clone()
        .and_then(|directory| Recorder::start(directory, log));
    let mut state = State::new(settings, reading, reserve_lanes);
    state.recorder = recorder;
    let shared: Shared = Arc::new(Mutex::new(state));
    // The sampler stops when `serve` returns and drops `_stop`.
    let (_stop, stopped) = mpsc::channel::<()>();
    let sampler = Arc::clone(&shared);
    let sampler_log = Arc::clone(log);
    let (_watching, watch_stopped) = mpsc::channel::<()>();
    watch::start(&shared, probe.clone(), watch_stopped, log);
    let (reason, ends) = mpsc::channel::<String>();
    let ending = Ending::new(listener, reason);
    thread::spawn(move || sample(&sampler, &probe, &stopped, &sampler_log, ending));
    let (_aging, aged) = mpsc::channel::<()>();
    let watchdog = Arc::clone(&shared);
    let watchdog_log = Arc::clone(log);
    thread::spawn(move || age(&watchdog, &aged, &watchdog_log));
    let every = lock(&shared).settings.sample_every;
    loop {
        if let Ok(reason) = ends.try_recv() {
            return io::Error::other(reason);
        }
        if !wait_for_connection(listener, every) {
            continue;
        }
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
/// reading admits, until `stopped` receives or disconnects or `ending`
/// says the daemon should stop. A panic here drops `ending`, which tells
/// the accept loop to stop.
///
/// The probe has no event to wait on, so the sampler polls it. The wait
/// is a timed receive on the daemon's stop channel.
fn sample(
    shared: &Shared,
    probe: &impl Probe,
    stopped: &mpsc::Receiver<()>,
    log: &Log,
    mut ending: Ending,
) {
    let every = lock(shared).settings.sample_every;
    while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
        if ending.tick() {
            return;
        }
        let (before, settings) = {
            let state = lock(shared);
            (state.reading, state.settings.clone())
        };
        let reading = read_machine(probe, &settings, log, Some(before));
        let reserve_lanes = priority_active(settings.priority_file.as_deref());
        let mut state = lock(shared);
        state.reading = reading;
        state.read_at = Instant::now();
        state.stale_noted = false;
        state.reserve_lanes = reserve_lanes;
        state.observe();
        state.grant();
        state.write_machine(Instant::now());
        if let Some(resource) = state.reclaim_now() {
            let config = state.settings.reclaim.clone();
            let quiet = state.watch.quiet(resource);
            drop(state);
            start_reclaim(shared, config, resource, quiet, log);
        }
    }
}

/// Starts the waiting jobs that a stale reading lets start, until `stopped`
/// receives or disconnects. The sampler does this on every sample while it
/// runs; this thread does it when the sampler stalls, and writes one log
/// line when the reading first goes stale.
fn age(shared: &Shared, stopped: &mpsc::Receiver<()>, log: &Log) {
    let every = stale_after(lock(shared).settings.sample_every);
    while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
        let mut state = lock(shared);
        if !state.stale(Instant::now()) {
            continue;
        }
        if !state.stale_noted {
            state.stale_noted = true;
            note(
                log,
                format_args!(
                    "the probe has not read the machine for over {}, so each job starts once a place is free until it reads again",
                    humantime::format_duration(every)
                ),
            );
        }
        state.grant();
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
                seat::ask(shared, stream, &mut reader, job);
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
        Ok(Some(Request::Seat { job, jobs, ran_ms })) => {
            if stream.set_read_timeout(None).is_ok() {
                seat::seat(shared, stream, &mut reader, job, jobs, ran_ms);
            }
        }
        Ok(Some(Request::Done { .. }) | None) | Err(_) => {}
    }
}
