//! Decides whether a waiting job starts now, how many threads it gets, and
//! holds the queue of waiting jobs and the list of running ones.
//!
//! [`admits`] is the whole admission rule, and [`Queue`] applies it to the
//! waiting jobs, the fast lane first and each lane in the order its jobs
//! arrived. Neither reads the machine or the clock, so the tests check every
//! case directly.

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::probe::{Disk, Pressure};

mod hold;
#[cfg(test)]
mod tests;

pub use self::hold::Hold;
use self::hold::percent;

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
    /// Whether each lane leaves one place open for the other, which a
    /// priority file with at least one entry turns on.
    pub reserve_lanes: bool,
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

/// The running jobs that the rule weighs for one waiting job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Running {
    /// How many jobs run.
    pub all: usize,
    /// How many of them run in the waiting job's lane.
    pub in_lane: usize,
    /// The threads of the running jobs that started in the last [`RECENT`].
    pub recent_jobs: usize,
}

/// Reports whether one more job starts now, or why it waits.
///
/// `reading` is what the daemon last read of the machine, or `None` when
/// the probe could not read it. `running` counts the jobs that run now.
///
/// A job waits while `max_jobs` jobs run, even when the probe failed. When
/// `limits.reserve_lanes` is on and more than one job may run, a job also
/// waits while its lane runs all but one place. A job waits while free disk
/// is under [`DISK_CRITICAL_BYTES`] or [`DISK_CRITICAL_PERCENT`], even when
/// nothing runs. Otherwise a job starts when nothing runs, so one job never
/// waits forever, and when the probe failed, so a broken probe never holds
/// work back. Past that, a job waits while memory pressure is above
/// normal, or while the load plus the threads of the jobs started in the
/// last [`RECENT`] reaches the cores. An unreadable disk or an unknown core
/// count holds nothing.
///
/// # Errors
///
/// Returns the [`Hold`] that keeps the job waiting.
pub fn admits(reading: Option<&Reading>, running: &Running, limits: &Limits) -> Result<(), Hold> {
    let max_jobs = limits.max_jobs.get();
    if running.all >= max_jobs {
        return Err(Hold::Full { max_jobs });
    }
    let places = max_jobs - 1;
    if limits.reserve_lanes && max_jobs > 1 && running.in_lane >= places {
        return Err(Hold::Lane { places });
    }
    if let Some(disk) = reading.and_then(|reading| reading.disk)
        && disk_critical(&disk)
    {
        return Err(Hold::Disk {
            free_bytes: disk.free_bytes,
            total_bytes: disk.total_bytes,
        });
    }
    let Some(reading) = reading.filter(|_| running.all > 0) else {
        return Ok(());
    };
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
        let (projected, cores_f) = (
            reading.load_1m + running.recent_jobs as f64,
            cores.get() as f64,
        );
        if projected >= cores_f {
            return Err(Hold::Load {
                projected,
                cores: cores.get(),
            });
        }
    }
    Ok(())
}

/// Reports whether `disk` has less free space than
/// [`DISK_CRITICAL_BYTES`] or [`DISK_CRITICAL_PERCENT`] of the disk.
#[must_use]
pub fn disk_critical(disk: &Disk) -> bool {
    disk.free_bytes < DISK_CRITICAL_BYTES
        || percent(disk.free_bytes, disk.total_bytes) < DISK_CRITICAL_PERCENT
}

/// The number that the queue gives each job it receives.
pub type JobId = u64;

/// The waiting jobs in each lane in the order they arrived, and the
/// running jobs.
#[derive(Debug)]
pub struct Queue<J> {
    /// The number the next job receives.
    next: JobId,
    /// The fast jobs that wait, the oldest first.
    fast: VecDeque<(JobId, J)>,
    /// The normal jobs that wait, the oldest first.
    normal: VecDeque<(JobId, J)>,
    /// The jobs that run, in the order they started.
    running: Vec<Started<J>>,
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
            held: None,
        }
    }
}

/// A running job, with what the load projection needs to know of it.
#[derive(Debug)]
struct Started<J> {
    /// The job's number.
    id: JobId,
    /// The job.
    job: J,
    /// The lane the job waited in.
    lane: Lane,
    /// When the job started.
    at: Instant,
    /// The job's threads.
    jobs: usize,
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

    /// Adds `job` as a job that already runs since `at`, placed among the
    /// running jobs in start order, and returns its number. The job counts
    /// at most the threads of one grant in `lane`, whatever `jobs` claims,
    /// and it runs even past `max_jobs`, because it has already started.
    pub fn seat(&mut self, job: J, lane: Lane, jobs: usize, at: Instant, limits: &Limits) -> JobId {
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        let place = self.running.partition_point(|each| each.at <= at);
        let jobs = jobs.min(limits.parallelism(lane));
        self.running.insert(
            place,
            Started {
                id,
                job,
                lane,
                at,
                jobs,
            },
        );
        id
    }

