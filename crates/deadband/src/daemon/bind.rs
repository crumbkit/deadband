//! Makes the daemon's socket: one daemon per socket, a socket that only its
//! user can open, and nothing removed that is not a socket.

use std::fmt;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crumbkit::{Code, Failure};
use crumbkit_durable::{LockError, PathLock};
use rustix::fs::Mode;
use rustix::process::{Resource, Rlimit, getrlimit, getuid, setrlimit, umask};

/// The code of a refusal to start a daemon while another one holds the
/// same socket.
pub const DAEMON_RUNNING: Code = Code::from_spelling("deadband::daemon::running");

/// The code of a socket that the daemon could not listen on.
pub const SOCKET_UNUSABLE: Code = Code::from_spelling("deadband::socket::unusable");

/// The code of a socket directory that deadband refuses to listen in.
pub const SOCKET_DIRECTORY: Code = Code::from_spelling("deadband::socket::directory");

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
    /// The directory that deadband owns for the fallback socket is not one
    /// only this user can use, so the daemon does not listen in it.
    #[failure(
        code = SOCKET_DIRECTORY,
        hint = "set XDG_RUNTIME_DIR or DEADBAND_SOCKET_DIR to a directory you own; only the directory's owner or root can remove this one",
        field(path = path.to_string_lossy())
    )]
    Directory {
        /// The directory's path.
        path: PathBuf,
        /// What is wrong with it.
        fault: DirectoryFault,
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
    /// The daemon listened and then stopped serving.
    #[failure(
        code = SOCKET_UNUSABLE,
        no_next_step = "launchd starts a new daemon in its place, and jobs start at once while none listens",
        field(path = path.to_string_lossy())
    )]
    Stopped {
        /// The socket's path.
        path: PathBuf,
        /// Why the daemon stopped serving.
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
            Self::Directory { path, fault } => write!(
                f,
                "{} {fault}, so deadband does not listen in it",
                path.display()
            ),
            Self::Socket { path, error } => write!(
                f,
                "deadband could not listen on {}: {error}",
                path.display()
            ),
            Self::Stopped { path, error } => {
                write!(f, "the daemon on {} stopped: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for Unbound {}

/// Why a directory that deadband owns for its socket is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryFault {
    /// What is at the path is not a directory. A symlink counts, because it
    /// could lead anywhere.
    NotDirectory,
    /// Another user owns the directory.
    Owner {
        /// The uid of the owner.
        owner: u32,
    },
    /// Other users can enter or change the directory.
    Mode {
        /// The directory's permission bits.
        mode: u32,
    },
}

impl fmt::Display for DirectoryFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotDirectory => f.write_str("is not a directory"),
            Self::Owner { owner } => write!(f, "belongs to uid {owner}"),
            Self::Mode { mode } => write!(f, "has mode {mode:04o}, which lets others use it"),
        }
    }
}

/// The mode of the directory that deadband makes for its socket.
const DIRECTORY_MODE: u32 = 0o700;

/// Makes `directory` with mode 0700 when it does not exist, and checks that
/// it is a directory (not a symlink) that this user owns and that no one
/// else can enter or change. A directory that fails the check is left as
/// it is, and the daemon does not listen in it.
///
/// # Errors
///
/// Returns [`Unbound::Directory`] when the directory is not safe, and
/// [`Unbound::Socket`] when it cannot be made or read.
pub(crate) fn private_directory(directory: &Path) -> Result<(), Unbound> {
    private_directory_as(directory, getuid().as_raw())
}

/// Checks the directory as [`private_directory`] does for the user whose
/// uid is `uid`.
pub(crate) fn private_directory_as(directory: &Path, uid: u32) -> Result<(), Unbound> {
    let failed = |error| Unbound::Socket {
        path: directory.to_owned(),
        error,
    };
    match std::fs::DirBuilder::new()
        .mode(DIRECTORY_MODE)
        .create(directory)
    {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(failed(error)),
    }
    let metadata = std::fs::symlink_metadata(directory).map_err(failed)?;
    let fault = if !metadata.is_dir() {
        Some(DirectoryFault::NotDirectory)
    } else if metadata.uid() != uid {
        Some(DirectoryFault::Owner {
            owner: metadata.uid(),
        })
    } else if metadata.mode() & 0o077 != 0 {
        Some(DirectoryFault::Mode {
            mode: metadata.mode() & 0o7777,
        })
    } else {
        None
    };
    fault.map_or(Ok(()), |fault| {
        Err(Unbound::Directory {
            path: directory.to_owned(),
            fault,
        })
    })
}

/// A listening socket and the lock that keeps a second daemon off it. The
/// lock holds until the value is dropped.
#[derive(Debug)]
pub struct Bound {
    /// The listening socket.
    pub listener: UnixListener,
    /// The lock on the lock file next to the socket, which the daemon's
    /// watchdog and sampler check with [`PathLock::still_current`].
    pub(super) lock: Arc<PathLock>,
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
/// The daemon first takes an exclusive lock on `<path>.lock` through
/// crumbkit-durable's [`PathLock`], without waiting, so two daemons never
/// both bind. The lock file is not followed through a symlink and must
/// belong to this user. Removing the lock file lets a second daemon lock a
/// new one, so a serving daemon checks its lock with its socket file once a
/// minute and stops when either is gone or replaced. A socket
/// left at `path` by a daemon that ended is removed; anything else at
/// `path` is refused. The socket and the lock
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
    // A deadline that has passed tries the lock once.
    let lock = match PathLock::acquire(&lock_path(path), Instant::now()) {
        Ok(lock) => lock,
        Err(LockError::Busy { .. }) => {
            return Err(Unbound::Running {
                path: path.to_owned(),
            });
        }
        Err(LockError::Io { source, .. }) => return Err(socket_error(source)),
        Err(error) => return Err(socket_error(io::Error::from(error))),
    };
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
        lock: Arc::new(lock),
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

/// The mode of the state directory: only its owner may enter it.
const STATE_DIR_MODE: u32 = 0o700;

/// Makes the state directory `directory` with mode 0700 when it does not
/// exist, and sets that mode when it does. The history writer, the reclaim
/// lock and the service lock each call it before they open a file there, so
/// whichever runs first leaves the directory private.
///
/// # Errors
///
/// Returns the error of making the directory or of setting its mode.
pub(crate) fn make_state_dir(directory: &Path) -> io::Result<()> {
    use std::fs::{self, DirBuilder, Permissions};
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    DirBuilder::new()
        .recursive(true)
        .mode(STATE_DIR_MODE)
        .create(directory)?;
    fs::set_permissions(directory, Permissions::from_mode(STATE_DIR_MODE))
}
