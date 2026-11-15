//! The client side of the socket: asking the daemon to start a job, and
//! asking it for its jobs.
//!
//! Every failure to reach the daemon or to read its answer gives
//! [`Admission::Open`], so the job starts at once and the caller says why
//! in one line. Each exchange with the daemon has a deadline: the request's
//! line and the first reply share one, and a queued job waits on a daemon
//! that sends beats for at most [`BEAT_SILENCE`] between two lines.

use std::fmt;
use std::io::{self, BufReader, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::process::getuid;

use crate::daemon::DirectoryFault;
use crate::protocol::{Jobs, Reply, Request, Timeline, read_line, write_line};

mod ask;
mod lease;
mod peer;
#[cfg(test)]
mod tests;

#[cfg(test)]
use self::ask::{Waits, ask_checked, ask_reasking};
pub use self::ask::{ask, ask_noticed};
pub use self::lease::{Lease, Notices};
pub(crate) use self::peer::peer_uid;

/// Reads the uid of the user who owns the process on the other end of a
/// connection. The client replaces it in tests, which cannot start a daemon
/// of another user.
pub(crate) type PeerUid = fn(&UnixStream) -> io::Result<u32>;

/// How long a client waits for the daemon to take its request and send the
/// first answer before it counts the daemon as not answering.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(2);

/// How long a queued client keeps asking for a daemon after its connection
/// closes. When no daemon answers in this time, the job starts at once.
/// Each loss of a daemon that queued the job and then sent a beat starts
/// this window again.
pub const REASK_FOR: Duration = Duration::from_secs(5);

/// How long a queued client whose daemon sends beats waits for any line
/// before it counts the daemon as hung and starts its job. The daemon beats
/// once per five 1-second samples, so this is three beat periods and one
/// sample.
pub const BEAT_SILENCE: Duration = Duration::from_secs(16);

/// How long a queued client pauses between two tries to reach a daemon.
const REASK_EVERY: Duration = Duration::from_millis(100);

/// The daemon's answer to a request to start a job.
#[derive(Debug)]
pub enum Admission {
    /// The daemon granted the job. The job counts as running until the
    /// lease ends.
    Granted(Lease),
    /// The daemon could not be asked, so the job starts at once.
    Open(Unasked),
}

/// Why the daemon could not be asked.
#[derive(Debug)]
pub enum Unasked {
    /// No daemon listens on the socket.
    NotRunning {
        /// The socket's path.
        path: PathBuf,
    },
    /// The socket exists, and connecting to it failed.
    Unreachable {
        /// The socket's path.
        path: PathBuf,
        /// The error of the connection.
        error: io::Error,
    },
    /// The process that listens on the socket belongs to another user, so
    /// the client sent it nothing.
    Foreign {
        /// The socket's path.
        path: PathBuf,
        /// The uid of the user who owns the listening process.
        uid: u32,
    },
    /// The socket is in a directory that another user could have made or can
    /// write to, so the client did not ask the daemon.
    Directory {
        /// The directory's path.
        path: PathBuf,
        /// What is wrong with it.
        fault: DirectoryFault,
    },
    /// The daemon did not take the request or answer it in time.
    Silent {
        /// How long the client waited.
        waited: Duration,
    },
    /// The daemon queued the job, promised beats, and then sent no line in
    /// time.
    Hung {
        /// How long the client waited for a line.
        waited: Duration,
    },
    /// The daemon closed the connection before it granted the job.
    Closed,
    /// The daemon's answer was not one the client knows, or reading it
    /// failed.
    Garbled {
        /// The error of the read.
        error: io::Error,
    },
}

impl fmt::Display for Unasked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning { path } => write!(
                f,
                "the daemon is not running (nothing listens on {})",
                path.display()
            ),
            Self::Unreachable { path, error } => write!(
                f,
                "the daemon at {} could not be reached: {error}",
                path.display()
            ),
            Self::Foreign { path, uid } => write!(
                f,
                "the process that listens on {} belongs to uid {uid}, not to this user, so the command line was not sent",
                path.display()
            ),
            Self::Directory { path, fault } => {
                write!(f, "{} {fault}, so the daemon was not asked", path.display())
            }
            Self::Silent { waited } => write!(
                f,
                "the daemon did not answer within {} ms",
                waited.as_millis()
            ),
            Self::Hung { waited } => write!(
                f,
                "the daemon queued the job and then sent nothing for {} ms",
                waited.as_millis()
            ),
            Self::Closed => {
                f.write_str("the daemon closed the connection before it started the job")
            }
            Self::Garbled { error } => write!(f, "the daemon's answer could not be read: {error}"),
        }
    }
}

