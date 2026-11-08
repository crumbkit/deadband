//! The per-user daemon: it listens on the socket, samples the probe, and
//! starts waiting jobs as [`admit`](crate::admit) allows.
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
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use crumbkit::{Code, Failure, Recovery};

use crate::admit::{JobId, Queue};
use crate::probe::{Pressure, Probe};
use crate::protocol::{Job, Jobs, Reply, Request, read_line, write_line};

#[cfg(test)]
mod tests;

/// How often the daemon samples the probe.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(1);

/// How long the daemon waits for a client to take a reply before it counts
/// the client as gone.
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// The code of a refusal to start a daemon while another one answers on
/// the same socket.
pub const DAEMON_RUNNING: Code = Code::new("deadband", "daemon", "running");

/// The code of a socket that the daemon could not listen on.
pub const SOCKET_UNUSABLE: Code = Code::new("deadband", "socket", "unusable");

/// Where the daemon writes its log lines, which several threads share.
pub type Log = Arc<Mutex<dyn Write + Send>>;

/// How the daemon admits jobs.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// The most jobs that run at once.
    pub max_jobs: NonZeroUsize,
    /// How often the daemon samples the probe.
    pub sample_every: Duration,
}

impl Settings {
    /// Returns the settings with `max_jobs` and the default sampling
    /// interval, [`SAMPLE_EVERY`].
    #[must_use]
    pub const fn new(max_jobs: NonZeroUsize) -> Self {
        Self {
            max_jobs,
            sample_every: SAMPLE_EVERY,
        }
    }
}

/// Returns the default maximum number of jobs: half the logical cores that
/// `cores` gives, and at least 1.
#[must_use]
pub fn default_max_jobs(cores: Option<NonZeroUsize>) -> NonZeroUsize {
    let half = cores.map_or(1, |cores| cores.get() / 2);
    NonZeroUsize::new(half).unwrap_or(NonZeroUsize::MIN)
}

/// A daemon that could not start listening.
#[derive(Debug)]
pub enum Unbound {
    /// Another daemon answers on the socket.
    Running {
        /// The socket's path.
        path: PathBuf,
    },
    /// The socket could not be made.
    Socket {
        /// The socket's path.
        path: PathBuf,
        /// The error of the call that failed.
        error: io::Error,
    },
}

impl fmt::Display for Unbound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running { path } => {
                write!(f, "a deadband daemon already listens on {}", path.display())
            }
            Self::Socket { path, error } => write!(
                f,
                "deadband could not listen on {}: {error}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for Unbound {}

impl Failure for Unbound {
    fn code(&self) -> &'static Code {
        match self {
            Self::Running { .. } => &DAEMON_RUNNING,
            Self::Socket { .. } => &SOCKET_UNUSABLE,
        }
    }

    fn recovery(&self) -> Recovery {
        match self {
            Self::Running { .. } => Recovery::None {
                reason: "one daemon serves each socket, and the running one already admits jobs"
                    .to_owned(),
            },
            Self::Socket { .. } => Recovery::None {
                reason: "the socket's directory must exist and be writable; DEADBAND_SOCKET names another path".to_owned(),
            },
        }
    }
}

/// Makes the socket at `path` and listens on it.
///
/// A socket file that no daemon answers on is left from a daemon that
/// ended, so `bind` removes it first. The socket is readable and writable
/// by the daemon's user only.
///
/// # Errors
///
/// Returns [`Unbound::Running`] when a daemon answers on `path`, and
/// [`Unbound::Socket`] when the socket cannot be made.
pub fn bind(path: &Path) -> Result<UnixListener, Unbound> {
    let socket_error = |error| Unbound::Socket {
        path: path.to_owned(),
        error,
    };
    if UnixStream::connect(path).is_ok() {
        return Err(Unbound::Running {
            path: path.to_owned(),
        });
    }
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(socket_error(error)),
        _ => {}
    }
    let listener = UnixListener::bind(path).map_err(socket_error)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(socket_error)?;
    Ok(listener)
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
    /// The latest memory pressure, or `None` when the probe failed.
    pressure: Option<Pressure>,
    /// How the daemon admits jobs.
    settings: Settings,
}

impl State {
    /// Starts every waiting job that the admission rule lets start, and
    /// writes the grant to each one's client. A client that cannot take
    /// the grant is gone, so its job leaves the queue and frees its place.
    fn grant(&mut self) {
        loop {
            let started = self.queue.admit(self.pressure, self.settings.max_jobs);
            if started.is_empty() {
                return;
            }
            for id in started {
                let delivered = self
                    .queue
                    .running_job(id)
                    .is_some_and(|waiter| write_line(&mut &waiter.stream, &Reply::Granted).is_ok());
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
        }
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

/// Serves clients on `listener` until accepting a connection fails, and
/// returns that error. It samples `probe` every `settings.sample_every`.
pub fn serve<P: Probe + Send + 'static>(
    listener: &UnixListener,
    probe: P,
    settings: Settings,
    log: &Log,
) -> io::Error {
    let shared: Shared = Arc::new(Mutex::new(State {
        queue: Queue::default(),
        pressure: read_pressure(&probe, log, None),
        settings,
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
            Err(error) => return error,
        }
    }
}

/// Reads the pressure with `probe`. It writes a log line when the probe
/// starts failing or reads again, judged against `before`, the previous
/// reading.
fn read_pressure(
    probe: &impl Probe,
    log: &Log,
    before: Option<Option<Pressure>>,
) -> Option<Pressure> {
    match probe.sample() {
        Ok(sample) => {
            if before == Some(None) {
                note(log, format_args!("the probe reads the machine again"));
            }
            Some(sample.memory_pressure)
        }
        Err(error) => {
            if before.is_none_or(|before| before.is_some()) {
                note(
                    log,
                    format_args!("{error}, so every job starts at once until it reads again"),
                );
            }
            None
        }
    }
}

/// Samples `probe` every `sample_every` and starts the jobs that the new
/// pressure admits, until `stopped` receives or disconnects.
///
/// The probe has no event to wait on, so the sampler polls it. The wait
/// is a timed receive on the daemon's stop channel.
fn sample(shared: &Shared, probe: &impl Probe, stopped: &mpsc::Receiver<()>, log: &Log) {
    let every = lock(shared).settings.sample_every;
    while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
        let before = lock(shared).pressure;
        let pressure = read_pressure(probe, log, Some(before));
        let mut state = lock(shared);
        state.pressure = pressure;
        state.grant();
    }
}

/// Serves one client on `stream`.
fn connection(shared: &Shared, stream: UnixStream) {
    if stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err() {
        return;
    }
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    match read_line::<Request>(&mut reader) {
        Ok(Some(Request::Ask(job))) => ask(shared, stream, &mut reader, job),
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
        let id = state.queue.push(Waiter {
            job,
            stream: grant_stream,
        });
        state.grant();
        if let Some(ahead) = state.queue.ahead_of(id) {
            // A client that cannot take this reply also closes its
            // connection, which the read below sees.
            let _ = write_line(&mut &stream, &Reply::Queued { ahead });
        }
        id
    };
    while let Ok(Some(request)) = read_line::<Request>(reader) {
        if let Request::Done { .. } = request {
            break;
        }
    }
    let mut state = lock(shared);
    state.queue.remove(id);
    state.grant();
}
