//! The reclaims that [`Timeline`](super::Timeline) carries for `deadband
//! top`: each configured reclaimer's runs of the last ten minutes, the plan
//! of the reclaim that runs now, and how long until the next one starts.
//! Every text is cut and cleaned as [`Label`] and [`Short`] are, so a
//! reclaimer's output cannot carry a terminal escape into the view.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{Ago, Label, Short};

/// The most items that a [`ReclaimPlan`] names on each side. A plan with
/// more counts the rest in its `more`.
pub const PLAN_ITEMS: usize = 8;

/// How long from the reply until something happens, in whole milliseconds,
/// on the daemon's monotonic clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ahead(u64);

impl Ahead {
    /// Returns the wait that `duration` gives.
    #[must_use]
    pub fn from_duration(duration: Duration) -> Self {
        Self(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
    }

    /// Returns the wait as a duration.
    #[must_use]
    pub const fn duration(self) -> Duration {
        Duration::from_millis(self.0)
    }
}

/// How one run of a reclaimer stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReclaimRunState {
    /// The run has not ended.
    Running,
    /// The run has ended.
    Ended {
        /// When the run ended.
        ended: Ago,
        /// The bytes the reclaimer reported freeing, or `None` when it
        /// reported none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        freed_bytes: Option<u64>,
        /// Why the run failed, or `None` when it did not.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failed: Option<Short>,
    },
}

/// One run of a reclaimer: from its dry run to the end of its real run, or
/// to the end of a dry run that planned to free nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReclaimRun {
    /// The `--need` that the daemon asked for, such as `memory=4G`.
    pub need: Short,
    /// When the run started.
    pub started: Ago,
    /// Whether the run ended and how.
    pub state: ReclaimRunState,
}

/// One configured reclaimer and its runs in the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReclaimerRow {
    /// The name that the config gives the reclaimer.
    pub name: Label,
    /// The reclaimer's program and its subcommand words, such as
    /// `mxv reclaim`.
    pub command: Label,
    /// The runs, the oldest first.
    pub runs: Vec<ReclaimRun>,
}

/// An item that a reclaimer plans to leave as it is, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptItem {
    /// The item's name.
    pub name: Label,
    /// Why the reclaimer keeps it, such as `a job waits`.
    pub why: Short,
}

/// What a reclaimer's dry run says the running reclaim will do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReclaimPlan {
    /// The reclaimer's program and its subcommand words.
    pub command: Label,
    /// The items it will stop, at most [`PLAN_ITEMS`].
    pub stop: Vec<Label>,
    /// The bytes it plans to free.
    pub bytes: u64,
    /// The items it keeps, at most [`PLAN_ITEMS`].
    pub keep: Vec<KeptItem>,
    /// How many items the plan holds beyond those the two lists name.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub more: usize,
}

/// Reports whether `count` is 0, for serde.
const fn is_zero(count: &usize) -> bool {
    *count == 0
}
