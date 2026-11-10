//! Decides when a job counts as stuck, and reads the `stuck_after`,
//! `stuck_ignore` and `on_stuck` keys of the daemon's config file.
//!
//! The rule reads no clock and no process, so the tests check each case
//! directly. [`Track`] keeps one job's last reading of each process and the
//! time of its last activity. The daemon's watch thread feeds it.
//!
//! Activity is any of three things since the last activity: one second of
//! CPU accumulated across the job's process tree, one mebibyte of disk read
//! or written accumulated, or any change in the tree's set of process IDs.
//! The rule adds only the positive change of each process, so a child that
//! exits cannot lower the total.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use regex_lite::Regex;

use crate::probe::ProcessUse;
use crate::protocol::OutsideJobs;

#[cfg(test)]
mod tests;

/// The CPU time, accumulated across a job's tree, that counts as activity.
pub const CPU_ACTIVITY: Duration = Duration::from_secs(1);

/// The bytes read or written, accumulated across a job's tree, that count
/// as activity.
pub const DISK_ACTIVITY: u64 = 1024 * 1024;

/// How long a job must show no activity before it counts as stuck, when the
/// config sets no `stuck_after`.
pub const DEFAULT_AFTER: Duration = Duration::from_secs(5 * 60);

/// The shortest `stuck_after` that the config file accepts.
pub const MIN_AFTER: Duration = Duration::from_secs(60);

/// How often a job that stays stuck tells its caller so again.
pub const REMIND_EVERY: Duration = Duration::from_secs(10 * 60);

/// The most stuck periods that a job keeps. A fifth evicts the oldest.
pub const MAX_PERIODS: usize = 4;

/// The share of one core, in percent, at which the busiest process outside
/// the jobs is named.
pub const BUSY_PERCENT: u32 = 50;

/// One span of time in which a job showed no activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    /// The job's last activity before the period.
    pub since: Instant,
    /// When activity came back, or `None` while the job stays stuck.
    pub until: Option<Instant>,
}

/// What one reading of a job's tree changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The job has become stuck after `quiet` with no activity.
    Stuck {
        /// How long the job has shown no activity.
        quiet: Duration,
    },
    /// The job stays stuck, and its reminder is due.
    StillStuck {
        /// How long the job has shown no activity.
        quiet: Duration,
    },
    /// The stuck job shows activity again after `quiet`.
    Active {
        /// How long the job showed no activity.
        quiet: Duration,
    },
}

/// What the rule remembers of one watched job.
#[derive(Debug, Clone)]
pub struct Track {
    /// The latest CPU time and disk bytes of each process, or `None` before
    /// the first reading.
    last: Option<BTreeMap<u32, (Duration, u64)>>,
    /// The CPU time accumulated since the last activity.
    cpu: Duration,
    /// The disk bytes accumulated since the last activity.
    disk: u64,
    /// When the job last showed activity.
    activity: Instant,
    /// When the job's caller was last told that it is stuck, while it is.
    noticed: Option<Instant>,
    /// The job's stuck periods, the oldest first.
    periods: VecDeque<Period>,
}

impl Track {
    /// Returns the track of a job first watched at `now`.
    #[must_use]
    pub const fn new(now: Instant) -> Self {
        Self {
            last: None,
            cpu: Duration::ZERO,
            disk: 0,
            activity: now,
            noticed: None,
            periods: VecDeque::new(),
        }
    }

    /// Takes in one reading of the job's tree at `now`, and returns what it
    /// changed. A job counts as stuck once it has shown no activity for
    /// `after`. The first reading only sets the baseline.
    pub fn observe(
        &mut self,
        tree: &[ProcessUse],
        after: Duration,
        now: Instant,
    ) -> Option<Change> {
        let current: BTreeMap<_, _> = tree
            .iter()
            .map(|each| (each.pid, (each.cpu, each.disk_bytes)))
            .collect();
        let previous = self.last.replace(current.clone())?;
        for (pid, (cpu, disk)) in &current {
            if let Some((cpu_before, disk_before)) = previous.get(pid) {
                self.cpu = self.cpu.saturating_add(cpu.saturating_sub(*cpu_before));
                self.disk = self.disk.saturating_add(disk.saturating_sub(*disk_before));
            }
        }
        let changed = !current.keys().eq(previous.keys());
        if changed || self.cpu >= CPU_ACTIVITY || self.disk >= DISK_ACTIVITY {
            return self.active(now);
        }
        let quiet = now.saturating_duration_since(self.activity);
        match self.noticed {
            None if quiet >= after => {
                self.noticed = Some(now);
                if self.periods.len() == MAX_PERIODS {
                    self.periods.pop_front();
                }
                self.periods.push_back(Period {
                    since: self.activity,
                    until: None,
                });
                Some(Change::Stuck { quiet })
            }
            Some(noticed) if now.saturating_duration_since(noticed) >= REMIND_EVERY => {
                self.noticed = Some(now);
                Some(Change::StillStuck { quiet })
            }
            _ => None,
        }
    }

