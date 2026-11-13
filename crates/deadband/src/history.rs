//! The record of finished jobs: one private JSON Lines file per machine,
//! which an offline script replays against admission policies.
//!
//! The daemon is the only writer, and nothing in deadband reads the file.
//! The daemon builds a [`Line`] while it holds the queue lock and hands it
//! to [`Recorder::send`], which never blocks and never touches the disk. One
//! thread, in the `writer` module, appends each line. The disk work that a
//! record needs from the job's working directory is in [`Arrival::read`],
//! which the job's connection thread calls before it takes the lock.
//!
//! No failure here delays, holds or ends a job.
//!
//! Each line carries [`VERSION`] in its `v` field, and a reader refuses a
//! line with a higher version (DUR-3). `child_cpu_ms` and
//! `child_peak_rss_bytes` are lower bounds on a cold build, because a
//! compiler that a cache's server runs is not a child of the job. Peak memory
//! is the largest single process, and it is not a sum across processes.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::admit::{Hold, Lane};
use crate::probe::{ChildUsage, Pressure};
use crate::protocol::{Exit, Label};

mod identity;
mod kind;
#[cfg(test)]
pub(crate) mod tests;
mod writer;

pub use self::kind::{Arrival, Place, WORKSPACE_MARKER, kind, options};
pub use self::writer::{LOCK_FILE, Recorder};

/// The format version that each line carries.
pub const VERSION: u32 = 1;

/// The name of the history file in the state directory.
pub const FILE: &str = "jobs.jsonl";

/// The name that the history file takes when it passes [`MAX_BYTES`]. A
/// rotation replaces any earlier file of this name.
pub const ROTATED_FILE: &str = "jobs.1.jsonl";

/// The size in bytes past which the writer rotates the history file, so the
/// two files together hold about twice this.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// The most lines that wait for the writer. A line that finds the channel
/// full is dropped and counted in [`Recorder::dropped`].
pub const CHANNEL: usize = 256;

/// How often the daemon writes a machine line.
pub const MACHINE_EVERY: Duration = Duration::from_secs(60);

/// Returns the wall-clock time in Unix milliseconds. A clock set before 1970
/// gives 0.
#[must_use]
pub fn now_ms() -> u64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH);
    since.map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    })
}

/// How a job ended, as its line says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobEnd {
    /// The job exited with `code`.
    Exited {
        /// The job's exit code.
        code: i32,
    },
    /// A signal ended the job.
    Signalled {
        /// The signal's number.
        signal: i32,
    },
    /// The daemon never learned how the job ended.
    Lost,
    /// The job's client went away before the job ran.
    Left,
}

impl From<Option<Exit>> for JobEnd {
    fn from(exit: Option<Exit>) -> Self {
        match exit {
            Some(Exit::Exited { code }) => Self::Exited { code },
            Some(Exit::Signalled { signal }) => Self::Signalled { signal },
            Some(Exit::Lost) => Self::Lost,
            None => Self::Left,
        }
    }
}

/// The machine's figures across one job's run, gathered from the daemon's
/// readings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tally {
    /// The load when the job started, or `None` when it is unknown.
    load_at_start: Option<f64>,
    /// The sum of the load readings.
    load_sum: f64,
    /// The largest load reading.
    load_max: Option<f64>,
    /// How many load readings the tally holds.
    samples: u64,
    /// How long memory pressure was above normal.
    pressure: Duration,
    /// The most jobs that ran at once.
    running_max: usize,
}

impl Tally {
    /// Returns the tally of a job that starts now, with `load` as the
    /// machine's latest load and `running` jobs running with it counted.
    #[must_use]
    pub fn started(load: Option<f64>, running: usize) -> Self {
        Self {
            load_at_start: load,
            load_sum: load.unwrap_or(0.0),
            load_max: load,
            samples: u64::from(load.is_some()),
            running_max: running,
            ..Self::default()
        }
    }

    /// Returns the tally of a job that a new daemon takes over, whose load
    /// at its start is unknown.
    #[must_use]
    pub fn seated(running: usize) -> Self {
        Self {
            running_max: running,
            ..Self::default()
        }
    }

    /// Adds one reading of the machine, which covers `every` of the run.
    pub fn sample(&mut self, load: f64, pressure: Pressure, every: Duration, running: usize) {
        self.load_sum += load;
        self.load_max = Some(self.load_max.map_or(load, |max| max.max(load)));
        self.samples = self.samples.saturating_add(1);
        if pressure != Pressure::Normal {
            self.pressure = self.pressure.saturating_add(every);
        }
        self.running(running);
    }

    /// Notes that `running` jobs run at this moment.
    pub fn running(&mut self, running: usize) {
        self.running_max = self.running_max.max(running);
    }
}

