//! Decides whether a waiting job starts now, and holds the queue of waiting
//! jobs and the list of running ones.
//!
//! [`admits`] is the whole admission rule, and [`Queue`] applies it to the
//! waiting jobs in the order they arrived. Neither reads the machine or the
//! clock, so the tests check every case directly.

use std::collections::VecDeque;
use std::num::NonZeroUsize;

use crate::probe::Pressure;

#[cfg(test)]
mod tests;

/// Reports whether one more job starts now.
///
/// `pressure` is the latest memory pressure level, or `None` when the probe
/// could not read the machine. `running` counts the jobs that run now, and
/// `max_jobs` is the most jobs that run at once.
///
/// A job starts when nothing runs, so one job never waits forever. A job
/// also starts when the probe failed, so a broken probe never holds work
/// back. Otherwise a job starts when memory pressure is normal and fewer
/// than `max_jobs` jobs run.
#[must_use]
pub fn admits(pressure: Option<Pressure>, running: usize, max_jobs: NonZeroUsize) -> bool {
    match pressure {
        _ if running == 0 => true,
        None => true,
        Some(Pressure::Normal) => running < max_jobs.get(),
        Some(Pressure::Warn | Pressure::Critical) => false,
    }
}

/// The number that the queue gives each job it receives.
pub type JobId = u64;

/// The waiting jobs in the order they arrived, and the running jobs.
#[derive(Debug)]
pub struct Queue<J> {
    /// The number the next job receives.
    next: JobId,
    /// The jobs that wait, the oldest first.
    waiting: VecDeque<(JobId, J)>,
    /// The jobs that run, in the order they started.
    running: Vec<(JobId, J)>,
}

impl<J> Default for Queue<J> {
    fn default() -> Self {
        Self {
            next: 0,
            waiting: VecDeque::new(),
            running: Vec::new(),
        }
    }
}

impl<J> Queue<J> {
    /// Adds `job` to the end of the waiting jobs and returns its number.
    pub fn push(&mut self, job: J) -> JobId {
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        self.waiting.push_back((id, job));
        id
    }

    /// Starts waiting jobs, the oldest first, while [`admits`] lets one more
    /// start, and returns the numbers of the jobs it started.
    pub fn admit(&mut self, pressure: Option<Pressure>, max_jobs: NonZeroUsize) -> Vec<JobId> {
        let mut started = Vec::new();
        while !self.waiting.is_empty() && admits(pressure, self.running.len(), max_jobs) {
            if let Some((id, job)) = self.waiting.pop_front() {
                self.running.push((id, job));
                started.push(id);
            }
        }
        started
    }

    /// Removes the job numbered `id`, whether it waits or runs, and returns
    /// it, or `None` when the queue does not hold it.
    pub fn remove(&mut self, id: JobId) -> Option<J> {
        if let Some(index) = self.waiting.iter().position(|(each, _)| *each == id) {
            return self.waiting.remove(index).map(|(_, job)| job);
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

    /// Returns how many jobs wait ahead of the waiting job numbered `id`, or
    /// `None` when that job does not wait.
    #[must_use]
    pub fn ahead_of(&self, id: JobId) -> Option<usize> {
        self.waiting.iter().position(|(each, _)| *each == id)
    }

    /// Returns the running jobs in the order they started.
    pub fn running(&self) -> impl Iterator<Item = &J> {
        self.running.iter().map(|(_, job)| job)
    }

    /// Returns the waiting jobs, the oldest first.
    pub fn waiting(&self) -> impl Iterator<Item = &J> {
        self.waiting.iter().map(|(_, job)| job)
    }
}
