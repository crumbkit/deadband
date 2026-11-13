//! Tells the daemon when it should stop serving, and when its last reading
//! of the machine is too old to trust.
//!
//! A daemon whose socket file is gone or replaced serves nobody, so it
//! stops and launchd starts a new one. A daemon whose sampler panicked
//! grants nobody, so it stops for the same reason. A client whose daemon
//! stops starts its job at once.

use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, Timespec, poll};

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

/// The socket file that a daemon bound, named by its device and inode.
#[derive(Debug)]
struct SocketFile {
    /// Where the daemon bound the socket.
    path: PathBuf,
    /// The device that holds the file.
    device: u64,
    /// The file's inode.
    inode: u64,
}

impl SocketFile {
    /// Names the file that `listener` is bound to, or returns `None` when
    /// the socket has no path or its file cannot be read.
    fn of(listener: &UnixListener) -> Option<Self> {
        let path = listener.local_addr().ok()?.as_pathname()?.to_owned();
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
    socket: Option<SocketFile>,
    /// The samples since the last check of the socket file.
    since_check: u32,
    /// Where the reason for stopping goes.
    reason: Sender<String>,
}

impl Ending {
    /// Returns the ending for the daemon that serves `listener`.
    pub(super) fn new(listener: &UnixListener, reason: Sender<String>) -> Self {
        Self {
            socket: SocketFile::of(listener),
            since_check: 0,
            reason,
        }
    }

    /// Counts one sample, and once a minute checks the socket file. It
    /// sends the reason and returns `true` when the daemon should stop.
    pub(super) fn tick(&mut self) -> bool {
        self.since_check += 1;
        if self.since_check < CHECK_EVERY_SAMPLES {
            return false;
        }
        self.since_check = 0;
        let Some(socket) = self.socket.as_ref().filter(|socket| socket.gone()) else {
            return false;
        };
        let _ = self.reason.send(format!(
            "the socket file {} is gone or replaced, so the daemon stops serving",
            socket.path.display()
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
