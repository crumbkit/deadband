//! The per-user daemon: it listens on the socket, samples the probe and the
//! disk that holds the home directory, and starts waiting jobs as
//! [`admit`](crate::admit) allows.
//!
//! The daemon runs one thread that samples the probe every
//! [`SAMPLE_EVERY`], one thread per connection, and one thread in
//! the `watch` module that flags jobs with no activity. Each thread that changes
//! the queue or the pressure starts every job that the admission rule then
//! lets start, under the one lock that guards the queue, and writes
//! [`Reply::Granted`](crate::protocol::Reply::Granted) to that job's client
//! after it releases the lock, as the `reply` module describes. The daemon
//! never starts, stops or signals a job; each client runs its own job.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::{self, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use self::alive::{Ending, age, passing, unstarted, wait_for_connection};
use self::reading::{priority_active, read_machine};
use self::reply::deliver;
use self::timeline::Ended;
use crate::admit::{Hold, JobId, Limits, Queue, Reading, disk_critical};
use crate::history::{Arrival, Recorder, Tally};
use crate::probe::{Pressure, Probe};
use crate::protocol::{Job, Label};
use crate::reclaim::{self, ReclaimLock, Reclaimed, Resource, Watch};
use crate::spawn;

mod alive;
mod bind;
mod reading;
mod reply;
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
/// the client as gone. The daemon writes every reply with the queue lock
/// released, so this wait never holds the lock.
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
    /// The connection that the daemon writes the grant to, which a
    /// delivery shares after the queue lock is released.
    stream: Arc<UnixStream>,
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
/// that another try cannot fix, the socket file at the listener's path or
/// its lock file is gone or replaced, the sampler panics, or the operating
/// system refuses the sampler's, the watchdog's or a connection's thread,
/// and returns the reason as an error. It samples `probe` every
/// `settings.sample_every`, and checks the socket file and the lock file
/// once every 60 samples. The caller ends its process then, so
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
    let watchdog_ending = Ending::new(listener, reason.clone());
    let ending = Ending::new(listener, reason);
    let sampling = spawn::thread("deadband-sampler", move || {
        sample(&sampler, &probe, &stopped, &sampler_log, ending);
    });
    if let Err(error) = sampling {
        return unstarted("sampler", &error);
    }
    let (_aging, aged) = mpsc::channel::<()>();
    let watchdog = Arc::clone(&shared);
    let watchdog_log = Arc::clone(log);
    let aging = spawn::thread("deadband-age", move || {
        age(&watchdog, &aged, &watchdog_log, watchdog_ending);
    });
    if let Err(error) = aging {
        return unstarted("watchdog", &error);
    }
    let every = lock(&shared).settings.sample_every;
    if let Err(error) = listener.set_nonblocking(true) {
        return error;
    }
    loop {
        if let Ok(reason) = ends.try_recv() {
            return io::Error::other(reason);
        }
        if !wait_for_connection(listener, every) {
            continue;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                let shared = Arc::clone(&shared);
                // A refused thread drops `stream`, so its client sees the
                // daemon close and starts its job.
                let serving = spawn::thread("deadband-connection", move || {
                    seat::connection(&shared, stream);
                });
                if let Err(error) = serving {
                    return unstarted("connection", &error);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
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
        let replies = state.grant();
        state.write_machine(Instant::now());
        let reclaim = state.reclaim_now().map(|resource| {
            let config = state.settings.reclaim.clone();
            let lock_directory = state.settings.history.clone();
            (
                config,
                lock_directory,
                resource,
                state.watch.quiet(resource),
            )
        });
        drop(state);
        deliver(shared, replies);
        if let Some((config, lock_directory, resource, quiet)) = reclaim {
            start_reclaim(shared, config, lock_directory, resource, quiet, log);
        }
    }
}

/// Runs the reclaimers of `config` for `resource` on a thread of their
/// own, so admission never waits for them, and records each outcome for
/// `deadband status`. `quiet` says the last reclaim of `resource` freed
/// nothing. The reclaim holds the lock in `lock_directory` (the state
/// directory) while it runs; a daemon that finds it held, or that cannot
/// start the thread, logs that and starts no reclaim now.
fn start_reclaim(
    shared: &Shared,
    config: reclaim::Config,
    lock_directory: Option<PathBuf>,
    resource: Resource,
    quiet: bool,
    log: &Log,
) {
    let reclaiming = Arc::clone(shared);
    let reclaim_log = Arc::clone(log);
    let started = spawn::thread("deadband-reclaim", move || {
        let (shared, log) = (reclaiming, reclaim_log);
        let Some(_lock) = ReclaimLock::take(lock_directory.as_deref()) else {
            note(
                &log,
                format_args!("another daemon is reclaiming, so this daemon starts no reclaim now"),
            );
            lock(&shared).watch.skipped(Instant::now());
            return;
        };
        let freed = reclaim::reclaim(&config, resource, quiet, &log, &mut |reclaimed| {
            lock(&shared).reclaimed = Some(reclaimed);
        });
        lock(&shared).watch.ended(freed, Instant::now());
    });
    if let Err(error) = started {
        note(
            log,
            format_args!(
                "the reclaim thread could not start: {error}, so this daemon starts no reclaim now"
            ),
        );
        lock(shared).watch.skipped(Instant::now());
    }
}
