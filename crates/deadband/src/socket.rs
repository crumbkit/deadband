//! Finds the path of the per-user Unix socket that the daemon listens on,
//! and lists the environment variables it reads for it (BASE-28).
//! [`environment`](crate::environment) lists the other variables deadband
//! reads.
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
        absent: "deadband reads DEADBAND_SOCKET_DIR",
    },
    Variable {
        name: "DEADBAND_SOCKET_DIR",
        purpose: "the directory that holds `deadband.sock`, for a daemon and its callers that are not the launchd daemon",
        form: "a directory path",
        absent: "on macOS deadband uses the directory that `getconf DARWIN_USER_TEMP_DIR` prints, which it reads with `confstr`, and when that fails it reads TMPDIR",
    },
    Variable {
        name: "TMPDIR",
        purpose: "the temporary directory that holds `deadband.sock` where there is no per-user directory to ask for, such as on Linux",
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

/// Returns the path of the daemon's socket for a caller whose per-user
/// temporary directory `user_directory` returns, which on macOS is the
/// directory that `getconf DARWIN_USER_TEMP_DIR` prints.
///
/// The launchd daemon listens in that directory whatever `TMPDIR` its callers
/// have, so `run` and `status` look there too. `DEADBAND_SOCKET` and
/// `DEADBAND_SOCKET_DIR` still win, and then `user_directory` is not called.
/// When it returns `None`, the path is the one [`path`] returns.
pub fn path_in_user_directory(
    read: impl Fn(&str) -> Option<OsString>,
    user_directory: impl FnOnce() -> Option<PathBuf>,
) -> PathBuf {
    let overridden = ["DEADBAND_SOCKET", "DEADBAND_SOCKET_DIR"]
        .iter()
        .any(|name| read(name).is_some_and(|value| !value.is_empty()));
    let directory = if overridden { None } else { user_directory() };
    let Some(directory) = directory else {
        return path(read);
    };
    path(|name| match name {
        "TMPDIR" => Some(directory.clone().into_os_string()),
        other => read(other),
    })
}

/// Returns the path of the daemon's socket, reading each variable with
/// `read`, which returns `None` for an absent variable.
///
/// The path is `DEADBAND_SOCKET` when it is set. Otherwise it is
/// `deadband.sock` in `DEADBAND_SOCKET_DIR`, and otherwise in `TMPDIR`. When
/// none is set, it is `/tmp/deadband-<USER>.sock`, where other users can see
/// it; the daemon makes the socket readable and writable by its own user only.
pub fn path(read: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    let set = |name: &str| read(name).filter(|value| !value.is_empty());
    if let Some(socket) = set("DEADBAND_SOCKET") {
        return PathBuf::from(socket);
    }
    if let Some(directory) = set("DEADBAND_SOCKET_DIR") {
        return PathBuf::from(directory).join(FILE);
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
