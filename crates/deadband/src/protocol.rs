//! The messages that a client and the daemon exchange over the socket, one
//! JSON object per line.
//!
//! A client opens one connection per request. `run` sends [`Request::Ask`],
//! reads [`Reply::Granted`] or [`Reply::Queued`] and then, if it waited,
//! [`Reply::Granted`], which carries the job's threads, keeps the connection open while its job runs, and
//! sends [`Request::Done`] when the job ends. The daemon counts the job as
//! running until that line arrives or the connection closes. When that
//! connection closes while the job runs, `run` connects again every 100 ms
//! and sends [`Request::Seat`], and the daemon that reads it counts the job
//! as running without a reply. A daemon that does not know the line closes
//! the connection, and `run` tries again. `status` sends
//! [`Request::Status`] and reads [`Reply::Jobs`]. `top` sends
//! [`Request::Timeline`] once a second and reads [`Reply::Timeline`]. While
//! a job started with `watched` runs, the daemon may write [`Reply::Stuck`]
//! and [`Reply::Active`] on its connection.

use std::fmt;
use std::io::{self, BufRead, Read, Write};

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::admit::{Hold, Lane};
use crate::reclaim::Reclaimed;

#[cfg(test)]
mod tests;
mod timeline;

pub use self::timeline::{
    Ago, Exit, JobRecord, JobState, LABEL_BYTES, Label, StuckPeriod, Timeline,
};

/// The longest line, in bytes, that either side reads. A longer line is
/// refused as malformed, so a peer cannot make the reader hold an unbounded
/// line in memory.
pub const MAX_LINE: u64 = 64 * 1024;

/// A job, as a client describes it and `status` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Job {
    /// The label that `--label` gave, or the program's name.
    pub label: String,
    /// The process ID of the `deadband run` that asked for the job.
    pub pid: u32,
    /// The job's command line, with its words joined by spaces.
    pub command: String,
    /// The lane the job waits in. A client that names none asks for the
    /// normal lane.
    #[serde(default)]
    pub lane: Lane,
    /// Whether the daemon watches this job's process tree, whose root is
    /// `pid`. A client that names none, and every job that a program embeds,
    /// is not watched.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub watched: bool,
}

/// What the daemon knows of the processes outside the jobs when a job is
/// stuck.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum OutsideJobs {
    /// The busiest process outside the jobs uses at least 50% of one core.
    Busy {
        /// The process's name.
        name: String,
        /// The process's share of one core, in percent.
        cpu_percent: u32,
    },
    /// No process outside the jobs uses 50% of one core.
    Quiet,
}

impl OutsideJobs {
    /// Returns the clause that ends a line of `deadband run`.
    #[must_use]
    pub fn clause(&self) -> String {
        match self {
            Self::Busy { name, cpu_percent } => {
                format!("the busiest process outside the jobs is {name} at {cpu_percent}% CPU")
            }
            Self::Quiet => "no process outside the jobs is using more than 50% CPU".to_owned(),
        }
    }
}

impl fmt::Display for OutsideJobs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy { name, cpu_percent } => {
                write!(
                    f,
                    "busiest process outside the jobs: {name} at {cpu_percent}% CPU"
                )
            }
            Self::Quiet => f.write_str("no process outside the jobs is using more than 50% CPU"),
        }
    }
}

/// What a client asks the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    /// Asks to start a job, and waits for [`Reply::Granted`].
    Ask(Job),
    /// Asks for the running and waiting jobs.
    Status,
    /// Says that the granted job ended.
    Done {
        /// The job's exit code, or `None` when a signal ended it or it did
        /// not start.
        exit_code: Option<i32>,
        /// The signal that ended the job, or `None` when no signal did. An
        /// older client sends no signal, and an older daemon ignores it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<i32>,
    },
    /// Asks for the jobs that `deadband top` draws.
    Timeline,
    /// Says that a job the previous daemon granted still runs, as the first
    /// line of a new connection. It has no reply. The connection then
    /// behaves as a granted job's.
    Seat {
        /// The job as its client described it.
        job: Job,
        /// The threads of the job's grant.
        jobs: usize,
        /// How long the job has run, in milliseconds.
        ran_ms: u64,
    },
}

