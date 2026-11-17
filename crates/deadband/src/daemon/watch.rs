//! Watches the process tree of each job that `deadband run` started, and
//! says when a job has shown no activity.
//!
//! One thread reads each tree every `Watching::every`, and only while a
//! watched job runs. It copies the jobs under the queue lock, reads their
//! trees and runs `/bin/ps` with the lock released, applies the rule in
//! [`stuck`](crate::stuck) under the lock, and writes each notice with the
//! lock released, so admission never waits on a reading. The thread never
//! signals or changes a job. Every failure here leaves the job running and
//! writes one log line.
//!
//! All of this state is in memory, and a restart of the daemon loses it.
//! Each lease seats its job on the next daemon, which watches it with a
//! fresh timer.

use std::collections::{BTreeSet, HashMap};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use self::hook::Hooks;
use super::{Log, Shared, State, Watching, lock, note};
use crate::admit::JobId;
use crate::job::busiest::{Census, Failed};
use crate::probe::{Probe, ProcessUse, TreeUnreadable};
use crate::protocol::{Ago, Label, OutsideJobs, Reply, StuckJob, write_line};
use crate::spawn;
use crate::stuck::{self, Change, Period, Track};

mod hook;

/// What the daemon knows of the watched jobs.
#[derive(Debug, Default)]
pub(super) struct Watched {
    /// What the rule remembers of each running watched job, by the job's
    /// number. A program that embeds deadband reports its own process ID
    /// for every job, so two of its jobs have one process ID and two
    /// numbers. A track ends when its job leaves the queue.
    tracks: HashMap<JobId, Track>,
    /// The processes outside the jobs while a job is stuck.
    busiest: Option<OutsideJobs>,
    /// Whether the latest read of a job's processes failed.
    unreadable: bool,
}

impl Watched {
    /// Returns the processes outside the jobs while a job is stuck.
    pub(super) const fn busiest(&self) -> Option<&OutsideJobs> {
        self.busiest.as_ref()
    }

    /// Reports whether the latest read of a job's processes failed.
    pub(super) const fn unreadable(&self) -> bool {
        self.unreadable
    }

    /// Returns the stuck periods of the job numbered `id`.
    pub(super) fn periods(&self, id: JobId) -> Vec<Period> {
        self.tracks.get(&id).map(Track::periods).unwrap_or_default()
    }

    /// Ends the record of the job numbered `id`, and returns its stuck
    /// periods. A job that never ran has no record, and gets none.
    pub(super) fn end(&mut self, id: JobId) -> Vec<Period> {
        self.tracks
            .remove(&id)
            .map(|track| track.periods())
            .unwrap_or_default()
    }
}

/// The job a change belongs to.
struct Noticed {
    /// The job's number.
    id: JobId,
    /// The job's label.
    label: Label,
    /// What changed.
    change: Change,
}

impl State {
    /// Returns the stuck jobs at `now`, in the order they started.
    pub(super) fn stuck_jobs(&self, now: Instant) -> Vec<StuckJob> {
        self.queue
            .running_since()
            .filter_map(|(id, waiter, _)| {
                let quiet = self.stuck.tracks.get(&id)?.quiet(now)?;
                Some(StuckJob {
                    id: Some(id),
                    label: waiter.job.label.clone(),
                    pid: waiter.job.pid,
                    quiet,
                })
            })
            .collect()
    }

    /// Returns the number, root process ID and command of each running
    /// watched job.
    fn watched_jobs(&self) -> Vec<(JobId, u32, String)> {
        self.queue
            .running_since()
            .filter(|(_, waiter, _)| waiter.job.watched)
            .map(|(id, waiter, _)| (id, waiter.job.pid, waiter.job.command.clone()))
            .collect()
    }

