//! Makes the daemon's socket: one daemon per socket, a socket that only its
//! user can open, and nothing removed that is not a socket.

use std::fmt;
use std::fs::File;
use std::io;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use crumbkit::{Code, Failure};
use rustix::fs::Mode;
use rustix::process::{Resource, Rlimit, getrlimit, setrlimit, umask};

/// The code of a refusal to start a daemon while another one holds the
/// same socket.
pub const DAEMON_RUNNING: Code = Code::new("deadband", "daemon", "running");

/// The code of a socket that the daemon could not listen on.
pub const SOCKET_UNUSABLE: Code = Code::new("deadband", "socket", "unusable");

/// The file mode bits that the umask clears while the daemon makes its
/// socket and lock file: every bit for the group and for others.
const PRIVATE: Mode = Mode::RWXG.union(Mode::RWXO);

/// A daemon that could not start listening.
#[derive(Debug, Failure)]
pub enum Unbound {
    /// Another daemon holds the socket's lock.
    #[failure(
        code = DAEMON_RUNNING,
        no_next_step = "one daemon serves each socket, and the running one already admits jobs",
        field(path = path.to_string_lossy())
    )]
    Running {
        /// The socket's path.
        path: PathBuf,
    },
    /// Something other than a socket is at the socket's path, and the
    /// daemon does not remove it.
    #[failure(
        code = SOCKET_UNUSABLE,
        hint = "make the socket's directory exist and be writable, and remove what is at its path, or name another path in DEADBAND_SOCKET",
        field(path = path.to_string_lossy())
    )]
    NotSocket {
        /// The socket's path.
        path: PathBuf,
    },
    /// The socket or its lock could not be made.
    #[failure(
        code = SOCKET_UNUSABLE,
        hint = "make the socket's directory exist and be writable, and remove what is at its path, or name another path in DEADBAND_SOCKET",
        field(path = path.to_string_lossy())
    )]
    Socket {
        /// The socket's path.
        path: PathBuf,
        /// The error of the call that failed.
        error: io::Error,
    },
}

impl fmt::Display for Unbound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running { path } => {
                write!(f, "a deadband daemon already listens on {}", path.display())
            }
            Self::NotSocket { path } => write!(
                f,
                "{} exists and is not a socket, so deadband does not replace it",
                path.display()
            ),
            Self::Socket { path, error } => write!(
                f,
                "deadband could not listen on {}: {error}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for Unbound {}

/// A listening socket and the lock that keeps a second daemon off it. The
/// lock holds until the value is dropped.
#[derive(Debug)]
pub struct Bound {
    /// The listening socket.
    pub listener: UnixListener,
    /// The open lock file, which holds an exclusive lock.
    _lock: File,
}

/// Returns the path of the lock file next to the socket at `path`.
#[must_use]
pub fn lock_path(path: &Path) -> PathBuf {
    let mut lock = path.as_os_str().to_owned();
    lock.push(".lock");
    PathBuf::from(lock)
}

/// Makes the socket at `path` and listens on it.
///
/// The daemon first takes an exclusive lock on `<path>.lock`, so two
/// daemons never both bind. A socket left at `path` by a daemon that ended
/// is removed; anything else at `path` is refused. The socket and the lock
/// file are made with a umask that keeps every other user out.
///
/// # Errors
///
/// Returns [`Unbound::Running`] when another daemon holds the lock,
/// [`Unbound::NotSocket`] when something other than a socket is at `path`,
/// and [`Unbound::Socket`] when the lock or the socket cannot be made.
pub fn bind(path: &Path) -> Result<Bound, Unbound> {
    let before = umask(PRIVATE);
    let bound = bind_private(path);
    umask(before);
    bound
}

/// Binds as [`bind`] does, with the umask already set.
fn bind_private(path: &Path) -> Result<Bound, Unbound> {
    let socket_error = |error| Unbound::Socket {
        path: path.to_owned(),
        error,
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "crates/deadband/src/daemon/bind.rs opens the daemon's lock file, which holds no data, so it needs no atomic writer"
    )]
    let mut options = std::fs::OpenOptions::new();
    let lock = options
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path(path))
        .map_err(socket_error)?;
    #[expect(
        clippy::disallowed_methods,
        reason = "the daemon takes its lock without waiting, so the lock needs no deadline"
    )]
    let locked = lock.try_lock();
    match locked {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(Unbound::Running {
                path: path.to_owned(),
            });
        }
        Err(std::fs::TryLockError::Error(error)) => return Err(socket_error(error)),
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            std::fs::remove_file(path).map_err(socket_error)?;
        }
        Ok(_) => {
            return Err(Unbound::NotSocket {
                path: path.to_owned(),
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(socket_error(error)),
    }
    let listener = UnixListener::bind(path).map_err(socket_error)?;
    Ok(Bound {
        listener,
        _lock: lock,
    })
}

/// The most open files the daemon asks for. macOS refuses a soft limit
/// above its per-process maximum of 10240, even when the hard limit is
/// unlimited.
const MAX_FILES: u64 = 10_240;

/// Raises the soft limit on open files towards the hard limit, to at most
/// 10240, so that each waiting client's connection has a
/// descriptor. A limit that cannot be raised stays as it was.
pub fn raise_descriptor_limit() {
    let limit = getrlimit(Resource::Nofile);
    let wanted = limit
        .maximum
        .map_or(MAX_FILES, |maximum| maximum.min(MAX_FILES));
    if limit.current.is_some_and(|current| current < wanted) {
        let _ = setrlimit(
            Resource::Nofile,
            Rlimit {
                current: Some(wanted),
                maximum: limit.maximum,
            },
        );
    }
}
