//! `deadband service install` and `deadband service uninstall`, which set
//! up and remove the launchd agent that runs `deadband daemon` at login and
//! again whenever it stops, and the daemon's state that `deadband status`
//! names when no daemon answers.
//!
//! The agent moves between three states: not installed, when no plist
//! exists; installed but not loaded, when the plist exists and launchd has
//! not loaded it; and loaded. Install replaces the plist only by an atomic
//! rename, and never restarts a daemon that has jobs unless `--restart` is
//! given. Uninstall deletes the plist only after `launchctl bootout`
//! succeeded or launchd had not loaded the agent.

use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, Subcommand};
use crumbkit::{Document, DryRun, Prose, Yes};
use schemars::JsonSchema;
use serde::Serialize;

use crate::job::launchd::Launchd;

mod failed;
pub mod plist;
mod report;
#[cfg(test)]
mod tests;

pub use self::failed::{
    SERVICE_LAUNCHCTL_FAILED, SERVICE_PLIST_UNREADABLE, SERVICE_UNLOCATED, SERVICE_UNSUPPORTED,
    SERVICE_WRITE_FAILED, ServiceFailed,
};
pub use self::report::{Counts, Report, Step};

/// The label of deadband's launch agent.
pub const LABEL: &str = "io.github.crumbkit.deadband";

/// The commands for deadband's launchd service.
#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    /// Install the launchd agent that runs the daemon at login and restarts
    /// it when it stops.
    Install(InstallArgs),
    /// Unload the launchd agent and delete its plist. Running jobs keep
    /// running.
    Uninstall(UninstallArgs),
}

/// The arguments of `deadband service install`.
#[derive(Debug, Args)]
pub struct InstallArgs {
    /// The most jobs that the daemon runs at once, passed to `deadband
    /// daemon`. The default is the daemon's own.
    #[arg(long, value_name = "N")]
    pub max_jobs: Option<NonZeroUsize>,

    /// Reload a changed agent even while the daemon has running or waiting
    /// jobs. The jobs keep running.
    #[arg(long)]
    pub restart: bool,

    /// The dry-run flag, which crumbkit reads.
    #[command(flatten)]
    pub dry_run: DryRun,
}

/// The arguments of `deadband service uninstall`.
#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// The flag that skips the question.
    #[command(flatten)]
    pub yes: Yes,

    /// The dry-run flag, which crumbkit reads.
    #[command(flatten)]
    pub dry_run: DryRun,
}

/// What install does, as the command line asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Install {
    /// The `--max-jobs` that the daemon gets.
    pub max_jobs: Option<NonZeroUsize>,
    /// Whether to reload a changed agent while the daemon has jobs.
    pub restart: bool,
    /// Whether to report the steps and change nothing.
    pub dry_run: bool,
}

/// Where deadband's launch agent lives and how to reach launchd. The
/// machine has one only on macOS.
#[derive(Debug, Clone)]
pub struct LaunchAgent {
    /// The home directory, or `None` when the environment names none.
    pub home: Option<PathBuf>,
    /// The running binary's path as invoked, or `None` when it is unknown.
    pub binary: Option<PathBuf>,
    /// The user's id, which names the launchd domain `gui/<uid>`.
    pub uid: u32,
    /// The launchd that loads and unloads the agent.
    pub launchd: Arc<dyn Launchd>,
}

impl LaunchAgent {
    /// Returns the launchd domain, such as `gui/501`.
    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    /// Returns the service target, such as `gui/501/io.github.crumbkit.deadband`.
    fn target(&self) -> String {
        format!("gui/{}/{LABEL}", self.uid)
    }
}

/// Returns the plist's path under `home`.
fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// The daemon's state, which `deadband status --json` gives as
/// `daemon_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DaemonState {
    /// A daemon answers on the socket.
    Running,
    /// The plist exists and launchd has not loaded it.
    NotLoaded,
    /// No plist exists.
    NotInstalled,
}

/// Returns the agent's state when no daemon answers: not installed or not
/// loaded, or `None` when launchd has loaded it or cannot say.
#[must_use]
pub fn state(agent: &LaunchAgent) -> Option<DaemonState> {
    let path = plist_path(agent.home.as_deref()?);
    if !path.exists() {
        return Some(DaemonState::NotInstalled);
    }
    match agent.launchd.is_loaded(&agent.target()) {
        Ok(false) => Some(DaemonState::NotLoaded),
        Ok(true) | Err(_) => None,
    }
}