    /// Records activity at `now`, and returns the change when the job was
    /// stuck.
    fn active(&mut self, now: Instant) -> Option<Change> {
        let quiet = now.saturating_duration_since(self.activity);
        self.cpu = Duration::ZERO;
        self.disk = 0;
        self.activity = now;
        self.noticed.take()?;
        if let Some(period) = self.periods.back_mut() {
            period.until = Some(now);
        }
        Some(Change::Active { quiet })
    }

    /// Returns how long the job has shown no activity at `now`, while it is
    /// stuck.
    #[must_use]
    pub fn quiet(&self, now: Instant) -> Option<Duration> {
        self.noticed
            .map(|_| now.saturating_duration_since(self.activity))
    }

    /// Returns the job's stuck periods, the oldest first.
    #[must_use]
    pub fn periods(&self) -> Vec<Period> {
        self.periods.iter().copied().collect()
    }
}

/// Returns `quiet` as the status and the standard error lines show it:
/// seconds under a minute, whole minutes under an hour, and hours and minutes
/// from there.
#[must_use]
pub fn quiet_text(quiet: Duration) -> String {
    let seconds = quiet.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        _ => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
    }
}

/// Returns `line`, followed by what is known of the processes outside the
/// jobs.
fn with_outside(line: String, busiest: Option<&OutsideJobs>) -> String {
    match busiest {
        Some(outside) => format!("{line}; {}", outside.clause()),
        None => line,
    }
}

/// Returns the line that `deadband run` prints when its job becomes stuck.
#[must_use]
pub fn stuck_line(quiet: Duration, busiest: Option<&OutsideJobs>) -> String {
    let line = format!(
        "the job has shown no CPU use, new process or disk use for {}; deadband leaves it running",
        quiet_text(quiet)
    );
    with_outside(line, busiest)
}

/// Returns the line that `deadband run` prints while its job stays stuck.
#[must_use]
pub fn still_line(quiet: Duration, busiest: Option<&OutsideJobs>) -> String {
    let line = format!(
        "the job has still shown no activity after {}",
        quiet_text(quiet)
    );
    with_outside(line, busiest)
}

/// Returns the line that `deadband run` prints when its job is active again.
#[must_use]
pub fn active_line(quiet: Duration) -> String {
    format!(
        "the job is active again after {} with no activity",
        quiet_text(quiet)
    )
}

/// What the daemon reads from the stuck keys of its config file.
#[derive(Debug, Clone)]
pub struct Config {
    /// How long a job must show no activity to count as stuck, or `None`
    /// when `stuck_after` is `"off"`.
    pub after: Option<Duration>,
    /// The expressions of `stuck_ignore`. A job whose command line matches
    /// any of them is not watched.
    pub ignore: Vec<Regex>,
    /// The command of `on_stuck`, which is empty when the config names none.
    pub on_stuck: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            after: Some(DEFAULT_AFTER),
            ignore: Vec::new(),
            on_stuck: Vec::new(),
        }
    }
}

impl PartialEq for Config {
    fn eq(&self, other: &Self) -> bool {
        let patterns = |config: &Self| {
            config
                .ignore
                .iter()
                .map(|each| each.as_str().to_owned())
                .collect::<Vec<_>>()
        };
        self.after == other.after
            && self.on_stuck == other.on_stuck
            && patterns(self) == patterns(other)
    }
}

impl Eq for Config {}

impl Config {
    /// Reads the values of the three keys.
    ///
    /// # Errors
    ///
    /// Returns, in one line, why a value is invalid: a `stuck_after` that
    /// is neither `"off"` nor an age of at least [`MIN_AFTER`], a
    /// `stuck_ignore` expression that does not parse, or an empty `on_stuck`.
    pub fn parse(
        after: Option<&str>,
        ignore: &[String],
        on_stuck: Option<Vec<String>>,
    ) -> Result<Self, String> {
        let after = match after {
            None => Some(DEFAULT_AFTER),
            Some("off") => None,
            Some(text) => {
                let age = humantime::parse_duration(text).map_err(|error| {
                    format!("stuck_after {text:?} is not an age such as \"5m\": {error}")
                })?;
                if age < MIN_AFTER {
                    return Err(format!(
                        "stuck_after is {text}, under the shortest it accepts, 1m"
                    ));
                }
                Some(age)
            }
        };
        let ignore = ignore
            .iter()
            .map(|pattern| {
                Regex::new(pattern)
                    .map_err(|error| format!("stuck_ignore {pattern:?} does not parse: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if on_stuck.as_ref().is_some_and(Vec::is_empty) {
            return Err("on_stuck is an empty command".to_owned());
        }
        Ok(Self {
            after,
            ignore,
            on_stuck: on_stuck.unwrap_or_default(),
        })
    }

    /// Reports whether `command` matches any `stuck_ignore` expression.
    #[must_use]
    pub fn ignores(&self, command: &str) -> bool {
        self.ignore.iter().any(|each| each.is_match(command))
    }
}
