//! The reply to [`Request::Timeline`](super::Request::Timeline), which
//! `deadband top` draws. Every time in it is an age, so client and daemon
//! share no clock and a clock change has no effect.

use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{MAX_LINE, OutsideJobs};
use crate::admit::{Hold, Lane};

/// The most bytes that a [`Label`] holds.
pub const LABEL_BYTES: usize = 64;

/// The most bytes that a [`Short`] holds: three words of at most 32 bytes
/// each and the two spaces between them.
pub const SHORT_BYTES: usize = 98;

/// Returns `text` cut to at most `most` bytes on a character boundary, with
/// each control character replaced by a space.
fn clean(text: &str, most: usize) -> String {
    let mut kept = String::new();
    for each in text.chars() {
        if kept.len() + each.len_utf8() > most {
            break;
        }
        kept.push(if each.is_control() { ' ' } else { each });
    }
    kept
}

/// A job's label or workspace as `deadband status` and `deadband top` show
/// it: at most [`LABEL_BYTES`] bytes, cut on a character boundary, with each
/// control character replaced by a space. Deserializing runs the same
/// constructor, so a label cannot carry a terminal escape into the view,
/// even from a bad daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(from = "String")]
pub struct Label(String);

impl Label {
    /// Returns the label for `text`.
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self(clean(text, LABEL_BYTES))
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

/// A job's short command: the kind that the history module's rule gives
/// its command line, such as `cargo nextest run`, at most [`SHORT_BYTES`]
/// bytes. The daemon computes it, and the full command line never reaches
/// a display through it. Deserializing cleans the text as [`Label`] does.
/// An empty short command says that the rule kept no word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(from = "String")]
pub struct Short(String);

impl Short {
    /// Returns the short command for `text`.
    #[must_use]
    pub fn new(text: &str) -> Self {
        Self(clean(text, SHORT_BYTES))
    }

    /// Returns the short command's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Short {
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

    /// Returns the age that `duration` gives.
    #[must_use]
    pub fn from_duration(duration: Duration) -> Self {
        Self(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
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
    /// The workspace the job runs in, or `None` outside a workspace or from
    /// an older daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Label>,
    /// The job's short command, or `None` from an older daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short: Option<Short>,
    /// The lane the job waited in.
    pub lane: Lane,
    /// When the job asked to start.
    pub queued: Ago,
    /// Where the job is in its life.
    pub state: JobState,
    /// The job's last four periods with no activity, the oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stuck: Vec<StuckPeriod>,
}

/// One span of time in which a job showed no activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StuckPeriod {
    /// The job's last activity before the period.
    pub since: Ago,
    /// When activity came back, or `None` while the job stays stuck.
    pub until: Option<Ago>,
}

impl JobRecord {
    /// Reports whether the job is stuck now.
    #[must_use]
    pub fn is_stuck(&self) -> bool {
        matches!(self.state, JobState::Running { .. })
            && self
                .stuck
                .last()
                .is_some_and(|period| period.until.is_none())
    }
}

impl Timeline {
    /// Returns the timeline with its last rows, the oldest ended jobs, left
    /// out and counted in `hidden` until its reply fits in one line of
    /// [`MAX_LINE`] bytes.
    ///
    /// A row holds at most about 800 bytes of JSON, so 100 rows fit unless
    /// most of their labels and workspaces are long and full of characters
    /// that JSON escapes. Only then does a row leave, and each one costs a
    /// new serialization of the reply.
    #[must_use]
    pub fn fit(mut self) -> Self {
        // The reply wraps the timeline as `{"timeline":...}`, and its line
        // ends in a newline.
        let room = usize::try_from(MAX_LINE)
            .unwrap_or(usize::MAX)
            .saturating_sub(r#"{"timeline":}"#.len() + 1);
        while serde_json::to_vec(&self).map_or(0, |bytes| bytes.len()) > room
            && self.jobs.pop().is_some()
        {
            self.hidden = self.hidden.saturating_add(1);
        }
        self
    }
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
    /// The processes outside the jobs while a job is stuck, or `null` when
    /// no job is stuck or the daemon could not read them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busiest: Option<OutsideJobs>,
    /// Whether the daemon's latest read of a job's processes failed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unreadable: bool,
}
