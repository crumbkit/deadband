//! The number that the queue gives each job, and the count of threads that
//! a job may use.

use serde::{Deserialize, Serialize};

/// The number that the queue gives each job it receives. The daemon keeps
/// it in memory only, so a new daemon numbers a job it seats afresh. The
/// daemon's list of jobs carries it as a JSON number, so a reader can tell
/// apart two jobs that report one process ID.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(u64);

impl JobId {
    /// Returns this number and moves on to the next one, which wraps to 0
    /// after `u64::MAX`.
    pub const fn take(&mut self) -> Self {
        let id = *self;
        self.0 = self.0.wrapping_add(1);
        id
    }

    /// Returns the number `number`, which a test expects the queue to give.
    #[cfg(test)]
    pub(crate) const fn new(number: u64) -> Self {
        Self(number)
    }
}

/// The threads that a job may use, which `run` gives the job in its
/// parallelism variables. It is a JSON number on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Threads(usize);

impl Threads {
    /// Returns `count` threads.
    #[must_use]
    pub const fn new(count: usize) -> Self {
        Self(count)
    }

    /// Returns the number of threads.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}