/// Installs the agent as `install` asks. `jobs` returns the running and
/// waiting jobs of the daemon on the socket it is given, or `None` when no
/// daemon answers there.
///
/// # Errors
///
/// Returns [`ServiceFailed`] for the step that failed. The steps before it
/// took effect.
pub fn install(
    agent: Option<&LaunchAgent>,
    install: &Install,
    mut jobs: impl FnMut(&Path) -> Option<Counts>,
) -> Result<Prose<Report>, ServiceFailed> {
    let agent = agent.ok_or(ServiceFailed::Unsupported)?;
    let home = located(agent.home.as_deref(), "the home directory")?;
    let binary = located(agent.binary.as_deref(), "the path of the deadband binary")?;
    let path = plist_path(home);
    let temp_dir = agent.launchd.temp_dir().map_err(ServiceFailed::Launchctl)?;
    let text = plist::render(binary, install.max_jobs, home, &temp_dir);
    let existing = read_existing(&path)?;
    let loaded = agent
        .launchd
        .is_loaded(&agent.target())
        .map_err(ServiceFailed::Launchctl)?;
    let mut report = Report::new(home, install.dry_run);
    if install.dry_run {
        report.plist = Some(text.clone());
    }
    let changed = existing.as_deref() != Some(text.as_str());
    // The daemon that launchd runs listens under the directory that getconf
    // prints, whatever the caller's TMPDIR says.
    let socket =
        crate::socket::path(|name| (name == "TMPDIR").then(|| temp_dir.clone().into_os_string()));
    if loaded {
        if !changed && !install.restart {
            report.steps.push(Step::UpToDate { path });
            return Ok(Prose::new(report));
        }
        // The old plist stays when the reload waits, so a later install
        // still sees the change. A loaded agent whose daemon does not
        // answer may hold jobs, so it counts as busy. With --restart the
        // reload goes ahead, and the report says what the daemon held.
        match (jobs(&socket), install.restart) {
            (Some(counts), true) => report.steps.push(Step::Counted {
                running: counts.running,
                waiting: counts.waiting,
            }),
            (None, true) => report.steps.push(Step::NoCount),
            (Some(counts), false) if counts.total() == 0 => {}
            (Some(counts), false) => {
                report.steps.push(Step::Kept {
                    jobs: counts.total(),
                });
                return Ok(Prose::new(report));
            }
            (None, false) => {
                report.steps.push(Step::Unanswered);
                return Ok(Prose::new(report));
            }
        }
        if !install.dry_run {
            agent
                .launchd
                .bootout(&agent.target())
                .map_err(ServiceFailed::Launchctl)?;
            wait_until_unloaded(agent);
        }
        report.steps.push(Step::Unloaded);
    }
    if changed {
        if !install.dry_run {
            write(&path, &text)?;
        }
        report.steps.push(Step::Wrote { path: path.clone() });
    } else {
        report.steps.push(Step::Unchanged { path: path.clone() });
    }
    let mut answers = None;
    if !install.dry_run {
        agent
            .launchd
            .bootstrap(&agent.domain(), &path)
            .map_err(ServiceFailed::Launchctl)?;
        answers = Some(wait_for_answer(agent, || jobs(&socket).is_some()));
    }
    report.steps.push(Step::Loaded { answers });
    Ok(Prose::new(report))
}

/// The most times that install asks a freshly loaded daemon for its jobs,
/// [`PAUSE`](crate::job::launchd::PAUSE) apart, about two seconds in all.
const ANSWER_POLLS: u32 = 20;

/// Returns whether `answers` becomes true within [`ANSWER_POLLS`] asks, which
/// is whether the daemon that launchd started listens on its socket.
fn wait_for_answer(agent: &LaunchAgent, mut answers: impl FnMut() -> bool) -> bool {
    for _ in 0..ANSWER_POLLS {
        if answers() {
            return true;
        }
        agent.launchd.pause();
    }
    false
}

