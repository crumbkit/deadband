//! What `deadband service install` and `uninstall` print: one line per
//! step, and in a dry run of install the plist it would write.

use std::fmt;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;

use super::LABEL;

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
    /// launchd loaded the agent.
    Loaded,
    /// launchd had already loaded the agent and nothing changed.
    AlreadyLoaded,
    /// launchd unloaded the agent, which stopped its daemon.
    Unloaded,
    /// launchd had not loaded the agent.
    NotLoaded,
    /// The daemon has jobs, so install left it running with the old plist.
    Kept {
        /// The daemon's running and waiting jobs.
        jobs: usize,
    },
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
            Step::Loaded => format!("{} {LABEL}", self.verb("load", "loaded")),
            Step::AlreadyLoaded => format!("{LABEL} is already loaded, so nothing changed"),
            Step::Unloaded => format!("{} {LABEL}", self.verb("unload", "unloaded")),
            Step::NotLoaded => format!("{LABEL} was not loaded"),
            Step::Kept { jobs } => format!(
                "the daemon has {jobs} {}, so it keeps running with the old plist; run deadband service install again when no job runs, or add --restart",
                if *jobs == 1 { "job" } else { "jobs" }
            ),
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
