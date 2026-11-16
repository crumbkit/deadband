//! What the daemon remembers of its reclaims for `deadband top`: each
//! reclaimer's runs of the last ten minutes, and the plan of the reclaim
//! that runs now. The list lives in memory only, so a restart forgets it.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::job::CommandLine;
use crate::protocol::{
    Ago, KeptItem, Label, PLAN_ITEMS, ReclaimPlan, ReclaimRun, ReclaimRunState, ReclaimerRow, Short,
};
use crate::reclaim::{Config, Reclaimed, Reclaimer, Report};

/// How long a run stays in the list: the window that `deadband top` shows.
pub(super) const REMEMBER: Duration = Duration::from_secs(10 * 60);

/// The most runs that the daemon remembers. The 65th evicts the oldest.
pub(super) const MAX_RUNS: usize = 64;

/// The most subcommand words that a reclaimer's label keeps after its
/// program.
const LABEL_WORDS: usize = 2;

/// One run of one reclaimer that has ended.
#[derive(Debug)]
struct Run {
    /// The reclaimer's place in the config's list.
    reclaimer: usize,
    /// The `--need` of the run.
    need: String,
    /// When the run started.
    started: Instant,
    /// When the run ended.
    ended: Instant,
    /// The bytes the reclaimer reported freeing.
    freed_bytes: Option<u64>,
    /// Why the run failed.
    failed: Option<String>,
}

/// The reclaim that runs now.
#[derive(Debug)]
struct Begun {
    /// The `--need` of the reclaim.
    need: String,
    /// When the reclaimer that runs now started: the start of the reclaim,
    /// or the end of the reclaimer before it.
    since: Instant,
    /// How many reclaimers have ended their runs. The daemon asks the
    /// config's reclaimers in order, so this is the place of the one that
    /// runs now.
    done: usize,
}

/// The daemon's reclaim history.
#[derive(Debug, Default)]
pub(super) struct Reclaims {
    /// The runs that ended, the oldest first.
    runs: VecDeque<Run>,
    /// The reclaim that runs now.
    begun: Option<Begun>,
    /// The plan of the reclaimer that runs now.
    plan: Option<ReclaimPlan>,
}

/// Returns the reclaimer's program and its first plain subcommand words,
/// such as `mxv reclaim` for `/usr/local/bin/mxv reclaim --json`. The
/// config is the owner's, so its words are not secrets as a job's may be.
pub(super) fn command_label(command: &CommandLine) -> String {
    let program = command
        .program
        .rsplit('/')
        .next()
        .unwrap_or(&command.program);
    let mut label = program.to_owned();
    let words = command
        .args
        .iter()
        .take_while(|word| plain(word))
        .take(LABEL_WORDS);
    for word in words {
        label.push(' ');
        label.push_str(word);
    }
    label
}

/// Reports whether `word` is a lowercase word, not an option or a value.
fn plain(word: &str) -> bool {
    word.starts_with(|first: char| first.is_ascii_lowercase())
        && word
            .chars()
            .all(|each| each.is_ascii_lowercase() || each.is_ascii_digit() || each == '-')
}

impl Reclaims {
    /// Notes that a reclaim of `need` begins at `now`.
    pub(super) fn begin(&mut self, need: String, now: Instant) {
        self.begun = Some(Begun {
            need,
            since: now,
            done: 0,
        });
        self.plan = None;
    }

    /// Notes that the reclaim ended, or never ran.
    pub(super) fn finish(&mut self) {
        self.begun = None;
        self.plan = None;
    }

    /// Notes the plan that `reclaimer`'s dry run printed in `report`.
    pub(super) fn planned(&mut self, reclaimer: &Reclaimer, report: &Report) {
        if self.begun.is_none() {
            return;
        }
        let named = |names: usize| names.saturating_sub(PLAN_ITEMS);
        let keep: Vec<_> = report
            .kept
            .iter()
            .filter(|kept| !kept.name.is_empty())
            .collect();
        self.plan = Some(ReclaimPlan {
            command: Label::new(&command_label(&reclaimer.command)),
            stop: report
                .items
                .iter()
                .take(PLAN_ITEMS)
                .map(|name| Label::new(name))
                .collect(),
            bytes: report.freed_bytes,
            keep: keep
                .iter()
                .take(PLAN_ITEMS)
                .map(|kept| KeptItem {
                    name: Label::new(&kept.name),
                    why: Short::new(&kept.why),
                })
                .collect(),
            more: named(report.items.len()).saturating_add(named(keep.len())),
        });
    }

    /// Notes that the reclaimer that ran now ended its run at `now` with
    /// the outcome `reclaimed`, and drops the runs the list no longer
    /// keeps.
    pub(super) fn ended(&mut self, reclaimed: &Reclaimed, now: Instant) {
        let Some(begun) = &mut self.begun else {
            return;
        };
        self.runs.push_back(Run {
            reclaimer: begun.done,
            need: reclaimed.need.clone(),
            started: begun.since,
            ended: now,
            freed_bytes: reclaimed.freed_bytes,
            failed: reclaimed.failed.clone(),
        });
        begun.done = begun.done.saturating_add(1);
        begun.since = now;
        self.plan = None;
        while self.runs.len() > MAX_RUNS
            || self
                .runs
                .front()
                .is_some_and(|run| now.saturating_duration_since(run.ended) >= REMEMBER)
        {
            self.runs.pop_front();
        }
    }

    /// Returns the plan of the reclaim that runs now.
    pub(super) fn plan(&self) -> Option<ReclaimPlan> {
        self.begun.as_ref().and(self.plan.clone())
    }

    /// Returns each reclaimer of `config` with its runs at `now`, or
    /// nothing when no reclaim ended in the window and none runs now.
    pub(super) fn rows(&self, config: &Config, now: Instant) -> Vec<ReclaimerRow> {
        let recent = |run: &&Run| now.saturating_duration_since(run.ended) < REMEMBER;
        if self.begun.is_none() && !self.runs.iter().any(|run| recent(&run)) {
            return Vec::new();
        }
        config
            .reclaimers
            .iter()
            .enumerate()
            .map(|(place, reclaimer)| {
                let ended = self
                    .runs
                    .iter()
                    .filter(recent)
                    .filter(|run| run.reclaimer == place)
                    .map(|run| ReclaimRun {
                        need: Short::new(&run.need),
                        started: Ago::between(run.started, now),
                        state: ReclaimRunState::Ended {
                            ended: Ago::between(run.ended, now),
                            freed_bytes: run.freed_bytes,
                            failed: run.failed.as_deref().map(Short::new),
                        },
                    });
                let running =
                    self.begun
                        .as_ref()
                        .filter(|begun| begun.done == place)
                        .map(|begun| ReclaimRun {
                            need: Short::new(&begun.need),
                            started: Ago::between(begun.since, now),
                            state: ReclaimRunState::Running,
                        });
                ReclaimerRow {
                    name: Label::new(&reclaimer.name),
                    command: Label::new(&command_label(&reclaimer.command)),
                    runs: ended.chain(running).collect(),
                }
            })
            .collect()
    }
}