    /// Feeds each reading to the rule, and returns what changed. A job that
    /// ended while its tree was read has no record any more, and gets none.
    fn apply(
        &mut self,
        readings: &[(JobId, Vec<ProcessUse>)],
        after: Duration,
        now: Instant,
    ) -> Vec<Noticed> {
        let running: HashMap<JobId, Label> = self
            .queue
            .running_since()
            .map(|(id, waiter, _)| (id, waiter.job.label.clone()))
            .collect();
        let mut noticed = Vec::new();
        for (id, tree) in readings {
            let Some(label) = running.get(id) else {
                continue;
            };
            let track = self
                .stuck
                .tracks
                .entry(*id)
                .or_insert_with(|| Track::new(now));
            if let Some(change) = track.observe(tree, after, now) {
                noticed.push(Noticed {
                    id: *id,
                    label: label.clone(),
                    change,
                });
            }
        }
        noticed
    }

    /// Returns the connection of the running job numbered `id`, which the
    /// caller writes to after it releases the lock.
    fn notice_stream(&self, id: JobId) -> Option<Arc<UnixStream>> {
        let waiter = self.queue.running_job(id)?;
        Some(Arc::clone(&waiter.stream))
    }
}

/// What the watch thread keeps between samples.
struct Watcher<P> {
    /// The daemon's state.
    shared: Shared,
    /// The probe that reads the trees.
    probe: P,
    /// The daemon's log.
    log: Log,
    /// The stuck keys of the config.
    config: stuck::Config,
    /// How long a job must show no activity to count as stuck.
    after: Duration,
    /// How the daemon watches jobs.
    watching: Watching,
    /// The process list, at most one at a time.
    census: Census,
    /// When the process list last ran.
    census_at: Option<Instant>,
    /// Whether the last process list failed.
    census_failing: bool,
    /// Whether the last read of a tree failed.
    tree_failing: bool,
    /// Runs `on_stuck`.
    hooks: Hooks,
}

/// Starts the watch thread, unless the config turns detection off. The
/// thread stops when `stopped` receives or disconnects.
pub(super) fn start<P: Probe + Send + 'static>(
    shared: &Shared,
    probe: P,
    stopped: mpsc::Receiver<()>,
    log: &Log,
) {
    let (config, watching) = {
        let state = lock(shared);
        (
            state.settings.reclaim.stuck.clone(),
            state.settings.watching.clone(),
        )
    };
    let Some(after) = config.after else {
        return;
    };
    let hooks = Hooks::new(config.on_stuck.clone(), watching.on_stuck_within, log);
    let mut watcher = Watcher {
        shared: Arc::clone(shared),
        probe,
        log: Arc::clone(log),
        config,
        after,
        watching,
        census: Census::default(),
        census_at: None,
        census_failing: false,
        tree_failing: false,
        hooks,
    };
    // A thread that cannot start leaves every job showing as running.
    let spawned = spawn::thread("deadband-watch", move || {
        let every = watcher.watching.every;
        while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
            watcher.tick();
        }
    });
    if let Err(error) = spawned {
        note(
            log,
            format_args!("the watch thread could not start: {error}, so no job is flagged stuck"),
        );
    }
}

impl<P: Probe> Watcher<P> {
    /// Takes one sample of every watched job.
    fn tick(&mut self) {
        let mut roots = lock(&self.shared).watched_jobs();
        roots.retain(|(_, _, command)| !self.config.ignores(command));
        if roots.is_empty() {
            let mut state = lock(&self.shared);
            state.stuck.unreadable = false;
            state.stuck.busiest = None;
            return;
        }
        let (readings, failure) = self.read_trees(&roots);
        self.note_trees(failure.as_ref());
        let now = Instant::now();
        let (noticed, any_stuck) = {
            let mut state = lock(&self.shared);
            state.stuck.unreadable = failure.is_some();
            let noticed = state.apply(&readings, self.after, now);
            (noticed, !state.stuck_jobs(now).is_empty())
        };
        if any_stuck {
            let became = noticed
                .iter()
                .any(|each| matches!(each.change, Change::Stuck { .. }));
            self.name_busiest(became, &readings, now);
        } else {
            lock(&self.shared).stuck.busiest = None;
        }
        let busiest = lock(&self.shared).stuck.busiest.clone();
        for each in noticed {
            self.tell(&each, busiest.as_ref());
        }
    }

