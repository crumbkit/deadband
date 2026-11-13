//! What `deadband service install` and `uninstall` print: one line per
//! step, and in a dry run of install the plist it would write.

use std::fmt;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;

use super::LABEL;

/// The daemon's log, under the home directory. The launch agent's plist sends
/// the daemon's standard error to it.
pub(super) const LOG: &str = "Library/Logs/deadband.log";

/// The jobs that a daemon answered with: those it runs and those that wait
/// for a place.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// The jobs that run.
    pub running: usize,
    /// The jobs that wait to run.
    pub waiting: usize,
}

impl Counts {
    /// Returns the running and waiting jobs together.
    #[must_use]
    pub const fn total(self) -> usize {
        self.running + self.waiting
    }
}

/// One step that install or uninstall took or skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// The plist was written.
    Wrote {
        /// The plist's path.
        path: PathBuf,
    },
    /// The plist already held what install would write.
    Unchanged {
        /// The plist's path.
        path: PathBuf,
    },
    /// The plist already held what install would write, launchd had loaded
    /// it, and nothing changed.
    UpToDate {
        /// The plist's path.
        path: PathBuf,
    },
    /// launchd loaded the agent. `answers` says whether a daemon answered on
    /// the socket within a few seconds, and is absent in a dry run.
    Loaded {
        /// Whether the daemon answered.
        #[serde(skip_serializing_if = "Option::is_none")]
        answers: Option<bool>,
    },
    /// The daemon held these jobs when the step after this one stopped it.
    Counted {
        /// The jobs that run.
        running: usize,
        /// The jobs that wait for a place.
        waiting: usize,
    },
    /// The daemon did not answer before the step after this one stopped it,
    /// so its jobs are unknown.
    NoCount,
    /// These files stay after uninstall. The person deletes them to remove
    /// deadband's data.
    Stays {
        /// The files and directories that exist and stay.
        paths: Vec<PathBuf>,
    },
    /// launchd unloaded the agent, which stopped its daemon.
    Unloaded,
    /// launchd had not loaded the agent.
    NotLoaded,
    /// The daemon has jobs, so install left the old plist in place and the
    /// daemon running.
    Kept {
        /// The daemon's running and waiting jobs.
        jobs: usize,
    },
    /// launchd has loaded the agent and its daemon did not answer, so
    /// install left the old plist in place and the daemon running.
    Unanswered,
    /// The plist was deleted.
    Deleted {
        /// The plist's path.
        path: PathBuf,
    },
    /// No plist existed to delete.
    Absent {
        /// The plist's path.
        path: PathBuf,
    },
}

/// What install or uninstall did: one line per step, and in a dry run of
/// install the plist it would write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Report {
    /// The steps, in the order they ran.
    pub steps: Vec<Step>,
    /// The plist that a dry run of install would write, or absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plist: Option<String>,
    /// Whether the run was a dry run, which the text says on each line.
    #[serde(skip)]
    #[schemars(skip)]
    dry_run: bool,
    /// The home directory, which the text shows as `~`.
    #[serde(skip)]
    #[schemars(skip)]
    home: PathBuf,
}

impl Report {
    /// Returns a report with no steps yet.
    pub(super) fn new(home: &Path, dry_run: bool) -> Self {
        Self {
            steps: Vec::new(),
            plist: None,
            dry_run,
            home: home.to_path_buf(),
        }
    }

    /// Returns `path` with the home directory shown as `~`.
    fn shown(&self, path: &Path) -> String {
        path.strip_prefix(&self.home).map_or_else(
            |_| path.display().to_string(),
            |rest| format!("~/{}", rest.display()),
        )
    }

    /// Returns `done` for a step that ran, or `would {now}` in a dry run.
    fn verb(&self, now: &str, done: &'static str) -> String {
        if self.dry_run {
            format!("would {now}")
        } else {
            done.to_owned()
        }
    }

    /// Returns the line of `step`.
    fn line(&self, step: &Step) -> String {
        match step {
            Step::Wrote { path } => format!("{} {}", self.verb("write", "wrote"), self.shown(path)),
            Step::Unchanged { path } => format!("left {} unchanged", self.shown(path)),
            Step::Loaded { answers } => {
                let loaded = format!("{} {LABEL}", self.verb("load", "loaded"));
                match answers {
                    None => loaded,
                    Some(true) => format!("{loaded}, daemon answers"),
                    Some(false) => format!(
                        "{loaded}, daemon does not answer; see {}",
                        self.shown(&self.home.join(LOG))
                    ),
                }
            }
            Step::Counted { running, waiting } => format!(
                "the daemon has {running} running and {waiting} waiting {}; the running jobs keep running and the waiting jobs start at once",
                if *waiting == 1 { "job" } else { "jobs" }
            ),
            Step::NoCount => {
                "the daemon did not answer, so deadband cannot say how many jobs it holds".to_owned()
            }
            Step::Stays { paths } => {
                let shown: Vec<String> = paths.iter().map(|path| self.shown(path)).collect();
                format!("{} in place: {}", self.verb("leave", "left"), shown.join(", "))
            }
            Step::UpToDate { path } => format!(
                "{} is up to date and {LABEL} is loaded, so nothing changed",
                self.shown(path)
            ),
            Step::Unloaded => format!("{} {LABEL}", self.verb("unload", "unloaded")),
            Step::NotLoaded => format!("{LABEL} was not loaded"),
            Step::Kept { jobs } => format!(
                "the daemon has {jobs} {}, so install left the old plist in place; the new plist waits for deadband service install when no job runs, or for --restart",
                if *jobs == 1 { "job" } else { "jobs" }
            ),
            Step::Unanswered => "the daemon did not answer, so install left the old plist in place; the new plist waits for deadband service install when the daemon answers with no jobs, or for --restart".to_owned(),
            Step::Deleted { path } => {
                format!("{} {}", self.verb("delete", "deleted"), self.shown(path))
            }
            Step::Absent { path } => format!("{} does not exist", self.shown(path)),
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(plist) = &self.plist {
            f.write_str(plist)?;
        }
        let lines: Vec<String> = self.steps.iter().map(|step| self.line(step)).collect();
        f.write_str(&lines.join("\n"))
    }
}
