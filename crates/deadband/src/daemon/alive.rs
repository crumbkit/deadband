//! Tells the daemon when it should stop serving, and when its last reading
//! of the machine is too old to trust.
//!
//! A daemon whose socket file is gone or replaced serves nobody, so it
//! stops and launchd starts a new one. A daemon whose lock file is gone or
//! replaced no longer keeps a second daemon off its socket, so it stops for
//! the same reason. A daemon whose sampler panicked, or that the operating
//! system refuses a thread it needs, grants nobody, so it stops too. A
//! client whose daemon stops starts its job at once. The watchdog here also
//! beats to the waiting clients that asked for beats.
//!
//! launchd appends the daemon's standard error to a log file that nothing
//! else limits, so at start and at each alive check the daemon empties
//! that file once it has passed [`LOG_LIMIT`].

use std::fmt;
use std::fs::File;
use std::io;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl};
use rustix::io::Errno;

use super::reply::{Replies, deliver};
use super::{Log, Shared, State, lock, lock_path, note};
use crate::admit::Reading;
use crate::file_id::FileId;
use crate::reclaim::ShownAge;

impl State {
    /// Reports whether the sampler stored its reading longer ago than
    /// [`stale_after`] allows at `now`.
    pub(super) fn stale(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.read_at) > stale_after(self.settings.sample_every)
    }

    /// Returns the reading that admission may use at `now`. A stale reading
    /// counts as a failed probe, which admits a job once a place is free.
    pub(super) fn fresh_reading(&self, now: Instant) -> Option<&Reading> {
        self.reading.as_ref().filter(|_| !self.stale(now))
    }
}

/// Runs while the daemon serves, until `stopped` receives or disconnects.
/// Once per [`stale_after`] it sends a beat to each waiting job whose
/// client asked for beats, so a client stops hearing them when the daemon
/// stops, or when a thread blocks or deadlocks while it holds the queue
/// lock. Each time the reading is stale it also starts the waiting jobs
/// that a stale reading lets start, checks the socket file with `ending`,
/// and writes one log line when the reading first goes stale. It also
/// writes the line that the history thread has stopped, once a line found
/// it stopped.
///
/// Only the decisions and the grants happen under the queue lock. The file
/// checks and the log lines come after the lock is released, so a log that
/// blocks, such as a stopped terminal, never holds the queue up.
pub(super) fn age(shared: &Shared, stopped: &Receiver<()>, log: &Log, ending: Ending) {
    let (every, recorder) = {
        let state = lock(shared);
        (
            stale_after(state.settings.sample_every),
            state.recorder.clone(),
        )
    };
    while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
        let mut state = lock(shared);
        let mut replies = Replies::default();
        let stale = state.stale(Instant::now());
        let first_stale = stale && !state.stale_noted;
        if stale {
            state.stale_noted = true;
            replies = state.grant();
        }
        state.beat(&mut replies);
        drop(state);
        deliver(shared, replies);
        if stale && ending.check() {
            return;
        }
        if first_stale {
            note(
                log,
                format_args!(
                    "the probe has not read the machine for over {}, so each job starts once a place is free until it reads again",
                    ShownAge(every)
                ),
            );
        }
        if let Some(recorder) = &recorder {
            recorder.say_stopped();
        }
    }
}

/// How many samples pass between two checks of the socket file, which is
/// once a minute at the default sampling interval.
const CHECK_EVERY_SAMPLES: u32 = 60;

/// How many sampling intervals old a reading may be before the admission
/// rule counts the probe as failed.
const STALE_AFTER_SAMPLES: u32 = 5;

/// The least time a reading may be old before it counts as stale, so a
/// short sampling interval does not make a busy machine's brief pause
/// look like a stalled sampler.
const STALE_FLOOR: Duration = Duration::from_millis(250);

/// Returns how old a reading may be before the admission rule counts the
/// probe as failed, for a sampler that samples every `every`.
pub(super) fn stale_after(every: Duration) -> Duration {
    every.saturating_mul(STALE_AFTER_SAMPLES).max(STALE_FLOOR)
}

