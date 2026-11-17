//! The lock that `deadband service install` and `uninstall` hold while they
//! read and change the plist and launchd, so two of them never interleave.
//! Without it, an install racing an uninstall can load an agent whose plist
//! the uninstall then deletes.

use std::cell::RefCell;
use std::path::Path;
use std::time::{Duration, Instant};

use crumbkit_durable::{LockError, PathLock};

use super::ServiceFailed;
use crate::daemon::make_state_dir;

/// The name of the lock file in the state directory.
pub(super) const LOCK_FILE: &str = "service.lock";

/// How long a service command looks again for a lock that another one
/// holds, ten seconds, [`PAUSE`](crate::job::launchd::PAUSE) apart. An
/// install that reloads a daemon takes a few seconds; one whose new daemon
/// accepts and never replies holds the lock for up to about nine seconds,
/// and the second command may then refuse.
pub(super) const LOCK_WITHIN: Duration = Duration::from_secs(10);

/// The lock that one service command holds until the value is dropped.
#[derive(Debug)]
pub(super) struct ServiceLock {
    /// The lock, which holds until it drops.
    _lock: PathLock,
}

/// Takes the lock in `state`, the state directory, which it makes with mode
/// 0700 when it does not exist and sets to mode 0700 when it does. The lock
/// is crumbkit-durable's [`PathLock`]. While another command holds the lock
/// it calls `pause` and looks again, until [`LOCK_WITHIN`] has passed on the
/// clock that `now` reads, or on the system clock, whichever comes first.
///
/// # Errors
///
/// Returns [`ServiceFailed::Locked`] when the lock is still held after the
/// last look, and [`ServiceFailed::Lock`] when the directory or the lock
/// file cannot be made or locked.
pub(super) fn hold(
    state: &Path,
    now: impl Fn() -> Instant,
    pause: impl FnMut(),
) -> Result<ServiceLock, ServiceFailed> {
    let path = state.join(LOCK_FILE);
    if let Err(error) = make_state_dir(state) {
        return Err(ServiceFailed::Lock { path, error });
    }
    let deadline = now() + LOCK_WITHIN;
    // PathLock asks `give_up` through a shared reference, and `pause` may
    // change its caller's state.
    let pause = RefCell::new(pause);
    let give_up = || {
        if now() >= deadline {
            return true;
        }
        (pause.borrow_mut())();
        false
    };
    match PathLock::acquire_unless(&path, Instant::now() + LOCK_WITHIN, &give_up) {
        Ok(lock) => Ok(ServiceLock { _lock: lock }),
        Err(LockError::Busy { .. }) => Err(ServiceFailed::Locked { path }),
        Err(LockError::Io { source, .. }) => Err(ServiceFailed::Lock {
            path,
            error: source,
        }),
        Err(error) => Err(ServiceFailed::Lock {
            path,
            error: error.into(),
        }),
    }
}
