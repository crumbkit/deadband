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
use std::time::{Duration, Instant};

use crate::admit::Hold;
use crate::protocol::{Job, Jobs, Reply, Request, Timeline, read_line, write_line};

mod lease;
#[cfg(test)]
mod tests;

pub use self::lease::{Lease, Notices};

/// How long a client waits for the daemon's first answer before it counts
/// the daemon as not answering.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(2);

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
    UnixStream::connect(path).map_err(|error| match error.kind() {
        ErrorKind::NotFound | ErrorKind::ConnectionRefused => Unasked::NotRunning {
            path: path.to_owned(),
        },
        _ => Unasked::Unreachable {
            path: path.to_owned(),
            error,
        },
    })
}

/// Reads one reply from `reader`, saying why when none arrives.
fn reply(reader: &mut BufReader<impl Read>, waited: Duration) -> Result<Reply, Unasked> {
    match read_line::<Reply>(reader) {
        Ok(Some(reply)) => Ok(reply),
        Ok(None) => Err(Unasked::Closed),
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
    match try_ask(path, job, answer_within, queued, notices) {
        Ok(lease) => Admission::Granted(lease),
        Err(unasked) => Admission::Open(unasked),
    }
}

/// Asks as [`ask`] does, and returns the lease or why there is none.
fn try_ask(
    path: &Path,
    job: &Job,
    answer_within: Duration,
    queued: impl FnOnce(usize, Option<Hold>),
    notices: Option<&Notices>,
) -> Result<Lease, Unasked> {
    let mut stream = connect(path)?;
    let garbled = |error| Unasked::Garbled { error };
    stream
        .set_read_timeout(Some(answer_within))
        .map_err(garbled)?;
    write_line(&mut stream, &Request::Ask(job.clone())).map_err(|_| Unasked::Closed)?;
    let mut reader = BufReader::new(stream.try_clone().map_err(garbled)?);
    match reply(&mut reader, answer_within)? {
        Reply::Granted { jobs } => return Ok(Lease::watched(stream, jobs, path, job, notices)),
        Reply::Queued { ahead, held } => queued(ahead, held),
        _ => return Err(wrong_reply()),
    }
    stream.set_read_timeout(None).map_err(garbled)?;
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
fn request(path: &Path, request: &Request, answer_within: Duration) -> Result<Reply, Unasked> {
    let mut stream = connect(path)?;
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
    match request(path, &Request::Status, answer_within)? {
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
    match request(path, &Request::Timeline, answer_within)? {
        Reply::Timeline(timeline) => Ok(timeline),
        _ => Err(wrong_reply()),
    }
}
