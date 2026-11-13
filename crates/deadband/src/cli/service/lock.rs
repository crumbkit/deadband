//! The lock that `deadband service install` and `uninstall` hold while they
//! read and change the plist and launchd, so two of them never interleave.
//! Without it, an install racing an uninstall can load an agent whose plist
//! the uninstall then deletes.

use std::fs::{DirBuilder, File, TryLockError};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;

use super::ServiceFailed;
use crate::daemon::take_lock;

/// The name of the lock file in the state directory.
pub(super) const LOCK_FILE: &str = "service.lock";

/// The most times a service command looks again for a lock that another one
/// holds, [`PAUSE`](crate::job::launchd::PAUSE) apart, about ten seconds in
/// all. An install that reloads a daemon takes a few seconds; one whose new
/// daemon accepts and never replies can hold the lock for up to 47 seconds,
/// and the second command then refuses.
pub(super) const LOCK_POLLS: u32 = 100;

/// The mode of the state directory when the lock makes it: only its owner
/// may enter it, as the history writer makes it.
const DIRECTORY_MODE: u32 = 0o700;

/// The lock that one service command holds until the value is dropped.
#[derive(Debug)]
pub(super) struct ServiceLock {
    /// The open lock file, which holds the lock until it is closed.
    _file: File,
}

/// Takes the lock in `state`, the state directory, which it makes when it
/// does not exist. While another command holds the lock it calls `pause`
/// and looks again, up to [`LOCK_POLLS`] times.
///
/// # Errors
///
/// Returns [`ServiceFailed::Locked`] when the lock is still held after the
/// last look, and [`ServiceFailed::Lock`] when the directory or the lock
/// file cannot be made or locked.
pub(super) fn hold(state: &Path, mut pause: impl FnMut()) -> Result<ServiceLock, ServiceFailed> {
    let path = state.join(LOCK_FILE);
    if let Err(error) = DirBuilder::new()
        .recursive(true)
        .mode(DIRECTORY_MODE)
        .create(state)
    {
        return Err(ServiceFailed::Lock { path, error });
    }
    let mut polls = 0;
    loop {
        match take_lock(&path) {
            Ok(file) => return Ok(ServiceLock { _file: file }),
            Err(TryLockError::WouldBlock) if polls < LOCK_POLLS => {
                polls += 1;
                pause();
            }
            Err(TryLockError::WouldBlock) => return Err(ServiceFailed::Locked { path }),
            Err(TryLockError::Error(error)) => return Err(ServiceFailed::Lock { path, error }),
        }
    }
}
