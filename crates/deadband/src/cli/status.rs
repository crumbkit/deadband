//! What `deadband status` prints: one sample of the machine and the
//! daemon's jobs, each named by its workspace and label and shown by its
//! short command. The full command line can hold a secret, so neither the
//! text nor the JSON document prints it, except in the text for an older
//! daemon that sends no short command.

use std::fmt;
use std::time::Duration;

use crumbkit::{Document, Prose};
use schemars::JsonSchema;
use serde::Serialize;

use super::service::{DaemonState, LOG};
use crate::admit::Lane;
use crate::name::name;
use crate::probe::Sample;
use crate::protocol::{Job, Jobs, Label, Short};
use crate::stuck::quiet_text;

/// A job as `deadband status` shows it, which leaves out its command line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ShownJob {
    /// The label that `--label` gave, or the program's name.
    pub label: String,
    /// The workspace the job runs in, or absent outside a workspace or from
    /// an older daemon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Label>,
    /// The job's short command, at most three plain words such as `cargo
    /// nextest run`, or absent from an older daemon. It is empty when the
    /// rule kept no word of the command line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short: Option<Short>,
    /// The process ID of the `deadband run` that asked for the job.
    pub pid: u32,
    /// The lane the job waits in.
    pub lane: Lane,
    /// Whether the daemon watches this job's process tree.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub watched: bool,
    /// The working directory of the job, or absent from an older client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The full command line, kept only for the text of an older daemon,
    /// which sends no short command. The JSON document never holds it.
    #[serde(skip)]
    #[schemars(skip)]
    older_command: Option<String>,
}

impl From<Job> for ShownJob {
    fn from(job: Job) -> Self {
        Self {
            older_command: job.short.is_none().then_some(job.command),
            label: job.label,
            workspace: job.workspace,
            short: job.short,
            pid: job.pid,
            lane: job.lane,
            watched: job.watched,
            cwd: job.cwd,
        }
    }
}

impl ShownJob {
    /// Returns the job's name: its workspace and its label, joined.
    fn name(&self) -> String {
        name(
            self.workspace.as_ref().map(Label::as_str),
            &self.label,
            self.short.as_ref().map(Short::as_str),
        )
    }

    /// Returns the text after the pid: the short command, or the full
    /// command line from an older daemon, or `None` when the short command
    /// is empty.
    fn command(&self) -> Option<&str> {
        match &self.short {
            Some(short) => Some(short.as_str()).filter(|text| !text.is_empty()),
            None => self.older_command.as_deref(),
        }
    }
}

/// What `deadband status` prints: one sample of the machine, and the
/// daemon's jobs.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Status {
    /// One sample of the machine.
    #[serde(flatten)]
    pub sample: Sample,
    /// The daemon's running and waiting jobs, or `null` when no daemon
    /// answers.
    pub daemon: Option<Jobs<ShownJob>>,
    /// Whether the daemon runs, or why it does not: its launchd agent is
    /// not installed, not loaded, loaded but silent, or launchd could not be
    /// asked. It is absent when no daemon answers and deadband cannot say
    /// why, such as on a platform without launchd.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_state: Option<DaemonState>,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.sample)?;
        let Some(jobs) = &self.daemon else {
            return f.write_str(match self.daemon_state {
                Some(DaemonState::NotInstalled) => {
                    "daemon: not installed; run deadband service install"
                }
                Some(DaemonState::NotLoaded) => {
                    "daemon: installed but not loaded; run deadband service install"
                }
                Some(DaemonState::LoadedSilent) => {
                    return write!(f, "daemon: loaded but not answering; see ~/{LOG}");
                }
                Some(DaemonState::LaunchdUnasked) => {
                    "daemon: not running, and launchd could not be asked whether it loaded the agent"
                }
                Some(DaemonState::Running) | None => "daemon: not running",
            });
        };
        write!(
            f,
            "daemon: {} running, {} waiting, at most {} at once",
            jobs.running.len(),
            jobs.waiting.len(),
            jobs.max_jobs
        )?;
        for stuck in &jobs.stuck {
            let shown = jobs.running.iter().find(|job| job.pid == stuck.pid);
            write!(
                f,
                "\nstuck: {} (pid {}): no CPU, new process or disk use for {}",
                shown.map_or_else(|| stuck.label.clone(), ShownJob::name),
                stuck.pid,
                quiet_text(Duration::from_secs(stuck.quiet_seconds))
            )?;
        }
        if let (false, Some(busiest)) = (jobs.stuck.is_empty(), &jobs.busiest) {
            write!(f, "\n{busiest}")?;
        }
        if jobs.unreadable {
            f.write_str(
                "\ndeadband could not read the jobs' processes, so it cannot say whether any job is stuck",
            )?;
        }
        if let (Some(job), Some(hold)) = (jobs.waiting.first(), &jobs.held) {
            write!(f, "\n{} waits because: {hold}", job.name())?;
        }
        let is_stuck = |job: &&ShownJob| jobs.stuck.iter().any(|stuck| stuck.pid == job.pid);
        let (stuck, others): (Vec<_>, Vec<_>) = jobs.running.iter().partition(is_stuck);
        let running = stuck.into_iter().chain(others).map(|job| ("running", job));
        for (state, job) in running.chain(jobs.waiting.iter().map(|job| ("waiting", job))) {
            write!(f, "\n{state}: {} (pid {})", job.name(), job.pid)?;
            if let Some(command) = job.command() {
                write!(f, ": {command}")?;
            }
        }
        if let Some(reclaimed) = &jobs.reclaim {
            write!(f, "\n{reclaimed}")?;
        }
        if let Some(reclaiming) = &jobs.reclaiming {
            write!(f, "\n{reclaiming}")?;
        }
        Ok(())
    }
}

/// The document that `deadband status` prints.
pub const STATUS: Document<Prose<Status>> = Document::new(
    "deadband",
    "status",
    2,
    "One sample of the machine's 1-minute load average, memory pressure and swap, and the daemon's running and waiting jobs, each with its workspace and short command.",
);