/// Connects to the daemon at `path`, or says why it cannot.
fn connect(path: &Path) -> Result<UnixStream, Unasked> {
    connect_checked(path, peer_uid)
}

/// Connects as [`connect`] does, and closes the connection unless `peer`
/// says that this user owns the process that listens. The caller writes
/// nothing before this returns, so a listener of another user never sees a
/// command line.
fn connect_checked(path: &Path, peer: PeerUid) -> Result<UnixStream, Unasked> {
    check_directory(path, getuid().as_raw())?;
    let stream = dial(path).map_err(|error| match error.kind() {
        ErrorKind::NotFound | ErrorKind::ConnectionRefused => Unasked::NotRunning {
            path: path.to_owned(),
        },
        _ => Unasked::Unreachable {
            path: path.to_owned(),
            error,
        },
    })?;
    match peer(&stream) {
        Ok(uid) if uid == getuid().as_raw() => Ok(stream),
        Ok(uid) => Err(Unasked::Foreign {
            path: path.to_owned(),
            uid,
        }),
        Err(error) => Err(Unasked::Unreachable {
            path: path.to_owned(),
            error,
        }),
    }
}

/// Refuses a socket whose directory another user made or can write to,
/// before the client connects: the directory must belong to `uid` or to
/// root, and no group or other user may write to it unless it is sticky, as
/// `/tmp` is. The check is read-only. A directory that fails it can hold a
/// listener of another user that never accepts.
fn check_directory(path: &Path, uid: u32) -> Result<(), Unasked> {
    use std::os::unix::fs::MetadataExt as _;

    let Some(directory) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    // A directory that cannot be read is left to the connection to report.
    let Ok(metadata) = std::fs::metadata(directory) else {
        return Ok(());
    };
    let fault = if metadata.uid() != uid && metadata.uid() != 0 {
        Some(DirectoryFault::Owner {
            owner: metadata.uid(),
        })
    } else if metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0 {
        Some(DirectoryFault::Mode {
            mode: metadata.mode() & 0o7777,
        })
    } else {
        None
    };
    fault.map_or(Ok(()), |fault| {
        Err(Unasked::Directory {
            path: directory.to_owned(),
            fault,
        })
    })
}

/// Connects to the socket at `path` without waiting: a listener whose queue
/// is full makes the call fail at once, where a blocking connect on Linux
/// waits for room for as long as the listener lets it.
fn dial(path: &Path) -> io::Result<UnixStream> {
    use rustix::fs::{OFlags, fcntl_setfl};
    use rustix::io::{FdFlags, fcntl_setfd};
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketType, connect, socket};

    let address = SocketAddrUnix::new(path)?;
    let socket = socket(AddressFamily::UNIX, SocketType::STREAM, None)?;
    fcntl_setfd(&socket, FdFlags::CLOEXEC)?;
    fcntl_setfl(&socket, OFlags::NONBLOCK)?;
    connect(&socket, &address)?;
    fcntl_setfl(&socket, OFlags::empty())?;
    Ok(UnixStream::from(socket))
}

/// Reads one reply from `reader`, saying why when none arrives.
fn reply(reader: &mut BufReader<impl Read>, waited: Duration) -> Result<Reply, Unasked> {
    match read_line::<Reply>(reader) {
        Ok(Some(reply)) => Ok(reply),
        Ok(None) => Err(Unasked::Closed),
        // A daemon that stops with unread data on the connection resets it.
        Err(error) if error.kind() == ErrorKind::ConnectionReset => Err(Unasked::Closed),
        Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
            Err(Unasked::Silent { waited })
        }
        Err(error) => Err(Unasked::Garbled { error }),
    }
}