/// Unloads the agent and deletes its plist, or with `dry_run` reports the
/// steps and changes nothing.
///
/// # Errors
///
/// Returns [`ServiceFailed`] for the step that failed. A failed bootout
/// keeps the plist.
pub fn uninstall(
    agent: Option<&LaunchAgent>,
    dry_run: bool,
    daemon: Option<Counts>,
    stays: &[PathBuf],
) -> Result<Prose<Report>, ServiceFailed> {
    let agent = agent.ok_or(ServiceFailed::Unsupported)?;
    let home = located(agent.home.as_deref(), "the home directory")?;
    let path = plist_path(home);
    let mut report = Report::new(home, dry_run);
    if agent
        .launchd
        .is_loaded(&agent.target())
        .map_err(ServiceFailed::Launchctl)?
    {
        report
            .steps
            .push(daemon.map_or(Step::NoCount, |counts| Step::Counted {
                running: counts.running,
                waiting: counts.waiting,
            }));
        if !dry_run {
            agent
                .launchd
                .bootout(&agent.target())
                .map_err(ServiceFailed::Launchctl)?;
        }
        report.steps.push(Step::Unloaded);
    } else {
        report.steps.push(Step::NotLoaded);
    }
    if path.exists() {
        if !dry_run {
            std::fs::remove_file(&path).map_err(|error| ServiceFailed::Delete {
                path: path.clone(),
                error,
            })?;
        }
        report.steps.push(Step::Deleted { path });
    } else {
        report.steps.push(Step::Absent { path });
    }
    let paths: Vec<PathBuf> = stays
        .iter()
        .cloned()
        .chain([home.join(report::LOG)])
        .filter(|stay| stay.exists())
        .collect();
    if !paths.is_empty() {
        report.steps.push(Step::Stays { paths });
    }
    Ok(Prose::new(report))
}

/// The most `launchctl print` polls that install makes after a bootout,
/// [`PAUSE`](crate::job::launchd::PAUSE) apart, about five seconds in all.
const UNLOAD_POLLS: u32 = 50;

/// Waits until launchd no longer lists the agent, which happens once the
/// booted-out daemon has exited, so the bootstrap that follows is not
/// refused. After [`UNLOAD_POLLS`] polls, or when a poll fails, it stops
/// waiting and the bootstrap reports what launchd says.
fn wait_until_unloaded(agent: &LaunchAgent) {
    for _ in 0..UNLOAD_POLLS {
        match agent.launchd.is_loaded(&agent.target()) {
            Ok(true) => agent.launchd.pause(),
            Ok(false) | Err(_) => return,
        }
    }
}

/// Returns `found`, or the refusal that names `what` when it is `None`.
fn located<'a>(found: Option<&'a Path>, what: &'static str) -> Result<&'a Path, ServiceFailed> {
    found.ok_or(ServiceFailed::Unlocated { what })
}

/// Returns the text of the plist at `path`, or `None` when there is none.
fn read_existing(path: &Path) -> Result<Option<String>, ServiceFailed> {
    let unreadable = |reason: String| ServiceFailed::Unreadable {
        path: path.to_path_buf(),
        reason,
    };
    match std::fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8(bytes)
                .map_err(|_| unreadable("it is not UTF-8 text".to_owned()))?;
            if plist::is_plist(&text) {
                Ok(Some(text))
            } else {
                Err(unreadable("it is not an XML property list".to_owned()))
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(unreadable(error.to_string())),
    }
}

/// Writes `text` to `path` through a temporary file and a rename, after it
/// makes the plist's directory.
fn write(path: &Path, text: &str) -> Result<(), ServiceFailed> {
    let failed = |error| ServiceFailed::Write {
        path: path.to_path_buf(),
        error,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    crumbkit::files::write_atomically(path, text).map_err(failed)
}

/// The document that `deadband service install` prints.
pub const SERVICE_INSTALL: Document<Prose<Report>> = Document::new(
    "deadband",
    "service-install",
    1,
    "The steps that deadband service install took or skipped, and in a dry run the plist it would write.",
);

/// The document that `deadband service uninstall` prints.
pub const SERVICE_UNINSTALL: Document<Prose<Report>> = Document::new(
    "deadband",
    "service-uninstall",
    1,
    "The steps that deadband service uninstall took or skipped.",
);
