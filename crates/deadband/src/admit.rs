//! Decides whether a waiting job starts now, how many threads it gets, and
//! holds the queue of waiting jobs and the list of running ones.
//!
//! [`admits`] is the whole admission rule, and [`Queue`] applies it to the
//! waiting jobs, the fast lane first and each lane in the order its jobs
//! arrived. Neither reads the machine or the clock, so the tests check every
//! case directly.

use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::probe::{Disk, Pressure};

#[cfg(test)]
mod tests;

/// The bytes in a gibibyte.
pub const GIB: u64 = 1024 * 1024 * 1024;

/// A job waits while the disk has less free space than this, in bytes.
pub const DISK_CRITICAL_BYTES: u64 = 20 * GIB;

/// A job waits while the disk's free space is less than this percentage of
/// the disk.
pub const DISK_CRITICAL_PERCENT: u64 = 3;

/// How long a started job counts towards the load. The 1-minute load
/// average takes about this long to show a new job, so until then the rule
/// adds the job's threads to the load it reads.
pub const RECENT: Duration = Duration::from_secs(60);

/// The lane a job waits in. Waiting fast jobs start before waiting normal
/// ones, and get one more thread.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    /// Priority work.
    Fast,
    /// Every other job.
    #[default]
    Normal,
}

/// What the daemon last read of the machine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reading {
    /// The kernel's memory pressure level.
    pub pressure: Pressure,
    /// The 1-minute load average.
    pub load_1m: f64,
    /// The space of the disk that holds the home directory, or `None` when
    /// it could not be read.
    pub disk: Option<Disk>,
}

/// The limits that the daemon admits jobs within.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// The most jobs that run at once.
    pub max_jobs: NonZeroUsize,
    /// The number of logical cores, or `None` when it is unknown.
    pub cores: Option<NonZeroUsize>,
}

impl Limits {
    /// Returns the threads a job in `lane` gets: the cores divided by the
    /// most jobs, at least 1, and one more in the fast lane.
    #[must_use]
    pub fn parallelism(&self, lane: Lane) -> usize {
        let cores = self.cores.map_or(1, NonZeroUsize::get);
        let normal = (cores / self.max_jobs.get()).max(1);
        match lane {
            Lane::Fast => normal.saturating_add(1),
            Lane::Normal => normal,
        }
    }
}

/// Why a waiting job does not start yet.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "hold", rename_all = "snake_case")]
pub enum Hold {
    /// The most jobs run already.
    Full {
        /// The most jobs that run at once.
        max_jobs: usize,
    },
    /// The disk is critically short of free space.
    Disk {
        /// The disk's free bytes.
        free_bytes: u64,
        /// The disk's total bytes.
        total_bytes: u64,
    },
    /// Memory pressure is above normal.
    Memory {
        /// The memory pressure level.
        pressure: Pressure,
    },
    /// The load, with the threads of the jobs started in the last minute,
    /// exceeds the cores.
    Load {
        /// The 1-minute load plus those threads.
        projected: f64,
        /// The number of logical cores.
        cores: usize,
    },
}

impl fmt::Display for Hold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full { max_jobs: 1 } => {
                f.write_str("the daemon already runs 1 job, its most at once")
            }
            Self::Full { max_jobs } => {
                write!(
                    f,
                    "the daemon already runs {max_jobs} jobs, its most at once"
                )
            }
            Self::Disk {
                free_bytes,
                total_bytes,
            } => write!(
                f,
                "free disk is {} GiB, {}% of the disk, under {} GiB or {DISK_CRITICAL_PERCENT}%",
                free_bytes / GIB,
                percent(*free_bytes, *total_bytes),
                DISK_CRITICAL_BYTES / GIB
            ),
            Self::Memory { pressure } => write!(f, "memory pressure is {pressure}"),
            Self::Load { projected, cores } => write!(
                f,
                "the 1-minute load with the jobs started in the last minute is {projected:.1}, over the {cores} cores"
            ),
        }
    }
}

/// Returns `part` as a whole percentage of `whole`, rounded down, or 100
/// for an empty whole.
fn percent(part: u64, whole: u64) -> u64 {
    part.saturating_mul(100).checked_div(whole).unwrap_or(100)
}

/// Reports whether one more job starts now, or why it waits.
///
/// `reading` is what the daemon last read of the machine, or `None` when
/// the probe could not read it. `running` counts the jobs that run now, and
/// `recent_jobs` adds up the threads of the jobs started in the last
/// [`RECENT`].
///
/// A job starts when nothing runs, so one job never waits forever. A job
/// also starts when the probe failed, so a broken probe never holds work
/// back. Otherwise a job waits while `max_jobs` jobs run, while free disk
/// is under [`DISK_CRITICAL_BYTES`] or [`DISK_CRITICAL_PERCENT`], while
/// memory pressure is above normal, or while the load plus `recent_jobs`
/// exceeds the cores. An unreadable disk or an unknown core count holds
/// nothing.
///
/// # Errors
///
/// Returns the [`Hold`] that keeps the job waiting.
pub fn admits(
    reading: Option<&Reading>,
    running: usize,
    recent_jobs: usize,
    limits: &Limits,
) -> Result<(), Hold> {
    let Some(reading) = reading.filter(|_| running > 0) else {
        return Ok(());
    };
    if running >= limits.max_jobs.get() {
        return Err(Hold::Full {
            max_jobs: limits.max_jobs.get(),
        });
    }
    if let Some(disk) = reading.disk {
        let critical = disk.free_bytes < DISK_CRITICAL_BYTES
            || percent(disk.free_bytes, disk.total_bytes) < DISK_CRITICAL_PERCENT;
        if critical {
            return Err(Hold::Disk {
                free_bytes: disk.free_bytes,
                total_bytes: disk.total_bytes,
            });
        }
    }
    if reading.pressure != Pressure::Normal {
        return Err(Hold::Memory {
            pressure: reading.pressure,
        });
    }
    if let Some(cores) = limits.cores {
        #[expect(
            clippy::cast_precision_loss,
            reason = "thread and core counts are far below 2^52, so the conversion is exact"
        )]
        let projected = reading.load_1m + recent_jobs as f64;
        #[expect(
            clippy::cast_precision_loss,
            reason = "a core count is far below 2^52, so the conversion is exact"
        )]
        if projected > cores.get() as f64 {
            return Err(Hold::Load {
                projected,
                cores: cores.get(),
            });
        }
    }
    Ok(())
}

