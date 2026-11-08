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
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use rustix::io::Errno;

use crate::admit::{JobId, Queue};
use crate::probe::{Pressure, Probe};
use crate::protocol::{Job, Jobs, Reply, Request, read_line, write_line};

mod bind;
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
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// The most jobs that run at once.
    pub max_jobs: NonZeroUsize,
    /// How often the daemon samples the probe.
    pub sample_every: Duration,
    /// How long the daemon waits for a new connection's first line.
    pub first_line_within: Duration,
}

impl Settings {
    /// Returns the settings with `max_jobs`, the default sampling
    /// interval, [`SAMPLE_EVERY`], and the default wait for a first line,
    /// [`FIRST_LINE_WITHIN`].
    #[must_use]
    pub const fn new(max_jobs: NonZeroUsize) -> Self {
        Self {
            max_jobs,
            sample_every: SAMPLE_EVERY,
            first_line_within: FIRST_LINE_WITHIN,
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
