//! How the daemon admits jobs, and its default limit.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::Duration;

use super::{FIRST_LINE_WITHIN, SAMPLE_EVERY};
use crate::history::MACHINE_EVERY;
use crate::job::busiest::PS_COMMAND;
use crate::reclaim;

/// How the daemon admits jobs.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The most jobs that run at once.
    pub max_jobs: NonZeroUsize,
    /// The number of logical cores, or `None` when it is unknown.
    pub cores: Option<NonZeroUsize>,
    /// The home directory, whose disk the daemon reads, or `None` when it
    /// is unknown and the disk holds no job.
    pub home: Option<PathBuf>,
    /// The priority file. While it holds an entry, each lane leaves one
    /// place open for the other.
    pub priority_file: Option<PathBuf>,
    /// How often the daemon samples the probe.
    pub sample_every: Duration,
    /// How long the daemon waits for a new connection's first line.
    pub first_line_within: Duration,
    /// The reclaimers and when the daemon asks them.
    pub reclaim: reclaim::Config,
    /// How the daemon watches jobs for activity.
    pub watching: Watching,
    /// The directory of the history file, or `None` when the daemon records
    /// no job.
    pub history: Option<PathBuf>,
    /// How often the daemon writes a machine line to the history file.
    pub machine_every: Duration,
}

/// How the daemon watches jobs for activity.
#[derive(Debug, Clone)]
pub struct Watching {
    /// How often the daemon reads the process tree of each watched job.
    pub every: Duration,
    /// The command that lists every process.
    pub census: Vec<String>,
    /// How long the daemon waits for that command.
    pub census_within: Duration,
    /// How often that command runs while a job stays stuck.
    pub census_every: Duration,
    /// How long the daemon waits for one run of `on_stuck` before it logs
    /// that the command is late.
    pub on_stuck_within: Duration,
}

impl Default for Watching {
    fn default() -> Self {
        Self {
            every: Duration::from_secs(15),
            census: PS_COMMAND.map(str::to_owned).to_vec(),
            census_within: Duration::from_secs(5),
            census_every: Duration::from_secs(60),
            on_stuck_within: Duration::from_secs(60),
        }
    }
}

impl Settings {
    /// Returns the settings with `max_jobs`, `cores`, `home` and
    /// `priority_file`, the default sampling interval, [`SAMPLE_EVERY`],
    /// the default wait for a first line, [`FIRST_LINE_WITHIN`], no
    /// reclaimer and no history.
    #[must_use]
    pub fn new(
        max_jobs: NonZeroUsize,
        cores: Option<NonZeroUsize>,
        home: Option<PathBuf>,
        priority_file: Option<PathBuf>,
    ) -> Self {
        Self {
            max_jobs,
            cores,
            home,
            priority_file,
            sample_every: SAMPLE_EVERY,
            first_line_within: FIRST_LINE_WITHIN,
            reclaim: reclaim::Config::default(),
            watching: Watching::default(),
            history: None,
            machine_every: MACHINE_EVERY,
        }
    }
}

/// Returns the default maximum number of jobs: a quarter of the logical
/// cores that `cores` gives, rounded up, and at least 1. With 14 cores it
/// is 4 jobs of 3 threads each.
#[must_use]
pub fn default_max_jobs(cores: Option<NonZeroUsize>) -> NonZeroUsize {
    let quarter = cores.map_or(1, |cores| cores.get().div_ceil(4));
    NonZeroUsize::new(quarter).unwrap_or(NonZeroUsize::MIN)
}