/// The number that the queue gives each job it receives.
pub type JobId = u64;

/// The waiting jobs in each lane in the order they arrived, the running
/// jobs, and the threads of the jobs started recently.
#[derive(Debug)]
pub struct Queue<J> {
    /// The number the next job receives.
    next: JobId,
    /// The fast jobs that wait, the oldest first.
    fast: VecDeque<(JobId, J)>,
    /// The normal jobs that wait, the oldest first.
    normal: VecDeque<(JobId, J)>,
    /// The jobs that run, in the order they started.
    running: Vec<(JobId, J)>,
    /// When each job of the last [`RECENT`] started, with its threads.
    recent: VecDeque<(Instant, usize)>,
    /// Why the first waiting job did not start at the last admission.
    held: Option<Hold>,
}

impl<J> Default for Queue<J> {
    fn default() -> Self {
        Self {
            next: 0,
            fast: VecDeque::new(),
            normal: VecDeque::new(),
            running: Vec::new(),
            recent: VecDeque::new(),
            held: None,
        }
    }
}

impl<J> Queue<J> {
    /// Adds `job` to the end of `lane` and returns its number.
    pub fn push(&mut self, job: J, lane: Lane) -> JobId {
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        match lane {
            Lane::Fast => self.fast.push_back((id, job)),
            Lane::Normal => self.normal.push_back((id, job)),
        }
        id
    }

    /// Starts waiting jobs, the fast lane first, while [`admits`] lets one
    /// more start at `now`, and returns the number of each job it started
    /// with its threads.
    pub fn admit(
        &mut self,
        reading: Option<&Reading>,
        limits: &Limits,
        now: Instant,
    ) -> Vec<(JobId, usize)> {
        self.recent
            .retain(|(at, _)| now.saturating_duration_since(*at) < RECENT);
        self.held = None;
        let mut started = Vec::new();
        loop {
            let (lane, waiting) = if self.fast.is_empty() {
                (Lane::Normal, &mut self.normal)
            } else {
                (Lane::Fast, &mut self.fast)
            };
            if waiting.is_empty() {
                break;
            }
            let recent_jobs = self.recent.iter().map(|(_, jobs)| jobs).sum();
            if let Err(hold) = admits(reading, self.running.len(), recent_jobs, limits) {
                self.held = Some(hold);
                break;
            }
            if let Some((id, job)) = waiting.pop_front() {
                let jobs = limits.parallelism(lane);
                self.running.push((id, job));
                self.recent.push_back((now, jobs));
                started.push((id, jobs));
            }
        }
        started
    }

    /// Returns why the first waiting job did not start at the last
    /// admission, or `None` when no job waited then.
    #[must_use]
    pub const fn held(&self) -> Option<Hold> {
        self.held
    }

    /// Removes the job numbered `id`, whether it waits or runs, and returns
    /// it, or `None` when the queue does not hold it.
    pub fn remove(&mut self, id: JobId) -> Option<J> {
        for waiting in [&mut self.fast, &mut self.normal] {
            if let Some(index) = waiting.iter().position(|(each, _)| *each == id) {
                return waiting.remove(index).map(|(_, job)| job);
            }
        }
        let index = self.running.iter().position(|(each, _)| *each == id)?;
        Some(self.running.remove(index).1)
    }

    /// Returns the running job numbered `id`.
    #[must_use]
    pub fn running_job(&self, id: JobId) -> Option<&J> {
        self.running
            .iter()
            .find(|(each, _)| *each == id)
            .map(|(_, job)| job)
    }

    /// Returns how many jobs start before the waiting job numbered `id`, or
    /// `None` when that job does not wait.
    #[must_use]
    pub fn ahead_of(&self, id: JobId) -> Option<usize> {
        self.waiting_ids().position(|each| each == id)
    }

    /// Returns the numbers of the waiting jobs in the order they start.
    fn waiting_ids(&self) -> impl Iterator<Item = JobId> + '_ {
        self.fast.iter().chain(&self.normal).map(|(id, _)| *id)
    }

    /// Returns the running jobs in the order they started.
    pub fn running(&self) -> impl Iterator<Item = &J> {
        self.running.iter().map(|(_, job)| job)
    }

    /// Returns the waiting jobs in the order they start: the fast lane,
    /// then the normal lane, each oldest first.
    pub fn waiting(&self) -> impl Iterator<Item = &J> {
        self.fast.iter().chain(&self.normal).map(|(_, job)| job)
    }
}
