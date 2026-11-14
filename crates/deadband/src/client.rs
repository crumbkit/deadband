//! The client side of the socket: asking the daemon to start a job, and
//! asking it for its jobs.
//!
//! Every failure to reach the daemon or to read its answer gives
//! [`Admission::Open`], so the job starts at once and the caller says why
//! in one line.

use std::fmt;
use std::io::{self, BufReader, ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::getuid;

use crate::admit::Hold;
use crate::daemon::DirectoryFault;
use crate::protocol::{Job, Jobs, Reply, Request, Timeline, read_line, write_line};

mod lease;
mod peer;
#[cfg(test)]
mod tests;

pub use self::lease::{Lease, Notices};
pub(crate) use self::peer::peer_uid;

/// Reads the uid of the user who owns the process on the other end of a
/// connection. The client replaces it in tests, which cannot start a daemon
/// of another user.
pub(crate) type PeerUid = fn(&UnixStream) -> io::Result<u32>;

/// How long a client waits for the daemon's first answer before it counts
/// the daemon as not answering.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(2);

/// How long a queued client keeps asking for a daemon after its connection
/// closes. When no daemon answers in this time, the job starts at once.
pub const REASK_FOR: Duration = Duration::from_secs(5);

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
    /// The daemon did not answer in time.
    Silent {
        /// How long the client waited.
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

/// Asks the daemon at `path` to start `job`, and waits until it does.
///
/// The daemon's first answer must arrive within `answer_within`. When the
/// job waits, `queued` receives how many jobs start before it and why the
/// first of them waits, and the client then waits for the grant as long as
/// the daemon keeps the connection open.
pub fn ask(
    path: &Path,
    job: &Job,
    answer_within: Duration,
    queued: impl FnOnce(usize, Option<Hold>),
) -> Admission {
    ask_noticed(path, job, answer_within, queued, None)
}

/// Asks as [`ask`] does, and passes each line about the job's activity to
/// `notices` while the job runs.
pub fn ask_noticed(
    path: &Path,
    job: &Job,
    answer_within: Duration,
    queued: impl FnOnce(usize, Option<Hold>),
    notices: Option<&Notices>,
) -> Admission {
    ask_reasking(path, job, answer_within, REASK_FOR, queued, notices)
}

/// Asks as [`ask_noticed`] does, and keeps asking for `reask_for` after a
/// queued job loses its connection.
fn ask_reasking(
    path: &Path,
    job: &Job,
    answer_within: Duration,
    reask_for: Duration,
    queued: impl FnOnce(usize, Option<Hold>),
    notices: Option<&Notices>,
) -> Admission {
    ask_reasking_checked(
        path,
        job,
        answer_within,
        reask_for,
        queued,
        notices,
        peer_uid,
    )
}

/// Asks as [`ask`] does, and reads the owner of the daemon with `peer`.
#[cfg(test)]
fn ask_checked(
    path: &Path,
    job: &Job,
    answer_within: Duration,
    queued: impl FnOnce(usize, Option<Hold>),
    notices: Option<&Notices>,
    peer: PeerUid,
) -> Admission {
    ask_reasking_checked(path, job, answer_within, REASK_FOR, queued, notices, peer)
}

/// Asks as [`ask_reasking`] does, and reads the owner of the daemon with
/// `peer`.
fn ask_reasking_checked(
    path: &Path,
    job: &Job,
    answer_within: Duration,
    reask_for: Duration,
    queued: impl FnOnce(usize, Option<Hold>),
    notices: Option<&Notices>,
    peer: PeerUid,
) -> Admission {
    let mut queued = Some(queued);
    // When the connection of a queued job first closed. The bound counts
    // from this moment, however often a daemon queues the job again.
    let mut lost: Option<Instant> = None;
    loop {
        let mut was_queued = false;
        match try_ask(
            path,
            job,
            answer_within,
            &mut queued,
            &mut was_queued,
            notices,
            peer,
        ) {
            Ok(lease) => return Admission::Granted(lease),
            Err(Unasked::Closed) if was_queued => {
                lost.get_or_insert_with(Instant::now);
            }
            Err(unasked) if lost.is_none() => return Admission::Open(unasked),
            Err(_) => {}
        }
        // No daemon kept the job in time, so it starts, and the line says
        // the connection that queued it closed.
        if lost.is_some_and(|at| at.elapsed() >= reask_for) {
            return Admission::Open(Unasked::Closed);
        }
        pause();
    }
}

/// Waits [`REASK_EVERY`] before the next try to reach a daemon.
fn pause() {
    #[expect(
        clippy::disallowed_methods,
        reason = "the pause between tries to reach a daemon, which has no event to wait on; REASK_EVERY names it"
    )]
    thread::sleep(REASK_EVERY);
}

/// Asks once as [`ask`] does, and returns the lease or why there is none.
/// `queued` is taken the first time the daemon queues the job, and
/// `was_queued` says whether the daemon queued it on this try.
fn try_ask(
    path: &Path,
    job: &Job,
    answer_within: Duration,
    queued: &mut Option<impl FnOnce(usize, Option<Hold>)>,
    was_queued: &mut bool,
    notices: Option<&Notices>,
    peer: PeerUid,
) -> Result<Lease, Unasked> {
    let mut stream = connect_checked(path, peer)?;
    let garbled = |error| Unasked::Garbled { error };
    stream
        .set_read_timeout(Some(answer_within))
        .map_err(garbled)?;
    write_line(&mut stream, &Request::Ask(job.clone())).map_err(|_| Unasked::Closed)?;
    let mut reader = BufReader::new(stream.try_clone().map_err(garbled)?);
    match reply(&mut reader, answer_within)? {
        Reply::Granted { jobs } => return Ok(Lease::watched(stream, jobs, path, job, notices)),
        Reply::Queued { ahead, held } => {
            *was_queued = true;
            if let Some(queued) = queued.take() {
                queued(ahead, held);
            }
        }
        _ => return Err(wrong_reply()),
    }
    // macOS refuses this call once the daemon has closed the connection.
    // The read then returns at once, so the refusal costs nothing. A
    // connection that is still open never refuses the call, so the read
    // that follows always waits without a deadline.
    let _ = stream.set_read_timeout(None);
    match reply(&mut reader, answer_within)? {
        Reply::Granted { jobs } => Ok(Lease::watched(stream, jobs, path, job, notices)),
        _ => Err(wrong_reply()),
    }
}

/// A connection whose reads all end at one moment, so a daemon that sends
/// a reply a byte at a time cannot extend the wait.
struct Deadline {
    /// The connection.
    stream: UnixStream,
    /// When the reads end.
    at: Instant,
}

impl Read for Deadline {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let left = self.at.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(ErrorKind::TimedOut.into());
        }
        // macOS refuses this call once the peer has closed. The read then
        // returns at once, so the refusal costs nothing.
        let _ = self.stream.set_read_timeout(Some(left));
        self.stream.read(buffer)
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
    let mut stream = connect_checked(path, peer)?;
    let at = Instant::now() + answer_within;
    stream
        .set_read_timeout(Some(answer_within))
        .map_err(|error| Unasked::Garbled { error })?;
    write_line(&mut stream, request).map_err(|_| Unasked::Closed)?;
    reply(&mut BufReader::new(Deadline { stream, at }), answer_within)
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