/// Returns why the daemon stops when the operating system refused to start
/// its `what` thread, which `serve` returns as its error.
pub(super) fn unstarted(what: &str, error: &io::Error) -> io::Error {
    io::Error::other(format!(
        "the {what} thread could not start: {error}, so the daemon stops serving"
    ))
}

/// Reports whether a failed accept can pass, so another try may succeed.
pub(super) fn passing(error: &std::io::Error) -> bool {
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

/// Waits until `listener` has a connection to accept or `within` passes,
/// and reports whether accepting can proceed. A wait that fails reports
/// `true`, so the caller accepts and meets the failure there.
pub(super) fn wait_for_connection(listener: &UnixListener, within: Duration) -> bool {
    let timeout = Timespec {
        tv_sec: i64::try_from(within.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(within.subsec_nanos()),
    };
    let mut fds = [PollFd::new(listener, PollFlags::IN)];
    poll(&mut fds, Some(&timeout)).map_or(true, |ready| ready > 0)
}

/// A file that the daemon depends on, its socket or its lock file, named by
/// its device and inode.
#[derive(Debug)]
struct HeldFile {
    /// The file's path.
    path: PathBuf,
    /// The file's identity when the daemon named it.
    id: FileId,
}

impl HeldFile {
    /// Names the file that `listener` is bound to, or returns `None` when
    /// the socket has no path or its file cannot be read.
    fn of(listener: &UnixListener) -> Option<Self> {
        Self::at(listener.local_addr().ok()?.as_pathname()?.to_owned())
    }

    /// Names the file at `path` now, or returns `None` when it cannot be
    /// read.
    fn at(path: PathBuf) -> Option<Self> {
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        Some(Self {
            path,
            id: FileId::from(&metadata),
        })
    }

    /// Reports whether the path no longer names this file. A file that
    /// cannot be read for another reason counts as still there.
    fn gone(&self) -> bool {
        match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => !self.id.is(&metadata),
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        }
    }
}

/// What the sampler tells the accept loop when the daemon should stop. The
/// loop stops and `serve` returns the reason.
#[derive(Debug)]
pub(super) struct Ending {
    /// The daemon's socket file, or `None` when it has no path to check.
    socket: Option<HeldFile>,
    /// The lock file next to the socket that keeps a second daemon off it,
    /// or `None` when the daemon took no lock.
    lock: Option<HeldFile>,
    /// The samples since the last check of the socket file.
    since_check: u32,
    /// The daemon's standard error, which the alive check empties past
    /// [`LOG_LIMIT`], or `None` when this ending checks no log file.
    log_file: Option<LogFile>,
    /// Where the reason for stopping goes.
    reason: Sender<String>,
}

impl Ending {
    /// Returns the ending for the daemon that serves `listener`. It names
    /// the lock file by a stat of the lock path when `serve` builds its
    /// endings, not from the file the daemon locked. Soft limit: a lock file
    /// replaced between `bind` and that stat is recorded as the daemon's own,
    /// and the daemon never notices the replacement.
    pub(super) fn new(listener: &UnixListener, reason: Sender<String>) -> Self {
        let socket = HeldFile::of(listener);
        let lock = socket
            .as_ref()
            .and_then(|socket| HeldFile::at(lock_path(&socket.path)));
        Self {
            socket,
            lock,
            since_check: 0,
            log_file: None,
            reason,
        }
    }

    /// Makes this ending check the daemon's standard error at each alive
    /// check, and checks it once now, at the daemon's start. `log` says
    /// when the check empties the file.
    pub(super) fn trimming(mut self, log: &Log) -> Self {
        self.log_file = standard_error().map(|file| LogFile {
            file,
            log: Arc::clone(log),
        });
        if let Some(log_file) = &self.log_file {
            trim(&log_file.file, LOG_LIMIT, &log_file.log);
        }
        self
    }

    /// Counts one sample, and once a minute empties the log file when it
    /// has passed [`LOG_LIMIT`] and checks the socket file and the lock
    /// file. It sends the reason and returns `true` when the daemon should
    /// stop.
    pub(super) fn tick(&mut self) -> bool {
        self.since_check += 1;
        if self.since_check < CHECK_EVERY_SAMPLES {
            return false;
        }
        self.since_check = 0;
        if let Some(log_file) = &self.log_file {
            trim(&log_file.file, LOG_LIMIT, &log_file.log);
        }
        self.check()
    }

    /// Checks the socket file and the lock file now. It sends the reason and
    /// returns `true` when either is gone or replaced. A daemon whose lock
    /// file is gone stops because a second daemon could take a new lock,
    /// remove this daemon's socket and bind its own.
    pub(super) fn check(&self) -> bool {
        let gone = if self.socket.as_ref().is_some_and(HeldFile::gone) {
            "socket file"
        } else if self.lock.as_ref().is_some_and(HeldFile::gone) {
            "lock file"
        } else {
            return false;
        };
        let _ = self.reason.send(format!(
            "the {gone} is gone or replaced, so the daemon stops serving"
        ));
        true
    }
}

/// The size past which the daemon empties its standard error file: 8 MiB.
pub(super) const LOG_LIMIT: u64 = 8 * 1024 * 1024;

/// The daemon's standard error, which launchd appends to
/// `~/Library/Logs/deadband.log`, and the log that says when it is emptied.
struct LogFile {
    /// A duplicate of the standard error descriptor.
    file: File,
    /// Where the line that says the file was emptied goes.
    log: Log,
}

impl fmt::Debug for LogFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LogFile")
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

/// Empties `file` when it is a regular file opened for appending and longer
/// than `limit` bytes, and logs one line saying so. Because the file is
/// opened for appending, the next write lands at the new end, so the
/// writer that opened it, such as launchd, needs no signal. A terminal, a
/// pipe or a file opened without appending is left alone. A file that
/// cannot be emptied is logged at each check, at most once a minute.
pub(super) fn trim(file: &File, limit: u64, log: &Log) {
    let Ok(metadata) = file.metadata() else {
        return;
    };
    let appending = fcntl_getfl(file).is_ok_and(|flags| flags.contains(OFlags::APPEND));
    if !metadata.is_file() || !appending || metadata.len() <= limit {
        return;
    }
    match file.set_len(0) {
        Ok(()) => note(
            log,
            format_args!("the log file had grown past {limit} bytes, so the daemon emptied it"),
        ),
        Err(error) => note(
            log,
            format_args!(
                "the log file has grown past {limit} bytes and the daemon could not empty it: {error}"
            ),
        ),
    }
}

/// Returns a duplicate of the process's standard error, or `None` when it
/// cannot be duplicated.
#[cfg(not(test))]
fn standard_error() -> Option<File> {
    use std::os::fd::AsFd as _;
    #[expect(
        clippy::disallowed_methods,
        reason = "the daemon duplicates its standard error to check the size of the log file launchd appends it to, and writes nothing through it"
    )]
    let stderr = io::stderr();
    stderr.as_fd().try_clone_to_owned().ok().map(File::from)
}

#[cfg(test)]
thread_local! {
    /// The file that a test gives the daemon as its standard error.
    static STANDARD_ERROR: std::cell::RefCell<Option<File>> = const { std::cell::RefCell::new(None) };
}

/// In the tests, the daemon's standard error is the file that the test on
/// this thread gave it, so no test empties the test process's own.
#[cfg(test)]
fn standard_error() -> Option<File> {
    STANDARD_ERROR.with(|file| file.borrow_mut().take())
}

/// Makes `file` the standard error of the daemon that the calling thread
/// serves next.
#[cfg(test)]
pub(super) fn standard_error_as(file: File) {
    STANDARD_ERROR.with(|standard| *standard.borrow_mut() = Some(file));
}

impl Drop for Ending {
    /// Sends the reason when the sampler panics, so the daemon stops and
    /// its clients start their jobs at once.
    fn drop(&mut self) {
        if thread::panicking() {
            let _ = self
                .reason
                .send("the sampler panicked, so the daemon stops serving".to_owned());
        }
    }
}
