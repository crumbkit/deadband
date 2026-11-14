//! The pure model of `deadband top`: what the view draws, computed from
//! the replies and samples it was given and the time it is passed.

use std::cmp::Reverse;
use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use crate::admit::Hold;
use crate::client::Unasked;
use crate::protocol::{OutsideJobs, Timeline};

mod rows;
#[cfg(test)]
mod tests;

pub(super) use self::rows::{Axis, Row, Rows, Status};
use self::rows::{in_window, row};

/// How far back the screen shows.
pub(super) const WINDOW: Duration = Duration::from_secs(600);

/// The most samples of the load and the hold that the view keeps, one per
/// poll, which is the window at one poll a second.
pub(super) const MAX_SAMPLES: usize = 600;

/// The narrowest terminal that shows the timeline.
pub(super) const MIN_COLUMNS: u16 = 80;

/// The shortest terminal that shows the timeline.
pub(super) const MIN_LINES: u16 = 16;

/// How far from the wanted age a sample may be and still answer for it.
/// One poll may take up to half a second longer than its second.
const NEAREST: Duration = Duration::from_millis(1500);

/// What held jobs at one moment, for the strip below the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Gate {
    /// Nothing held a job.
    Open,
    /// The jobs or the lane's places were full.
    Full,
    /// The load held a job.
    Load,
    /// Memory pressure held a job.
    Memory,
    /// The disk held a job.
    Disk,
}

impl From<Option<Hold>> for Gate {
    fn from(hold: Option<Hold>) -> Self {
        match hold {
            None => Self::Open,
            Some(Hold::Full { .. } | Hold::Lane { .. }) => Self::Full,
            Some(Hold::Load { .. }) => Self::Load,
            Some(Hold::Memory { .. }) => Self::Memory,
            Some(Hold::Disk { .. }) => Self::Disk,
        }
    }
}

/// Why no daemon answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Absence {
    /// Nothing listens on the socket.
    NotRunning,
    /// A daemon closed the connection with no reply, as one that knows no
    /// timeline request does.
    MaybeOlder,
}

/// What the screen shows, derived from the latest poll and the terminal's
/// size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Phase {
    /// No reply yet, with the reason when a poll failed.
    Connecting(Option<String>),
    /// The latest poll answered.
    Live,
    /// The latest poll failed and the last reply stays on the screen.
    Stale {
        /// How long since the last answer.
        silent: Duration,
        /// Why the latest poll failed.
        why: String,
    },
    /// No daemon answers, so there are no rows.
    DaemonDown(Absence),
    /// The terminal is under the minimum size.
    TooSmall {
        /// The terminal's columns.
        columns: u16,
        /// The terminal's lines.
        lines: u16,
    },
}

/// One poll's load and hold.
#[derive(Debug)]
struct Sample {
    /// When the poll ran.
    at: Instant,
    /// The 1-minute load, or `None` when the probe failed.
    load: Option<f64>,
    /// What held jobs, or `None` when no daemon answered.
    gate: Option<Gate>,
}

/// The last reply and when it arrived.
#[derive(Debug)]
struct Latest {
    /// The reply.
    timeline: Timeline,
    /// When it arrived.
    at: Instant,
}

/// Why the latest poll failed.
#[derive(Debug)]
enum Failed {
    /// No daemon answers.
    Down(Absence),
    /// A daemon may be there, and its answer did not arrive or read.
    Silent(String),
}

/// Everything the view knows. The jobs are the last reply alone.
#[derive(Debug)]
pub(super) struct Model {
    /// The machine's cores, or `None` when they are unknown.
    cores: Option<NonZeroUsize>,
    /// The load and hold of each poll, the oldest first.
    samples: VecDeque<Sample>,
    /// The last reply, or `None` before the first and while no daemon runs.
    latest: Option<Latest>,
    /// Why the latest poll failed, or `None` when it answered.
    failed: Option<Failed>,
    /// The `up` of the last reply.
    last_up: Option<Duration>,
    /// Whether a restart was seen less than a window ago.
    restarted: bool,
}

impl Model {
    /// Returns a model with nothing observed.
    pub(super) const fn new(cores: Option<NonZeroUsize>) -> Self {
        Self {
            cores,
            samples: VecDeque::new(),
            latest: None,
            failed: None,
            last_up: None,
            restarted: false,
        }
    }

    /// Returns the machine's cores.
    pub(super) const fn cores(&self) -> Option<NonZeroUsize> {
        self.cores
    }

    /// Takes in the poll at `now`: its answer, or why there is none, and
    /// the load that the local probe read.
    pub(super) fn observe(
        &mut self,
        now: Instant,
        polled: Result<Timeline, Unasked>,
        load: Option<f64>,
    ) {
        let gate = match polled {
            Ok(timeline) => {
                self.note_up(timeline.up.duration());
                let gate = Gate::from(timeline.held);
                self.latest = Some(Latest { timeline, at: now });
                self.failed = None;
                Some(gate)
            }
            Err(unasked) => {
                self.fail(&unasked);
                None
            }
        };
        if self.samples.len() == MAX_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(Sample {
            at: now,
            load,
            gate,
        });
    }

    /// Notes a reply's `up`: one smaller than the last shows a restart,
    /// which stays news until the window holds only the new daemon.
    fn note_up(&mut self, up: Duration) {
        if self.last_up.is_some_and(|before| up < before) {
            self.restarted = true;
        }
        if up >= WINDOW {
            self.restarted = false;
        }
        self.last_up = Some(up);
    }

