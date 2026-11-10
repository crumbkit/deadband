//! The rows of `deadband top` and the time axis they lie on.

use std::ops::Range;
use std::time::Duration;

use super::WINDOW;
use crate::admit::Lane;
use crate::name::shows_label;
use crate::protocol::{Ago, Exit, JobRecord, JobState, Label, Short};

/// The time axis, `width` columns wide, whose last column is now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::tui) struct Axis(u16);

impl Axis {
    /// Returns the axis of `width` columns.
    pub(in crate::tui) const fn new(width: u16) -> Self {
        Self(width)
    }

    /// Returns the column of something `age` old, which is negative for
    /// an age before the window.
    pub(in crate::tui) fn column(self, age: Duration) -> i64 {
        let columns = age.as_millis().saturating_mul(u128::from(self.0)) / WINDOW.as_millis();
        i64::from(self.0) - 1 - i64::try_from(columns).unwrap_or(i64::MAX)
    }

    /// Returns the columns from `first` to `last`, both included, clipped
    /// to the axis.
    fn span(self, first: i64, last: i64) -> Range<u16> {
        let width = i64::from(self.0);
        let start = first.clamp(0, width);
        let end = last.saturating_add(1).clamp(start, width);
        let column = |value| u16::try_from(value).unwrap_or(self.0);
        column(start)..column(end)
    }
}

/// The text at the right of a row, with the durations it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::tui) enum Status {
    /// The job has waited this long.
    Waiting(Duration),
    /// The job has run this long.
    Running(Duration),
    /// The job ended.
    Finished {
        /// How the job ended.
        exit: Exit,
        /// How long the job ran.
        ran: Duration,
    },
    /// The job has shown no activity for this long.
    Stuck(Duration),
    /// The job's client left.
    Left {
        /// How long the job waited.
        waited: Duration,
    },
}

/// One job's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) struct Row {
    /// The workspace the job runs in, or empty outside a workspace or from
    /// an older daemon.
    pub(in crate::tui) workspace: String,
    /// The job's label.
    pub(in crate::tui) label: String,
    /// Whether the job column shows the label, which the name rule leaves
    /// out when it adds nothing.
    pub(in crate::tui) shows_label: bool,
    /// The job's short command, or empty.
    pub(in crate::tui) short: String,
    /// Whether the job is in the fast lane.
    pub(in crate::tui) fast: bool,
    /// The columns where the job waited.
    pub(in crate::tui) waited: Range<u16>,
    /// The columns where the job ran.
    pub(in crate::tui) ran: Range<u16>,
    /// The column after the job's end, which may be one past the axis. The
    /// view draws the sign that the status gives.
    pub(in crate::tui) mark: Option<u16>,
    /// The text at the right.
    pub(in crate::tui) status: Status,
    /// The columns of each period with no activity.
    pub(in crate::tui) stuck: Vec<Range<u16>>,
}

/// The rows that fit on the screen.
#[derive(Debug, Default)]
pub(in crate::tui) struct Rows {
    /// The rows, the oldest queued first.
    pub(in crate::tui) rows: Vec<Row>,
    /// How many jobs are in the window and have no row.
    pub(in crate::tui) unseen: usize,
}

/// Reports whether `record`, `elapsed` after its reply, shows in the
/// window. A live job shows however old it is.
pub(super) fn in_window(record: &JobRecord, elapsed: Duration) -> bool {
    match record.state {
        JobState::Waiting | JobState::Running { .. } => true,
        JobState::Finished { ended, .. } | JobState::Left { ended } => {
            ended.duration() + elapsed < WINDOW
        }
    }
}

/// Returns the row of `record`, `elapsed` after its reply.
pub(super) fn row(record: &JobRecord, elapsed: Duration, axis: Axis) -> Row {
    let age = |ago: Ago| ago.duration() + elapsed;
    let queued = age(record.queued);
    let from = axis.column(queued);
    let before = |at: Duration| axis.span(from, axis.column(at) - 1);
    let mark = |at: Duration| u16::try_from(axis.column(at).saturating_add(1)).ok();
    let (waited, ran, mark, status) = match record.state {
        JobState::Waiting => (
            axis.span(from, axis.column(Duration::ZERO)),
            0..0,
            None,
            Status::Waiting(queued),
        ),
        JobState::Running { started } => {
            let started = age(started);
            let ran = axis.span(axis.column(started), axis.column(Duration::ZERO));
            (before(started), ran, None, Status::Running(started))
        }
        JobState::Finished {
            started,
            ended,
            exit,
        } => {
            let (started, ended) = (age(started), age(ended));
            let ran = axis.span(axis.column(started), axis.column(ended));
            let status = Status::Finished {
                exit,
                ran: started.saturating_sub(ended),
            };
            (before(started), ran, mark(ended), status)
        }
        JobState::Left { ended } => {
            let ended = age(ended);
            let waited = axis.span(from, axis.column(ended));
            let status = Status::Left {
                waited: queued.saturating_sub(ended),
            };
            (waited, 0..0, mark(ended), status)
        }
    };
    let stuck = record
        .stuck
        .iter()
        .map(|period| {
            let last = period.until.map_or_else(
                || axis.column(Duration::ZERO),
                |until| axis.column(age(until)) - 1,
            );
            axis.span(axis.column(age(period.since)), last)
        })
        .filter(|columns| !columns.is_empty())
        .collect();
    let status = match (status, record.stuck.last()) {
        (Status::Running(_), Some(period)) if record.is_stuck() => Status::Stuck(age(period.since)),
        (status, _) => status,
    };
    let workspace = record.workspace.as_ref().map(Label::as_str);
    let short = record.short.as_ref().map(Short::as_str);
    Row {
        workspace: workspace.unwrap_or_default().to_owned(),
        label: record.label.as_str().to_owned(),
        shows_label: shows_label(workspace, record.label.as_str(), short),
        short: short.unwrap_or_default().to_owned(),
        fast: record.lane == Lane::Fast,
        waited,
        ran,
        mark,
        status,
        stuck,
    }
}