    /// Starts waiting jobs, the fast lane first, while [`admits`] lets one
    /// more start at `now`, and returns the number of each job it started
    /// with its threads. When the fast lane's first job waits on its lane's
    /// cap, the normal lane's first job may still start.
    pub fn admit(
        &mut self,
        reading: Option<&Reading>,
        limits: &Limits,
        now: Instant,
    ) -> Vec<(JobId, usize)> {
        self.held = None;
        let mut started = Vec::new();
        while let Some((lane, hold)) = self.next_lane(reading, limits, now) {
            if let Some(hold) = hold {
                self.held = Some(hold);
                break;
            }
            let waiting = match lane {
                Lane::Fast => &mut self.fast,
                Lane::Normal => &mut self.normal,
            };
            if let Some((id, job)) = waiting.pop_front() {
                let jobs = limits.parallelism(lane);
                self.running.push(Started {
                    id,
                    job,
                    lane,
                    at: now,
                    jobs,
                });
                started.push((id, jobs));
            }
        }
        started
    }

    /// Returns the lane whose first waiting job starts next, with `None`,
    /// or the first lane that has a waiting job with the hold that keeps it
    /// waiting. Returns `None` when no job waits.
    fn next_lane(
        &self,
        reading: Option<&Reading>,
        limits: &Limits,
        now: Instant,
    ) -> Option<(Lane, Option<Hold>)> {
        let recent_jobs = self
            .running
            .iter()
            .filter(|each| now.saturating_duration_since(each.at) < RECENT)
            .map(|each| each.jobs)
            .sum();
        let mut first = None;
        for (lane, waiting) in [(Lane::Fast, &self.fast), (Lane::Normal, &self.normal)] {
            if waiting.is_empty() {
                continue;
            }
            let running = Running {
                all: self.running.len(),
                in_lane: self.running.iter().filter(|each| each.lane == lane).count(),
                recent_jobs,
            };
            match admits(reading, &running, limits) {
                Ok(()) => return Some((lane, None)),
                Err(hold) => {
                    first.get_or_insert((lane, Some(hold)));
                }
            }
        }
        first
    }

    /// Returns why the first waiting job did not start at the last
    /// admission, or `None` when no job waited then.
    #[must_use]
    pub const fn held(&self) -> Option<Hold> {
        self.held
    }

    /// Removes the job numbered `id`, whether it waits or runs, and returns
    /// it with when it started, which is `None` for a job that still
    /// waited. Returns `None` when the queue does not hold it.
    pub fn remove(&mut self, id: JobId) -> Option<(J, Option<Instant>)> {
        for waiting in [&mut self.fast, &mut self.normal] {
            if let Some(index) = waiting.iter().position(|(each, _)| *each == id) {
                return waiting.remove(index).map(|(_, job)| (job, None));
            }
        }
        let index = self.running.iter().position(|each| each.id == id)?;
        let started = self.running.remove(index);
        Some((started.job, Some(started.at)))
    }

    /// Returns the running job numbered `id`.
    #[must_use]
    pub fn running_job(&self, id: JobId) -> Option<&J> {
        self.running
            .iter()
            .find(|each| each.id == id)
            .map(|each| &each.job)
    }

    /// Returns the threads of the running job numbered `id`.
    #[must_use]
    pub fn threads_of(&self, id: JobId) -> Option<usize> {
        let job = self.running.iter().find(|each| each.id == id)?;
        Some(job.jobs)
    }

    /// Returns how many jobs start before the waiting job numbered `id`, or
    /// `None` when that job does not wait.
    #[must_use]
    pub fn ahead_of(&self, id: JobId) -> Option<usize> {
        self.waiting_ids().position(|each| each == id)
    }

    /// Returns how many waiting jobs start before a job that joins `lane`
    /// now, in the order that [`Queue::waiting`] gives.
    #[must_use]
    pub fn ahead_in(&self, lane: Lane) -> usize {
        match lane {
            Lane::Fast => self.fast.len(),
            Lane::Normal => self.fast.len() + self.normal.len(),
        }
    }

    /// Returns the numbers of the waiting jobs in the order they start.
    fn waiting_ids(&self) -> impl Iterator<Item = JobId> + '_ {
        self.fast.iter().chain(&self.normal).map(|(id, _)| *id)
    }

    /// Returns the running jobs in the order they started.
    pub fn running(&self) -> impl Iterator<Item = &J> {
        self.running.iter().map(|each| &each.job)
    }

    /// Returns the running jobs in the order they started, each with when
    /// it started.
    pub fn running_since(&self) -> impl Iterator<Item = (&J, Instant)> {
        self.running.iter().map(|each| (&each.job, each.at))
    }

    /// Returns the waiting jobs in the order they start: the fast lane,
    /// then the normal lane, each oldest first.
    pub fn waiting(&self) -> impl Iterator<Item = &J> {
        self.fast.iter().chain(&self.normal).map(|(_, job)| job)
    }
}
