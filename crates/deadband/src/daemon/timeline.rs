//! What the daemon remembers of the jobs that have ended, and the reply
//! that `deadband top` draws from them and from the queue.

use std::time::Instant;

use super::{State, Waiter};
use crate::admit::{JobId, Lane};
use crate::protocol::{Ago, Exit, JobRecord, JobState, Label, Timeline};

/// The most ended jobs that the daemon remembers. The 65th evicts the
/// oldest. The list lives in memory only, so a restart forgets it.
pub(super) const MAX_ENDED: usize = 64;

/// The most jobs in one reply. Further jobs count in the reply's `hidden`.
pub(super) const MAX_ROWS: usize = 100;

/// A job that has left the queue.
#[derive(Debug)]
pub(super) struct Ended {
    /// The job's label.
    label: Label,
    /// The lane the job waited in.
    lane: Lane,
    /// When the job asked to start.
    queued: Instant,
    /// When the job ended.
    ended: Instant,
    /// How the job ended, with when it started, or `None` when it left
    /// before it ran.
    run: Option<(Instant, Exit)>,
}

impl Ended {
    /// Returns the job's row at `now`.
    fn row(&self, now: Instant) -> JobRecord {
        let ended = Ago::between(self.ended, now);
        let state = match self.run {
            Some((started, exit)) => JobState::Finished {
                started: Ago::between(started, now),
                ended,
                exit,
            },
            None => JobState::Left { ended },
        };
        JobRecord {
            label: self.label.clone(),
            lane: self.lane,
            queued: Ago::between(self.queued, now),
            state,
        }
    }
}

impl Waiter {
    /// Returns the row at `now` of the job while it waits or runs.
    fn row(&self, now: Instant, state: JobState) -> JobRecord {
        JobRecord {
            label: self.label.clone(),
            lane: self.job.lane,
            queued: Ago::between(self.queued, now),
            state,
        }
    }
}

impl State {
    /// Takes the job numbered `id` out of the queue and remembers how it
    /// ended. `exit` is `None` when the job's client could not take its
    /// grant. A job that the queue no longer holds is recorded already, so
    /// nothing happens.
    pub(super) fn record(&mut self, id: JobId, exit: Option<Exit>) {
        let Some((waiter, started)) = self.queue.remove(id) else {
            return;
        };
        if self.ended.len() == MAX_ENDED {
            self.ended.pop_front();
        }
        self.ended.push_back(Ended {
            lane: waiter.job.lane,
            label: waiter.label,
            queued: waiter.queued,
            ended: Instant::now(),
            run: started.zip(exit),
        });
    }

    /// Returns the timeline at `now`: at most [`MAX_ROWS`] jobs, the
    /// running ones, then the waiting ones, then the ended ones, the
    /// newest first.
    pub(super) fn timeline(&self, now: Instant) -> Timeline {
        let running = self.queue.running_since().map(|(waiter, started)| {
            let started = Ago::between(started, now);
            waiter.row(now, JobState::Running { started })
        });
        let waiting = self
            .queue
            .waiting()
            .map(|waiter| waiter.row(now, JobState::Waiting));
        let ended = self.ended.iter().rev().map(|each| each.row(now));
        let known = self.queue.running().count() + self.queue.waiting().count() + self.ended.len();
        Timeline {
            up: Ago::between(self.up, now),
            jobs: running.chain(waiting).chain(ended).take(MAX_ROWS).collect(),
            hidden: known.saturating_sub(MAX_ROWS),
            held: self.queue.held(),
        }
    }
}