/// What the daemon knows about a job when it leaves.
#[derive(Debug, Clone, PartialEq)]
pub struct Departure {
    /// The job's label.
    pub label: Label,
    /// The lane the job waited in.
    pub lane: Lane,
    /// How many jobs were ahead of it when it asked.
    pub ahead: usize,
    /// The threads of the job's grant, or `None` when it never ran.
    pub threads: Option<usize>,
    /// How long the job ran, or `None` when it never ran.
    pub ran: Option<Duration>,
    /// How the job ended, or `None` when it never ran.
    pub exit: Option<Exit>,
    /// What the kernel reported of the job's children, when the client sent it.
    pub usage: Option<ChildUsage>,
    /// The machine's figures across the run, or `None` when it never ran.
    pub tally: Option<Tally>,
}

/// One line about a job that has left the daemon.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobLine {
    /// The format version.
    pub v: u32,
    /// When the job asked, in Unix milliseconds.
    pub queued_at_ms: u64,
    /// When the job started, or `null` when it left before it ran.
    pub started_at_ms: Option<u64>,
    /// When the job left the daemon.
    pub ended_at_ms: u64,
    /// Whether a new daemon took over the job, so its wait is unknown.
    pub seated: bool,
    /// The job's label, at most 64 bytes.
    pub label: Label,
    /// The program's file name, and up to two subcommand words when the
    /// config's `kind_tools` lists the program.
    pub kind: String,
    /// The names of the command's options, without their values.
    pub options: Vec<String>,
    /// The final directory name of the main checkout.
    pub repo: Option<String>,
    /// The final directory name of the worktree.
    pub checkout: Option<String>,
    /// Whether the job's build directory had an entry when it asked.
    pub warm: Option<bool>,
    /// The lane the job waited in.
    pub lane: Lane,
    /// The threads of the job's grant.
    pub threads: Option<usize>,
    /// How many jobs were ahead of it when it asked.
    pub ahead: usize,
    /// How the job ended.
    pub exit: JobEnd,
    /// The CPU time of the job's waited-for children, a lower bound.
    pub child_cpu_ms: Option<u64>,
    /// The peak memory of the job's largest waited-for child, a lower bound.
    pub child_peak_rss_bytes: Option<u64>,
    /// The 1-minute load average when the job started.
    pub load_at_start: Option<f64>,
    /// The mean of the 1-minute load average across the run.
    pub load_mean: Option<f64>,
    /// The largest 1-minute load average across the run.
    pub load_max: Option<f64>,
    /// The seconds of the run with memory pressure above normal.
    pub pressure_s: u64,
    /// How many readings the load figures hold.
    pub samples: u64,
    /// The most jobs that ran at once during the run.
    pub running_max: usize,
}

impl JobLine {
    /// Returns the line of a job that arrived as `arrival`, left as
    /// `departure`, and ended at `ended_at_ms`. A job that a new daemon took
    /// over has no known wait, so it counts as starting when it asked.
    #[must_use]
    pub fn new(arrival: Arrival, departure: Departure, ended_at_ms: u64) -> Self {
        let tally = departure.tally.unwrap_or_default();
        let started_at_ms = departure.ran.map(|ran| {
            if arrival.seated {
                arrival.at_ms
            } else {
                let ran = u64::try_from(ran.as_millis()).unwrap_or(u64::MAX);
                ended_at_ms.saturating_sub(ran).max(arrival.at_ms)
            }
        });
        #[expect(
            clippy::cast_precision_loss,
            reason = "the count of one-second readings stays far below 2^52"
        )]
        let load_mean = (tally.samples > 0).then(|| tally.load_sum / tally.samples as f64);
        Self {
            v: VERSION,
            queued_at_ms: arrival.at_ms,
            started_at_ms,
            ended_at_ms,
            seated: arrival.seated,
            label: departure.label,
            kind: arrival.kind,
            options: arrival.options,
            repo: arrival.place.repo,
            checkout: arrival.place.checkout,
            warm: arrival.place.warm,
            lane: departure.lane,
            threads: departure.threads,
            ahead: departure.ahead,
            exit: departure.exit.into(),
            child_cpu_ms: departure.usage.map(|usage| usage.cpu_ms),
            child_peak_rss_bytes: departure.usage.map(|usage| usage.peak_rss_bytes),
            load_at_start: tally.load_at_start,
            load_mean,
            load_max: tally.load_max,
            pressure_s: tally.pressure.as_secs(),
            samples: tally.samples,
            running_max: tally.running_max,
        }
    }
}

/// One line about the machine.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MachineLine {
    /// The format version.
    pub v: u32,
    /// When the daemon wrote the line, in Unix milliseconds.
    pub at_ms: u64,
    /// The 1-minute load average, or `null` when the probe failed.
    pub load_1m: Option<f64>,
    /// The memory pressure, or `null` when the probe failed.
    pub pressure: Option<Pressure>,
    /// How many jobs run.
    pub running: usize,
    /// How many jobs wait.
    pub waiting: usize,
    /// Why the first waiting job waits.
    pub held: Option<Hold>,
    /// The most jobs that the daemon lets run at once.
    pub max_jobs: usize,
    /// The logical cores, or `null` when they are unknown.
    pub cores: Option<usize>,
    /// How many lines the daemon has dropped since it started.
    pub dropped: u64,
}

/// One line of the history file.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Line {
    /// A job that left the daemon.
    Job(Box<JobLine>),
    /// The machine at one moment.
    Machine(MachineLine),
}
