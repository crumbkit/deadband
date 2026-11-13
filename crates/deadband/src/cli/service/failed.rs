//! Why a `deadband service` command changed nothing, or stopped part way.

use std::fmt;
use std::io;
use std::path::PathBuf;

use crumbkit::{Code, Failure};

use crate::job::launchd::Failed;

/// The code of a `service` command on a platform that has no launchd.
pub const SERVICE_UNSUPPORTED: Code = Code::new("deadband", "service", "unsupported");

/// The code of a `service` command that found no home directory or no path
/// to its own binary.
pub const SERVICE_UNLOCATED: Code = Code::new("deadband", "service", "unlocated");

/// The code of an install that found a file it cannot read as a plist.
pub const SERVICE_PLIST_UNREADABLE: Code = Code::new("deadband", "service", "plist_unreadable");

/// The code of a `service` command that could not write or delete the plist,
/// or make or lock its lock file.
pub const SERVICE_WRITE_FAILED: Code = Code::new("deadband", "service", "write_failed");

/// The code of a `service` command whose launchctl or getconf run failed.
pub const SERVICE_LAUNCHCTL_FAILED: Code = Code::new("deadband", "service", "launchctl_failed");

/// The code of a `service` command that waited for another one to finish
/// and gave up.
pub const SERVICE_LOCKED: Code = Code::new("deadband", "service", "locked");

/// Why a `service` command changed nothing, or stopped part way.
#[derive(Debug, Failure)]
pub enum ServiceFailed {
    /// The machine has no launchd.
    #[failure(
        code = SERVICE_UNSUPPORTED,
        no_next_step = "deadband installs its service only on macOS for now; run deadband daemon under the platform's own service manager"
    )]
    Unsupported,
    /// The environment names no home directory, or the binary's path is
    /// unknown.
    #[failure(
        code = SERVICE_UNLOCATED,
        hint = "set HOME to your home directory's absolute path and run the command again"
    )]
    Unlocated {
        /// What deadband could not find.
        what: &'static str,
    },
    /// The file at the plist's path cannot be read as a plist.
    #[failure(
        code = SERVICE_PLIST_UNREADABLE,
        hint = "move the file away and run deadband service install again",
        field(path = path.to_string_lossy())
    )]
    Unreadable {
        /// The plist's path.
        path: PathBuf,
        /// Why it cannot be read.
        reason: String,
    },
    /// Writing the plist failed.
    #[failure(
        code = SERVICE_WRITE_FAILED,
        hint = "make ~/Library/LaunchAgents writable, or free disk space, and run install again",
        field(path = path.to_string_lossy())
    )]
    Write {
        /// The plist's path.
        path: PathBuf,
        /// The error of the write.
        error: io::Error,
    },
    /// Deleting the plist failed.
    #[failure(
        code = SERVICE_WRITE_FAILED,
        hint = "make ~/Library/LaunchAgents writable and run the command again",
        field(path = path.to_string_lossy())
    )]
    Delete {
        /// The plist's path.
        path: PathBuf,
        /// The error of the delete.
        error: io::Error,
    },
    /// Another service command held the lock for longer than the wait.
    #[failure(
        code = SERVICE_LOCKED,
        recovery = crumbkit::Recovery::Retry,
        field(path = path.to_string_lossy())
    )]
    Locked {
        /// The lock file.
        path: PathBuf,
    },
    /// The state directory or the lock file in it cannot be made or locked.
    #[failure(
        code = SERVICE_WRITE_FAILED,
        hint = "make the state directory writable and run the command again",
        field(path = path.to_string_lossy())
    )]
    Lock {
        /// The lock file.
        path: PathBuf,
        /// The error of the open or the lock.
        error: io::Error,
    },
    /// A launchctl or getconf run failed.
    #[failure(code = SERVICE_LAUNCHCTL_FAILED, recovery = crumbkit::Recovery::Retry)]
    Launchctl(Failed),
}

impl fmt::Display for ServiceFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("deadband installs its service only with launchd, which this platform does not have"),
            Self::Unlocated { what } => write!(f, "deadband could not find {what}"),
            Self::Unreadable { path, reason } => {
                write!(f, "deadband cannot read {} as a plist: {reason}", path.display())
            }
            Self::Delete { path, error } => {
                write!(f, "deadband could not delete {}: {error}", path.display())
            }
            Self::Write { path, error } => {
                write!(f, "deadband could not write {}: {error}", path.display())
            }
            Self::Locked { path } => write!(
                f,
                "another deadband service command still holds {} after ten seconds, so this one changed nothing",
                path.display()
            ),
            Self::Lock { path, error } => {
                write!(f, "deadband could not lock {}: {error}", path.display())
            }
            Self::Launchctl(failed) => failed.fmt(f),
        }
    }
}

impl std::error::Error for ServiceFailed {}
