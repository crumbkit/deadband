//! The lock that keeps reclaims to one at a time for a user, whichever
//! daemons are up.

use std::fs::{File, TryLockError};
use std::path::Path;

use crate::daemon::take_lock;

/// The name of the lock file in the state directory that one reclaim holds.
const LOCK_FILE: &str = "reclaim.lock";

/// The lock that one reclaim holds while it runs. It holds until the value
/// is dropped.
#[derive(Debug)]
pub struct ReclaimLock {
    /// The open lock file, which holds the lock until it is closed.
    _file: Option<File>,
}

impl ReclaimLock {
    /// Takes the lock in `directory`, which is the state directory, without
    /// waiting, and returns `None` when another reclaim holds it. With no
    /// directory, or a lock file that cannot be made or locked, the lock
    /// holds nothing and the reclaim runs without one.
    pub fn take(directory: Option<&Path>) -> Option<Self> {
        let free = Some(Self { _file: None });
        let Some(directory) = directory else {
            return free;
        };
        if std::fs::create_dir_all(directory).is_err() {
            return free;
        }
        match take_lock(&directory.join(LOCK_FILE)) {
            Ok(file) => Some(Self { _file: Some(file) }),
            Err(TryLockError::WouldBlock) => None,
            Err(TryLockError::Error(_)) => free,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ReclaimLock;

    #[test]
    fn a_second_reclaim_finds_the_lock_held_until_the_first_ends() {
        let directory = std::env::temp_dir().join(format!("db-rl-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("the directory is made");
        let first = ReclaimLock::take(Some(&directory)).expect("the first reclaim takes the lock");
        assert!(ReclaimLock::take(Some(&directory)).is_none());
        drop(first);
        assert!(ReclaimLock::take(Some(&directory)).is_some());
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_reclaim_with_no_state_directory_runs_without_a_lock() {
        assert!(ReclaimLock::take(None).is_some());
    }
}