    /// Reads the tree of each root with the lock released. A platform with
    /// no reader gives no reading and no failure.
    fn read_trees(
        &self,
        roots: &[(JobId, u32, String)],
    ) -> (Vec<(JobId, Vec<ProcessUse>)>, Option<std::io::Error>) {
        let mut readings = Vec::new();
        let mut failure = None;
        for (id, pid, _) in roots {
            match self.probe.tree(*pid) {
                Ok(tree) => readings.push((*id, tree)),
                Err(TreeUnreadable::Unsupported) => {}
                Err(TreeUnreadable::Failed(error)) => failure = Some(error),
            }
        }
        (readings, failure)
    }

    /// Writes one log line when reading trees starts failing or reads again.
    fn note_trees(&mut self, failure: Option<&std::io::Error>) {
        match failure {
            Some(error) if !self.tree_failing => {
                self.tree_failing = true;
                note(
                    &self.log,
                    format_args!(
                        "deadband could not read the jobs' processes: {error}, so it cannot say whether any job is stuck until it reads them again"
                    ),
                );
            }
            None if self.tree_failing => {
                self.tree_failing = false;
                note(
                    &self.log,
                    format_args!("deadband reads the jobs' processes again"),
                );
            }
            _ => {}
        }
    }

    /// Runs the process list when a job has just `became` stuck, or at most
    /// once per `census_every` while one stays stuck. It holds no lock.
    fn name_busiest(&mut self, became: bool, readings: &[(JobId, Vec<ProcessUse>)], now: Instant) {
        let due = self
            .census_at
            .is_none_or(|at| now.saturating_duration_since(at) >= self.watching.census_every);
        if !became && !due {
            return;
        }
        self.census_at = Some(now);
        let mut exclude: BTreeSet<u32> = readings
            .iter()
            .flat_map(|(_, tree)| tree.iter().map(|each| each.pid))
            .collect();
        exclude.insert(std::process::id());
        let read = self
            .census
            .read(&self.watching.census, self.watching.census_within, &exclude);
        let busiest = match read {
            Ok(busiest) => {
                self.census_failing = false;
                Some(busiest)
            }
            Err(Failed::Running) => return,
            Err(failed) => {
                if !self.census_failing {
                    self.census_failing = true;
                    note(
                        &self.log,
                        format_args!("deadband could not name the busiest process: {failed}"),
                    );
                }
                None
            }
        };
        lock(&self.shared).stuck.busiest = busiest;
    }

    /// Writes the notice of `noticed` to its client with the lock released,
    /// and runs `on_stuck` when the job has become stuck. A write that fails
    /// or times out loses that line.
    fn tell(&self, noticed: &Noticed, busiest: Option<&OutsideJobs>) {
        let quiet = |quiet: Duration| Ago::from_duration(quiet);
        let reply = match noticed.change {
            Change::Stuck { quiet: q } => Reply::Stuck {
                quiet: quiet(q),
                again: false,
                busiest: busiest.cloned(),
            },
            Change::StillStuck { quiet: q } => Reply::Stuck {
                quiet: quiet(q),
                again: true,
                busiest: busiest.cloned(),
            },
            Change::Active { quiet: q } => Reply::Active { quiet: quiet(q) },
        };
        let stream = lock(&self.shared).notice_stream(noticed.id);
        if let Some(stream) = stream {
            let _ = write_line(&mut &*stream, &reply);
        }
        if let Change::Stuck { quiet } = noticed.change {
            self.hooks.run(noticed.label.as_str(), quiet, busiest);
        }
    }
}
