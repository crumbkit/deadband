//! The client side of the socket: asking the daemon to start a job, and
//! asking it for its jobs.
//!
//! Every failure to reach the daemon or to read its answer gives
//! [`Admission::Open`], so the job starts at once and the caller says why
//! in one line.

use std::fmt;
use std::io::{self, BufReader, ErrorKind};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::protocol::{Job, Jobs, Reply, Request, read_line, write_line};

#[cfg(test)]
mod tests;

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

/// A granted job's connection to the daemon.
#[derive(Debug)]
pub struct Lease {
    /// The connection the job holds while it runs.
    stream: UnixStream,
}

impl Lease {
    /// Tells the daemon that the job ended with `exit_code` and closes the
    /// connection. A daemon that cannot be told sees the connection close,
    /// which frees the job's place as well.
    pub fn end(mut self, exit_code: Option<i32>) {
        let _ = write_line(&mut self.stream, &Request::Done { exit_code });
    }
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
fn reply(reader: &mut BufReader<UnixStream>, waited: Duration) -> Result<Reply, Unasked> {
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
/// job waits, `queued` receives how many jobs wait in front of it, and the
/// client then waits for the grant as long as the daemon keeps the
/// connection open.
pub fn ask(
    path: &Path,
    job: &Job,
    answer_within: Duration,
    queued: impl FnOnce(usize),
) -> Admission {
    match try_ask(path, job, answer_within, queued) {
        Ok(lease) => Admission::Granted(lease),
        Err(unasked) => Admission::Open(unasked),
    }
}

/// Asks as [`ask`] does, and returns the lease or why there is none.
fn try_ask(
    path: &Path,
    job: &Job,
    answer_within: Duration,
    queued: impl FnOnce(usize),
) -> Result<Lease, Unasked> {
    let mut stream = connect(path)?;
    let garbled = |error| Unasked::Garbled { error };
    stream
        .set_read_timeout(Some(answer_within))
        .map_err(garbled)?;
    write_line(&mut stream, &Request::Ask(job.clone())).map_err(|_| Unasked::Closed)?;
    let mut reader = BufReader::new(stream.try_clone().map_err(garbled)?);
    match reply(&mut reader, answer_within)? {
        Reply::Granted => return Ok(Lease { stream }),
        Reply::Queued { ahead } => queued(ahead),
        Reply::Jobs(_) => return Err(garbled(ErrorKind::InvalidData.into())),
    }
    stream.set_read_timeout(None).map_err(garbled)?;
    match reply(&mut reader, answer_within)? {
        Reply::Granted => Ok(Lease { stream }),
        Reply::Queued { .. } | Reply::Jobs(_) => Err(garbled(ErrorKind::InvalidData.into())),
    }
}

/// Asks the daemon at `path` for its running and waiting jobs.
///
/// # Errors
///
/// Returns why the daemon could not be asked.
pub fn jobs(path: &Path, answer_within: Duration) -> Result<Jobs, Unasked> {
    let mut stream = connect(path)?;
    let garbled = |error| Unasked::Garbled { error };
    stream
        .set_read_timeout(Some(answer_within))
        .map_err(garbled)?;
    write_line(&mut stream, &Request::Status).map_err(|_| Unasked::Closed)?;
    let mut reader = BufReader::new(stream);
    match reply(&mut reader, answer_within)? {
        Reply::Jobs(jobs) => Ok(jobs),
        Reply::Granted | Reply::Queued { .. } => Err(garbled(ErrorKind::InvalidData.into())),
    }
}
