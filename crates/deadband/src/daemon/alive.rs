//! Tells the daemon when it should stop serving, and when its last reading
//! of the machine is too old to trust.
//!
//! A daemon whose socket file is gone or replaced serves nobody, so it
//! stops and launchd starts a new one. A daemon whose lock file is gone or
//! replaced no longer keeps a second daemon off its socket, so it stops for
//! the same reason. A daemon whose sampler panicked, or that the operating
//! system refuses a thread it needs, grants nobody, so it stops too. A
//! client whose daemon stops starts its job at once.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;

use super::reply::deliver;
use super::{Log, Shared, State, lock, lock_path, note};
use crate::admit::Reading;

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

/// Runs while the sampler may be stalled, until `stopped` receives or
/// disconnects. Each time the reading is stale it checks the socket file
/// with `ending`, starts the waiting jobs that a stale reading lets start,
/// and writes one log line when the reading first goes stale.
pub(super) fn age(shared: &Shared, stopped: &Receiver<()>, log: &Log, ending: Ending) {
    let every = stale_after(lock(shared).settings.sample_every);
    while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
        let mut state = lock(shared);
        if !state.stale(Instant::now()) {
            continue;
        }
        if ending.check() {
            return;
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
        let replies = state.grant();
        drop(state);
        deliver(shared, replies);
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
    /// The device that holds the file.
    device: u64,
    /// The file's inode.
    inode: u64,
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
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    /// Reports whether the path no longer names this file. A file that
    /// cannot be read for another reason counts as still there.
    fn gone(&self) -> bool {
        match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata.dev() != self.device || metadata.ino() != self.inode,
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
            reason,
        }
    }

    /// Counts one sample, and once a minute checks the socket file and the
    /// lock file. It sends the reason and returns `true` when the daemon
    /// should stop.
    pub(super) fn tick(&mut self) -> bool {
        self.since_check += 1;
        if self.since_check < CHECK_EVERY_SAMPLES {
            return false;
        }
        self.since_check = 0;
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
