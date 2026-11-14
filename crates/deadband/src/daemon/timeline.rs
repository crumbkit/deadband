//! What the daemon remembers of the jobs that have ended, and the reply
//! that `deadband top` draws from them and from the queue.

use std::time::{Instant, SystemTime};

use super::{State, Waiter};
use crate::admit::{JobId, Lane, Threads};
use crate::history::{Departure, JobLine, Line, MachineLine, Tally, VERSION, now_ms};
use crate::probe::ChildUsage;
use crate::protocol::{Ago, Exit, JobRecord, JobState, Jobs, Label, Short, StuckPeriod, Timeline};
use crate::stuck::Period;

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
    /// The workspace the job ran in.
    workspace: Option<Label>,
    /// The job's short command.
    short: Option<Short>,
    /// The lane the job waited in.
    lane: Lane,
    /// When the job asked to start.
    queued: Instant,
    /// When the job ended.
    ended: Instant,
    /// How the job ended, with when it started, or `None` when it left
    /// before it ran.
    run: Option<(Instant, Exit)>,
    /// The job's last periods with no activity.
    stuck: Vec<Period>,
}

/// Returns `period` as ages at `now`. A period still open at `closed` ends
/// there.
fn stuck_period(period: &Period, closed: Option<Instant>, now: Instant) -> StuckPeriod {
    StuckPeriod {
        since: Ago::between(period.since, now),
        until: period
            .until
            .or(closed)
            .map(|until| Ago::between(until, now)),
    }
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
            workspace: self.workspace.clone(),
            short: self.short.clone(),
            lane: self.lane,
            queued: Ago::between(self.queued, now),
            state,
            stuck: self
                .stuck
                .iter()
                .map(|period| stuck_period(period, Some(self.ended), now))
                .collect(),
        }
    }
}

impl Waiter {
    /// Returns the row at `now` of the job while it waits or runs.
    fn row(&self, now: Instant, state: JobState) -> JobRecord {
        JobRecord {
            label: self.job.label.clone(),
            workspace: self.job.workspace.clone(),
            short: self.job.short.clone(),
            lane: self.job.lane,
            queued: Ago::between(self.queued, now),
            state,
            stuck: Vec::new(),
        }
    }
}

impl State {
    /// Returns the running and waiting jobs.
    pub(super) fn jobs(&self) -> Jobs {
        let now = Instant::now();
        Jobs {
            max_jobs: self.settings.max_jobs.get(),
            running: self.queue.running().map(|each| each.job.clone()).collect(),
            waiting: self.queue.waiting().map(|each| each.job.clone()).collect(),
            held: self.queue.held(),
            reclaim: self.reclaimed.clone(),
            reclaiming: self
                .watch
                .reclaiming(&self.settings.reclaim, SystemTime::now())
                .map(Box::new),
            stuck: self.stuck_jobs(now),
            busiest: self.stuck.busiest().cloned(),
            unreadable: self.stuck.unreadable(),
        }
    }

    /// Takes the job numbered `id` out of the queue, remembers how it
    /// ended, and hands its line to the history writer. `exit` is `None`
    /// when the job's client could not take its grant. A job that the queue
    /// no longer holds is recorded already, so nothing happens.
    pub(super) fn record(&mut self, id: JobId, exit: Option<Exit>, usage: Option<ChildUsage>) {
        let Some((waiter, started)) = self.queue.remove(id) else {
            return;
        };
        let tally = self.tallies.remove(&id);
        let ended = Instant::now();
        let threads = started.map(|(_, threads)| threads);
        let run = started.map(|(at, _)| at).zip(exit);
        if let (Some(recorder), Some(arrival)) = (&self.recorder, waiter.arrival) {
            let departure = Departure {
                label: waiter.job.label.clone(),
                lane: waiter.job.lane,
                ahead: waiter.ahead,
                threads: run.and(threads).map(Threads::get),
                ran: run.map(|(at, _)| ended.saturating_duration_since(at)),
                exit: run.map(|(_, exit)| exit),
                usage,
                tally,
            };
            let line = JobLine::new(arrival, departure, now_ms());
            recorder.send(Line::Job(Box::new(line)));
        }
        if self.ended.len() == MAX_ENDED {
            self.ended.pop_front();
        }
        self.ended.push_back(Ended {
            lane: waiter.job.lane,
            stuck: self.stuck.end(waiter.job.pid),
            label: waiter.job.label,
            workspace: waiter.job.workspace,
            short: waiter.job.short,
            queued: waiter.queued,
            ended,
            run,
        });
    }

    /// Notes in every tally how many jobs run now, and returns that number.
    pub(super) fn note_running(&mut self) -> usize {
        let running = self.queue.running().count();
        for tally in self.tallies.values_mut() {
            tally.running(running);
        }
        running
    }

    /// Starts the tally of the job numbered `id`, which has just started.
    pub(super) fn count_started(&mut self, id: JobId) {
        let running = self.note_running();
        let load = self.reading.map(|reading| reading.load_1m);
        self.tallies.insert(id, Tally::started(load, running));
    }

    /// Adds the latest reading to the tally of each running job. A failed
    /// reading adds nothing.
    pub(super) fn observe(&mut self) {
        let Some(reading) = self.reading else {
            return;
        };
        let running = self.queue.running().count();
        let every = self.settings.sample_every;
        for tally in self.tallies.values_mut() {
            tally.sample(reading.load_1m, reading.pressure, every, running);
        }
    }

    /// Hands a machine line to the history writer when the settings'
    /// interval has passed since the last one.
    pub(super) fn write_machine(&mut self, now: Instant) {
        let Some(recorder) = &self.recorder else {
            return;
        };
        if now.saturating_duration_since(self.machine_at) < self.settings.machine_every {
            return;
        }
        self.machine_at = now;
        recorder.send(Line::Machine(MachineLine {
            v: VERSION,
            at_ms: now_ms(),
            load_1m: self.reading.map(|reading| reading.load_1m),
            pressure: self.reading.map(|reading| reading.pressure),
            running: self.queue.running().count(),
            waiting: self.queue.waiting().count(),
            held: self.queue.held(),
            max_jobs: self.settings.max_jobs.get(),
            cores: self.settings.cores.map(std::num::NonZeroUsize::get),
            dropped: recorder.dropped(),
        }));
    }

    /// Returns the timeline at `now`: at most [`MAX_ROWS`] jobs, the
    /// running ones, then the waiting ones, then the ended ones, the
    /// newest first.
    pub(super) fn timeline(&self, now: Instant) -> Timeline {
        let running = self.queue.running_since().map(|(waiter, started)| {
            let started = Ago::between(started, now);
            let mut row = waiter.row(now, JobState::Running { started });
            row.stuck = self
                .stuck
                .periods(waiter.job.pid)
                .iter()
                .map(|period| stuck_period(period, None, now))
                .collect();
            row
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
            busiest: self.stuck.busiest().cloned(),
            unreadable: self.stuck.unreadable(),
        }
    }
}