/// What the daemon answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    /// The job may start now, with `jobs` threads.
    Granted {
        /// The threads the job may use, which `run` gives the job in its
        /// parallelism variables.
        jobs: usize,
    },
    /// The job waits, and `ahead` jobs start before it.
    Queued {
        /// How many jobs start before this one.
        ahead: usize,
        /// Why the first waiting job does not start yet, or `None` when the
        /// daemon does not know.
        held: Option<Hold>,
    },
    /// The running and waiting jobs.
    Jobs(Jobs),
    /// The jobs that `deadband top` draws.
    Timeline(Timeline),
    /// The watched job has shown no activity. A client ignores any reply it
    /// cannot read.
    Stuck {
        /// How long the job has shown no activity.
        quiet: Ago,
        /// Whether the job was already stuck, so this is a reminder.
        again: bool,
        /// What the daemon knows of the processes outside the jobs, or
        /// `None` when it could not read them.
        busiest: Option<OutsideJobs>,
    },
    /// The watched job that was stuck shows activity again.
    Active {
        /// How long the job showed no activity.
        quiet: Ago,
    },
}

/// The daemon's running and waiting jobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Jobs {
    /// The most jobs that the daemon lets run at once.
    pub max_jobs: usize,
    /// The running jobs, in the order they started.
    pub running: Vec<Job>,
    /// The waiting jobs, the oldest first.
    pub waiting: Vec<Job>,
    /// Why the first waiting job does not start yet, or `null` when no job
    /// waits or the daemon does not know.
    #[serde(default)]
    pub held: Option<Hold>,
    /// The last reclaim, or `null` when the daemon has run none.
    #[serde(default)]
    pub reclaim: Option<Reclaimed>,
    /// The jobs that have shown no activity for the configured time, in the
    /// order they started.
    #[serde(default)]
    pub stuck: Vec<StuckJob>,
    /// The processes outside the jobs while a job is stuck, or `null` when
    /// no job is stuck or the daemon could not read them.
    #[serde(default)]
    pub busiest: Option<OutsideJobs>,
    /// Whether the daemon's latest read of a job's processes failed, so it
    /// cannot say whether that job is stuck.
    #[serde(default)]
    pub unreadable: bool,
}

/// A job that has shown no activity for the configured time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StuckJob {
    /// The job's label.
    pub label: String,
    /// The process ID of the `deadband run` that asked for the job.
    pub pid: u32,
    /// How long the job has shown no activity, in seconds.
    pub quiet_seconds: u64,
}

/// Writes `message` to `writer` as one JSON line and flushes it.
///
/// # Errors
///
/// Returns the error of a write that failed.
pub fn write_line<T: Serialize>(writer: &mut impl Write, message: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(io::Error::other)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

/// Reads one JSON line from `reader`, or returns `None` when the peer closed
/// the connection before it sent a line.
///
/// # Errors
///
/// Returns an error of kind [`io::ErrorKind::InvalidData`] for a line that
/// is not a `T` or is longer than [`MAX_LINE`], and the error of a read
/// that failed, such as a timeout.
pub fn read_line<T: DeserializeOwned>(reader: &mut impl BufRead) -> io::Result<Option<T>> {
    let mut line = Vec::new();
    // One byte past the limit shows that the line is longer than it.
    reader.take(MAX_LINE + 1).read_until(b'\n', &mut line)?;
    if line.is_empty() {
        return Ok(None);
    }
    let too_long = u64::try_from(line.len()).map_or(true, |length| length > MAX_LINE);
    if too_long {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a line is longer than {MAX_LINE} bytes"),
        ));
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}