/// A connection whose reads and writes all end at one moment, so a daemon
/// that takes or sends a line a byte at a time cannot extend the wait. With
/// no moment, reads wait for as long as the connection stays open.
struct Deadline {
    /// The connection.
    stream: UnixStream,
    /// When the reads and writes end, or `None` when reads have no end.
    at: Option<Instant>,
}

impl Deadline {
    /// Returns the time left before `at`, or an error once it has passed.
    fn left(&self) -> io::Result<Option<Duration>> {
        match self
            .at
            .map(|at| at.saturating_duration_since(Instant::now()))
        {
            Some(left) if left.is_zero() => Err(ErrorKind::TimedOut.into()),
            left => Ok(left),
        }
    }
}

impl Read for Deadline {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let left = self.left()?;
        // macOS refuses this call once the peer has closed. The read then
        // returns at once, so the refusal costs nothing.
        let _ = self.stream.set_read_timeout(left);
        self.stream.read(buffer)
    }
}

impl Write for Deadline {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let left = self.left()?;
        self.stream.set_write_timeout(left)?;
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// Connects to the daemon at `path`, sends `request` and reads the first
/// reply that is not a beat. The daemon must take the line and send that
/// reply within `answer_within`. Returns the connection's reader, whose
/// deadline the caller may move, and the reply.
fn exchange(
    path: &Path,
    request: &Request,
    answer_within: Duration,
    peer: PeerUid,
) -> Result<(BufReader<Deadline>, Reply), Unasked> {
    let stream = connect_checked(path, peer)?;
    let at = Some(Instant::now() + answer_within);
    let mut reader = BufReader::new(Deadline { stream, at });
    write_line(reader.get_mut(), request).map_err(|error| match error.kind() {
        ErrorKind::WouldBlock | ErrorKind::TimedOut => Unasked::Silent {
            waited: answer_within,
        },
        _ => Unasked::Closed,
    })?;
    // A beat that another thread of the daemon wrote before its `Queued`
    // reply is skipped.
    loop {
        match reply(&mut reader, answer_within)? {
            Reply::Beat => {}
            first => return Ok((reader, first)),
        }
    }
}

/// Sends `request` to the daemon at `path` and reads one reply, which must
/// arrive whole within `answer_within`.
fn request(
    path: &Path,
    request: &Request,
    answer_within: Duration,
    peer: PeerUid,
) -> Result<Reply, Unasked> {
    exchange(path, request, answer_within, peer).map(|(_, reply)| reply)
}

/// Returns the error of a reply of the wrong kind.
fn wrong_reply() -> Unasked {
    Unasked::Garbled {
        error: ErrorKind::InvalidData.into(),
    }
}

/// Asks the daemon at `path` for its running and waiting jobs.
///
/// # Errors
///
/// Returns why the daemon could not be asked.
pub fn jobs(path: &Path, answer_within: Duration) -> Result<Jobs, Unasked> {
    jobs_checked(path, answer_within, peer_uid)
}

/// Asks as [`jobs`] does, and reads the owner of the daemon with `peer`.
fn jobs_checked(path: &Path, answer_within: Duration, peer: PeerUid) -> Result<Jobs, Unasked> {
    match request(path, &Request::Status, answer_within, peer)? {
        Reply::Jobs(jobs) => Ok(jobs),
        _ => Err(wrong_reply()),
    }
}

/// Asks the daemon at `path` for the jobs that `deadband top` draws.
///
/// # Errors
///
/// Returns why the daemon could not be asked.
pub fn timeline(path: &Path, answer_within: Duration) -> Result<Timeline, Unasked> {
    match request(path, &Request::Timeline, answer_within, peer_uid)? {
        Reply::Timeline(timeline) => Ok(timeline),
        _ => Err(wrong_reply()),
    }
}
