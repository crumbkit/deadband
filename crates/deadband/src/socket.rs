//! Finds the path of the per-user Unix socket that the daemon listens on,
//! and lists the environment variables deadband reads (BASE-28).
//!
//! The entry point reads the environment and passes a reader to [`path`],
//! so this module reads no variable itself (BASE-6).

use std::ffi::OsString;
use std::path::PathBuf;

#[cfg(test)]
mod tests;

/// One environment variable that deadband reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Variable {
    /// The variable's name.
    pub name: &'static str,
    /// What deadband uses it for.
    pub purpose: &'static str,
    /// The form of value deadband accepts.
    pub form: &'static str,
    /// What deadband does when the variable is absent or empty.
    pub absent: &'static str,
}

/// The file name of the socket inside a per-user directory.
const FILE: &str = "deadband.sock";

/// The directory of the socket when no per-user directory is set.
const SHARED_DIRECTORY: &str = "/tmp";

/// Every environment variable deadband reads, in the order [`path`] reads
/// them. deadband takes the first one that is set and not empty.
pub const VARIABLES: &[Variable] = &[
    Variable {
        name: "DEADBAND_SOCKET",
        purpose: "the path of the daemon's socket, which the daemon, `run` and `status` all use",
        form: "a file path",
        absent: "deadband reads TMPDIR",
    },
    Variable {
        name: "TMPDIR",
        purpose: "the temporary directory, which macOS makes per-user, and which holds `deadband.sock`",
        form: "a directory path",
        absent: "deadband uses /tmp/deadband-<USER>.sock",
    },
    Variable {
        name: "USER",
        purpose: "the user's name, which keeps one user's socket in /tmp apart from another's",
        form: "a user name",
        absent: "deadband uses /tmp/deadband.sock",
    },
];

/// Returns the path of the daemon's socket, reading each variable with
/// `read`, which returns `None` for an absent variable.
///
/// The path is `DEADBAND_SOCKET` when it is set. Otherwise it is
/// `deadband.sock` in `TMPDIR`, which macOS makes per-user. When neither is
/// set, it is `/tmp/deadband-<USER>.sock`, where other users can see it; the
/// daemon makes the socket readable and writable by its own user only.
pub fn path(read: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    let set = |name: &str| read(name).filter(|value| !value.is_empty());
    if let Some(socket) = set("DEADBAND_SOCKET") {
        return PathBuf::from(socket);
    }
    if let Some(directory) = set("TMPDIR") {
        return PathBuf::from(directory).join(FILE);
    }
    let file = match set("USER") {
        Some(user) => {
            let mut file = OsString::from("deadband-");
            file.push(user);
            file.push(".sock");
            file
        }
        None => OsString::from(FILE),
    };
    PathBuf::from(SHARED_DIRECTORY).join(file)
}