    /// Notes a poll with no answer.
    fn fail(&mut self, unasked: &Unasked) {
        let absence = match unasked {
            Unasked::NotRunning { .. } => Some(Absence::NotRunning),
            Unasked::Closed => Some(Absence::MaybeOlder),
            Unasked::Unreachable { .. }
            | Unasked::Foreign { .. }
            | Unasked::Directory { .. }
            | Unasked::Silent { .. }
            | Unasked::Garbled { .. } => None,
        };
        self.failed = Some(match absence {
            Some(absence) => {
                self.latest = None;
                Failed::Down(absence)
            }
            None => Failed::Silent(unasked.to_string()),
        });
    }

    /// Returns what the screen shows at `now` in a terminal of this size.
    pub(super) fn phase(&self, now: Instant, columns: u16, lines: u16) -> Phase {
        if columns < MIN_COLUMNS || lines < MIN_LINES {
            return Phase::TooSmall { columns, lines };
        }
        match (&self.failed, &self.latest) {
            (Some(Failed::Down(absence)), _) => Phase::DaemonDown(*absence),
            (Some(Failed::Silent(why)), Some(latest)) => Phase::Stale {
                silent: now.saturating_duration_since(latest.at),
                why: why.clone(),
            },
            (Some(Failed::Silent(why)), None) => Phase::Connecting(Some(why.clone())),
            (None, Some(_)) => Phase::Live,
            (None, None) => Phase::Connecting(None),
        }
    }

    /// Returns the rows at `now` for at most `capacity` lines. Stuck jobs
    /// take their lines first, then the other live jobs as the reply lists
    /// them. The stuck jobs are listed first, and the rows then run from the
    /// oldest queued to the newest.
    pub(super) fn rows(&self, now: Instant, axis: Axis, capacity: usize) -> Rows {
        let Some(latest) = &self.latest else {
            return Rows::default();
        };
        let elapsed = now.saturating_duration_since(latest.at);
        let mut in_window: Vec<_> = latest
            .timeline
            .jobs
            .iter()
            .filter(|record| in_window(record, elapsed))
            .collect();
        in_window.sort_by_key(|record| !record.is_stuck());
        let mut chosen: Vec<_> = in_window.iter().take(capacity).copied().collect();
        chosen.sort_by_key(|record| (!record.is_stuck(), Reverse(record.queued.duration())));
        Rows {
            rows: chosen
                .into_iter()
                .map(|record| row(record, elapsed, axis))
                .collect(),
            unseen: in_window
                .len()
                .saturating_sub(capacity)
                .saturating_add(latest.timeline.hidden),
        }
    }

    /// Returns the load about `age` before `now`, or `None` for a gap or a
    /// time before the view opened.
    pub(super) fn load_at(&self, now: Instant, age: Duration) -> Option<f64> {
        self.sample_at(now, age)?.load
    }

    /// Returns what held jobs about `age` before `now`, or `None` for a
    /// time with no answer or before the view opened.
    pub(super) fn gate_at(&self, now: Instant, age: Duration) -> Option<Gate> {
        self.sample_at(now, age)?.gate
    }

    /// Returns the sample nearest to `age` before `now`.
    fn sample_at(&self, now: Instant, age: Duration) -> Option<&Sample> {
        let age_of = |sample: &Sample| now.saturating_duration_since(sample.at);
        if age > age_of(self.samples.front()?) {
            return None;
        }
        let newer = self.samples.partition_point(|sample| age_of(sample) > age);
        let candidates = [
            newer
                .checked_sub(1)
                .and_then(|index| self.samples.get(index)),
            self.samples.get(newer),
        ];
        candidates
            .into_iter()
            .flatten()
            .min_by_key(|sample| age_of(sample).abs_diff(age))
            .filter(|sample| age_of(sample).abs_diff(age) <= NEAREST)
    }

    /// Returns the load of the latest poll.
    pub(super) fn latest_load(&self) -> Option<f64> {
        self.samples.back()?.load
    }

    /// Returns why jobs wait now, as the last reply said.
    pub(super) fn held(&self) -> Option<Hold> {
        self.latest.as_ref()?.timeline.held
    }

    /// Returns how long ago the daemon restarted, while that is news.
    pub(super) fn restarted(&self) -> Option<Duration> {
        let latest = self.latest.as_ref().filter(|_| self.restarted)?;
        Some(latest.timeline.up.duration())
    }

    /// Returns how many jobs are stuck, as the last reply said.
    pub(super) fn stuck_count(&self) -> usize {
        self.latest.as_ref().map_or(0, |latest| {
            latest
                .timeline
                .jobs
                .iter()
                .filter(|record| record.is_stuck())
                .count()
        })
    }

    /// Returns what the last reply said of the processes outside the jobs.
    pub(super) fn busiest(&self) -> Option<&OutsideJobs> {
        self.latest.as_ref()?.timeline.busiest.as_ref()
    }

    /// Reports whether the last reply said that the jobs' processes could
    /// not be read.
    pub(super) fn unreadable(&self) -> bool {
        self.latest
            .as_ref()
            .is_some_and(|latest| latest.timeline.unreadable)
    }

    /// Returns how many samples the model holds.
    #[cfg(test)]
    pub(super) fn sample_count(&self) -> usize {
        self.samples.len()
    }
}
