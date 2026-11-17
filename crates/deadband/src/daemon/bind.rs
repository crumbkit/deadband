//! Makes the daemon's socket: one daemon per socket, a socket that only its
//! user can open, and nothing removed that is not a socket.

use std::fmt;
use std::fs::{File, TryLockError};
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use crumbkit::{Code, Failure};
use rustix::fs::{Mode, OFlags};
use rustix::process::{Resource, Rlimit, getrlimit, getuid, setrlimit, umask};

/// The code of a refusal to start a daemon while another one holds the
/// same socket.
pub const DAEMON_RUNNING: Code = Code::new("deadband", "daemon", "running");

/// The code of a socket that the daemon could not listen on.
pub const SOCKET_UNUSABLE: Code = Code::new("deadband", "socket", "unusable");

/// The code of a socket directory that deadband refuses to listen in.
pub const SOCKET_DIRECTORY: Code = Code::new("deadband", "socket", "directory");

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

/// Opens the lock file at `path`, making it with mode 0600 when it does not
/// exist, and takes an exclusive lock on it without waiting. The lock holds
/// until the returned file is closed. The daemon's socket and the writer of
/// the job history both lock through this function.
///
/// The open does not follow a symlink at `path`, and the opened file must
/// belong to the caller, so another user cannot plant a link or a file that
/// makes the daemon lock or create something of theirs.
///
/// # Errors
///
/// Returns [`TryLockError::WouldBlock`] when another open file holds the
/// lock, and [`TryLockError::Error`] when the file cannot be opened or
/// locked, is a symlink, or belongs to another user.
pub(crate) fn take_lock(path: &Path) -> Result<File, TryLockError> {
    take_lock_as(path, getuid().as_raw())
}

/// Takes the lock as [`take_lock`] does for the user whose uid is `uid`.
pub(crate) fn take_lock_as(path: &Path, uid: u32) -> Result<File, TryLockError> {
    let opened = rustix::fs::open(
        path,
        OFlags::WRONLY | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|errno| TryLockError::Error(io::Error::from(errno)))?;
    let lock = File::from(opened);
    let owner = lock.metadata().map_err(TryLockError::Error)?.uid();
    if owner != uid {
        return Err(TryLockError::Error(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} belongs to uid {owner}, not to this user",
                path.display()
            ),
        )));
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "crates/deadband/src/daemon/bind.rs takes each lock without waiting, so the lock needs no deadline"
    )]
    let locked = lock.try_lock();
    locked.map(|()| lock)
}

/// Makes the socket at `path` and listens on it.
///
/// The daemon first takes an exclusive lock on `<path>.lock`, so two
/// daemons never both bind. Removing the lock file lets a second daemon
/// lock a new one, so a serving daemon checks its lock file with its socket
/// file once a minute and stops when either is gone or replaced. A socket
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
    let lock = match take_lock(&lock_path(path)) {
        Ok(lock) => lock,
        Err(TryLockError::WouldBlock) => {
            return Err(Unbound::Running {
                path: path.to_owned(),
            });
        }
        Err(TryLockError::Error(error)) => return Err(socket_error(error)),
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
