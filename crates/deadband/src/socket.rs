//! Finds the path of the per-user Unix socket that the daemon listens on,
//! and lists the environment variables it reads for it (BASE-28).
//! [`environment`](crate::environment) lists the other variables deadband
//! reads.
//!
//! The entry point reads the environment and passes a reader to [`path`],
//! so this module reads no variable itself (BASE-6).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rustix::process::getuid;

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

/// The directory that deadband makes inside `XDG_RUNTIME_DIR`.
const RUNTIME_SUBDIRECTORY: &str = "deadband";

/// The directory of the socket when no per-user directory is set and there
/// is no runtime directory: `/tmp/deadband-<uid>`.
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
        absent: "deadband reads XDG_RUNTIME_DIR",
    },
    Variable {
        name: "XDG_RUNTIME_DIR",
        purpose: "the runtime directory, in which the daemon makes a `deadband` directory with mode 0700 to hold `deadband.sock`",
        form: "a directory path",
        absent: "deadband makes /tmp/deadband-<uid> with mode 0700 and uses `deadband.sock` in it",
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
/// none is set, it is `deadband.sock` in `deadband` inside `XDG_RUNTIME_DIR`,
/// or else in `/tmp/deadband-<uid>`, a directory that the daemon makes so
/// that only this user can enter it.
pub fn path(read: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    path_for(read, getuid().as_raw())
}

/// Returns the path as [`path`] does for the user whose uid is `uid`.
pub(crate) fn path_for(read: impl Fn(&str) -> Option<OsString>, uid: u32) -> PathBuf {
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
    fallback(&read, uid).join(FILE)
}

/// Returns the directory that holds the socket when no variable names one:
/// `deadband` in `XDG_RUNTIME_DIR`, or else `/tmp/deadband-<uid>`.
fn fallback(read: &impl Fn(&str) -> Option<OsString>, uid: u32) -> PathBuf {
    match read("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) {
        Some(runtime) => PathBuf::from(runtime).join(RUNTIME_SUBDIRECTORY),
        None => PathBuf::from(SHARED_DIRECTORY).join(format!("deadband-{uid}")),
    }
}

/// Returns the directory that the daemon makes and checks for the user
/// whose uid is `uid`, which is the one of the fallback socket. It returns
/// `None` when `DEADBAND_SOCKET`, `DEADBAND_SOCKET_DIR` or `TMPDIR` names the
/// socket, because the person who named it owns the directory it is in.
pub(crate) fn fallback_directory(
    read: impl Fn(&str) -> Option<OsString>,
    uid: u32,
) -> Option<PathBuf> {
    let named = ["DEADBAND_SOCKET", "DEADBAND_SOCKET_DIR", "TMPDIR"]
        .iter()
        .any(|name| read(name).is_some_and(|value| !value.is_empty()));
    (!named).then(|| fallback(&read, uid))
}

/// Returns the directory that deadband owns for `socket`, or `None` when
/// `socket` is not the fallback socket, as when the per-user directory of
/// macOS moved it.
pub fn owned_directory(read: impl Fn(&str) -> Option<OsString>, socket: &Path) -> Option<PathBuf> {
    owned_directory_for(read, getuid().as_raw(), socket)
}

/// Returns the directory as [`owned_directory`] does for the user whose uid
/// is `uid`.
pub(crate) fn owned_directory_for(
    read: impl Fn(&str) -> Option<OsString>,
    uid: u32,
    socket: &Path,
) -> Option<PathBuf> {
    let directory = fallback_directory(read, uid)?;
    (socket == directory.join(FILE)).then_some(directory)
}
