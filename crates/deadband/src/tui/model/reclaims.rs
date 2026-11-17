//! The reclaims of `deadband top`: one row for each configured reclaimer
//! with its runs on the time axis, what the screen says of a reclaim that
//! runs or failed, and when the next one starts.

use std::ops::Range;
use std::time::Duration;

use super::Axis;
use crate::protocol::{ReclaimRun, ReclaimRunState, ReclaimerRow, Timeline};

/// How a run ends on the axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::tui) enum Ending {
    /// The run has not ended.
    Open,
    /// The run ended without a failure.
    Done,
    /// The run failed.
    Failed,
}

/// One run of a reclaimer on the axis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) struct Bar {
    /// The columns of the run, which are none for a run that ended in the
    /// column it started in.
    pub(in crate::tui) columns: Range<u16>,
    /// The column of the sign after the run's end, with how it ended.
    pub(in crate::tui) end: Option<(u16, Ending)>,
}

/// The text at the right of a reclaimer's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::tui) enum Standing {
    /// The reclaimer ran no run in the window.
    Idle,
    /// The reclaimer's latest run started this long ago and has not ended.
    Running(Duration),
    /// The reclaimer's latest run ended this long ago, with the bytes the
    /// reclaimer reported freeing.
    Freed {
        /// The bytes reported, or `None` when it reported none.
        bytes: Option<u64>,
        /// How long ago the run ended.
        ago: Duration,
    },
    /// The reclaimer's latest run failed this long ago.
    Failed(Duration),
}

/// One reclaimer's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) struct ReclaimRow {
    /// The reclaimer's program and subcommand words, which the workspace
    /// column shows.
    pub(in crate::tui) command: String,
    /// The reclaimer's name, which the job column shows.
    pub(in crate::tui) name: String,
    /// The `--need` of its latest run, or empty.
    pub(in crate::tui) need: String,
    /// The runs in the window, the oldest first.
    pub(in crate::tui) bars: Vec<Bar>,
    /// The text at the right.
    pub(in crate::tui) standing: Standing,
}

/// What the screen says of the reclaims as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tui) enum Reclaim {
    /// No reclaim runs, and no reclaimer's latest run failed.
    Quiet,
    /// A reclaim runs.
    Running {
        /// The program and subcommand words of the reclaimer that runs.
        command: String,
        /// The `--need` of the reclaim.
        need: String,
        /// How long ago the reclaimer's run started.
        since: Duration,
    },
    /// No reclaim runs, and the latest run of some reclaimer failed.
    Failed {
        /// The program and subcommand words of the reclaimer that failed
        /// most lately.
        command: String,
        /// How many of its runs in a row, ending with the latest, failed.
        times: usize,
        /// Why its latest run failed.
        why: String,
    },
}

/// Returns `run`'s start and end as ages `elapsed` after the reply, and
/// how it ended.
fn ages(run: &ReclaimRun, elapsed: Duration) -> (Duration, Option<Duration>, Ending) {
    let started = run.started.duration() + elapsed;
    match &run.state {
        ReclaimRunState::Running => (started, None, Ending::Open),
        ReclaimRunState::Ended { ended, failed, .. } => (
            started,
            Some(ended.duration() + elapsed),
            if failed.is_some() {
                Ending::Failed
            } else {
                Ending::Done
            },
        ),
    }
}

/// Returns the bar of `run`, `elapsed` after the reply, or `None` when the
/// run ended before the window.
fn bar(run: &ReclaimRun, elapsed: Duration, axis: Axis) -> Option<Bar> {
    let (started, ended, ending) = ages(run, elapsed);
    let now = axis.column(Duration::ZERO);
    let end = ended.map_or(now, |ended| axis.column(ended));
    if end < 0 {
        return None;
    }
    let first = axis.column(started);
    Some(match ended {
        None => Bar {
            columns: axis.span(first, end),
            end: None,
        },
        Some(_) => Bar {
            columns: axis.span(first, end - 1),
            end: u16::try_from(end).ok().map(|column| (column, ending)),
        },
    })
}

/// Returns what the latest of `runs` says at the right of a row.
fn standing(runs: &[ReclaimRun], elapsed: Duration) -> Standing {
    let Some(last) = runs.last() else {
        return Standing::Idle;
    };
    match &last.state {
        ReclaimRunState::Running => Standing::Running(last.started.duration() + elapsed),
        ReclaimRunState::Ended {
            ended,
            failed: Some(_),
            ..
        } => Standing::Failed(ended.duration() + elapsed),
        ReclaimRunState::Ended {
            ended, freed_bytes, ..
        } => Standing::Freed {
            bytes: *freed_bytes,
            ago: ended.duration() + elapsed,
        },
    }
}

/// Returns the row of `reclaimer`, `elapsed` after the reply.
fn row(reclaimer: &ReclaimerRow, elapsed: Duration, axis: Axis) -> ReclaimRow {
    ReclaimRow {
        command: reclaimer.command.as_str().to_owned(),
        name: reclaimer.name.as_str().to_owned(),
        need: reclaimer
            .runs
            .last()
            .map(|run| run.need.as_str().to_owned())
            .unwrap_or_default(),
        bars: reclaimer
            .runs
            .iter()
            .filter_map(|run| bar(run, elapsed, axis))
            .collect(),
        standing: standing(&reclaimer.runs, elapsed),
    }
}

/// Returns the rows of the reclaimers in `timeline`, `elapsed` after the
/// reply.
pub(super) fn rows(timeline: &Timeline, elapsed: Duration, axis: Axis) -> Vec<ReclaimRow> {
    timeline
        .reclaimers
        .iter()
        .map(|reclaimer| row(reclaimer, elapsed, axis))
        .collect()
}

/// Returns what the screen says of the reclaims of `timeline`, `elapsed`
/// after the reply.
pub(super) fn state(timeline: &Timeline, elapsed: Duration) -> Reclaim {
    let running = timeline.reclaimers.iter().find_map(|reclaimer| {
        let run = reclaimer
            .runs
            .iter()
            .find(|run| run.state == ReclaimRunState::Running)?;
        Some(Reclaim::Running {
            command: reclaimer.command.as_str().to_owned(),
            need: run.need.as_str().to_owned(),
            since: run.started.duration() + elapsed,
        })
    });
    running
        .or_else(|| failed(timeline))
        .unwrap_or(Reclaim::Quiet)
}

/// Returns the reclaimer whose latest run failed most lately, with its
/// failures in a row.
fn failed(timeline: &Timeline) -> Option<Reclaim> {
    timeline
        .reclaimers
        .iter()
        .filter_map(|reclaimer| {
            let Some(ReclaimRun {
                state:
                    ReclaimRunState::Ended {
                        ended,
                        failed: Some(why),
                        ..
                    },
                ..
            }) = reclaimer.runs.last()
            else {
                return None;
            };
            let times = reclaimer
                .runs
                .iter()
                .rev()
                .take_while(|run| {
                    matches!(
                        run.state,
                        ReclaimRunState::Ended {
                            failed: Some(_),
                            ..
                        }
                    )
                })
                .count();
            Some((
                ended.duration(),
                Reclaim::Failed {
                    command: reclaimer.command.as_str().to_owned(),
                    times,
                    why: why.as_str().to_owned(),
                },
            ))
        })
        .min_by_key(|(ended, _)| *ended)
        .map(|(_, failed)| failed)
}
