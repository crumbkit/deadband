//! The reply to [`Request::Timeline`](super::Request::Timeline), which
//! `deadband top` draws. Every time in it is an age, so client and daemon
//! share no clock and a clock change has no effect.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::admit::{Hold, Lane};

/// The most bytes that a [`Label`] holds.
pub const LABEL_BYTES: usize = 64;

/// A job's label as `deadband top` shows it: at most [`LABEL_BYTES`] bytes,
/// cut on a character boundary, with each control character replaced by a
/// space. Deserializing runs the same constructor, so a label cannot carry
/// a terminal escape into the view, even from a bad daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String")]
pub struct Label(String);

impl Label {
    /// Returns the label for `text`.
    #[must_use]
    pub fn new(text: &str) -> Self {
        let mut label = String::new();
        for each in text.chars() {
            if label.len() + each.len_utf8() > LABEL_BYTES {
                break;
            }
            label.push(if each.is_control() { ' ' } else { each });
        }
        Self(label)
    }

    /// Returns the label's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Label {
    fn from(text: String) -> Self {
        Self::new(&text)
    }
}

/// How long before the reply something happened, in whole milliseconds, on
/// the daemon's monotonic clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ago(u64);

impl Ago {
    /// Returns the age at `now` of something that happened at `at`.
    #[must_use]
    pub fn between(at: Instant, now: Instant) -> Self {
        let millis = now.saturating_duration_since(at).as_millis();
        Self(u64::try_from(millis).unwrap_or(u64::MAX))
    }

    /// Returns the age of something that happened `millis` milliseconds ago.
    #[cfg(test)]
    pub(crate) const fn from_millis(millis: u64) -> Self {
        Self(millis)
    }

    /// Returns the age as a duration.
    #[must_use]
    pub const fn duration(self) -> Duration {
        Duration::from_millis(self.0)
    }
}

/// How a job that started ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exit {
    /// The job exited with `code`.
    Exited {
        /// The job's exit code.
        code: i32,
    },
    /// A signal ended the job.
    Signalled {
        /// The signal's number.
        signal: i32,
    },
    /// The daemon never learned how the job ended.
    Lost,
}

impl Exit {
    /// Returns the end that a client's `Done` reports: the exit code, or
    /// the signal, or [`Exit::Lost`] when it gives neither.
    #[must_use]
    pub const fn reported(exit_code: Option<i32>, signal: Option<i32>) -> Self {
        match (exit_code, signal) {
            (Some(code), _) => Self::Exited { code },
            (None, Some(signal)) => Self::Signalled { signal },
            (None, None) => Self::Lost,
        }
    }
}

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// The job waits for the daemon to admit it.
    Waiting,
    /// The job runs.
    Running {
        /// When the job started.
        started: Ago,
    },
    /// The job started and has ended.
    Finished {
        /// When the job started.
        started: Ago,
        /// When the job ended.
        ended: Ago,
        /// How the job ended.
        exit: Exit,
    },
    /// The job's client went away before the job started.
    Left {
        /// When the client went away.
        ended: Ago,
    },
}

/// One job on the timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecord {
    /// The job's label.
    pub label: Label,
    /// The lane the job waited in.
    pub lane: Lane,
    /// When the job asked to start.
    pub queued: Ago,
    /// Where the job is in its life.
    pub state: JobState,
}

/// What `deadband top` asks the daemon for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    /// When the daemon started. A value smaller than the one before shows
    /// that the daemon restarted.
    pub up: Ago,
    /// At most 100 jobs: the running ones, then the waiting ones, then the
    /// ended ones, the newest first.
    pub jobs: Vec<JobRecord>,
    /// How many more jobs the daemon knows and left out of `jobs`.
    pub hidden: usize,
    /// Why the first waiting job does not start yet, or `null` when no job
    /// waits or the daemon does not know.
    pub held: Option<Hold>,
}
