//! The `deadband` command line.

use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, CommandFactory, Parser, Subcommand};
use crumbkit::help::{Effects, Example, Exit, Guide, Leaf};
use crumbkit::{Document, Prose, Sink, ViewFlags};
use schemars::JsonSchema;
use serde::Serialize;

use crate::admit::Lane;
use crate::client::{self, ANSWER_WITHIN};
use crate::daemon::{DAEMON_RUNNING, Log, SOCKET_UNUSABLE};
use crate::environment::Environment;
use crate::probe::{MACHINE_UNREADABLE, MACHINE_UNSUPPORTED, Probe, Sample, Unreadable};
use crate::protocol::Jobs;

mod commands;
#[cfg(test)]
mod tests;

pub use self::commands::{JOB_NOT_FOUND, JOB_NOT_STARTED, NotStarted};

/// Keep a busy machine usable while many heavy jobs compete for it.
#[derive(Debug, Parser)]
#[command(name = "deadband")]
pub struct Cli {
    /// The command to run.
    #[command(subcommand)]
    pub command: Command,

    /// The flags that choose how the result prints.
    #[command(flatten)]
    pub view: ViewFlags,
}

/// The commands of `deadband`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the machine's load, memory pressure and swap, and the daemon's
    /// jobs.
    Status,
    /// Wait until the daemon admits a job, then run it.
    ///
    /// `run` exits with the job's exit status, or with 128 plus the signal's
    /// number when a signal ended the job. It exits with 127 when the
    /// program is not found and with 126 when it is found but cannot start,
    /// as `env` and `timeout` do.
    Run(RunArgs),
    /// Run the daemon that admits jobs, in the foreground.
    Daemon(DaemonArgs),
}

/// The arguments of `deadband run`.
#[derive(Debug, Args)]
pub struct RunArgs {
    /// The label that `deadband status` shows for the job. The default is
    /// the program's name.
    #[arg(long)]
    pub label: Option<String>,

    /// The lane the job waits in. Waiting fast jobs start first and get
    /// one more thread, and normal jobs run at nice 5. The default is the
    /// fast lane when the priority file matches the job, and the normal
    /// lane otherwise.
    #[arg(long, value_enum)]
    pub lane: Option<Lane>,

    /// The command to run, after `--`.
    #[arg(last = true, required = true, value_name = "COMMAND")]
    pub command: Vec<OsString>,
}

/// The arguments of `deadband daemon`.
#[derive(Debug, Args)]
pub struct DaemonArgs {
    /// The most jobs that run at once. The default is a quarter of the
    /// logical cores, rounded up, and at least 1.
    #[arg(long, value_name = "N")]
    pub max_jobs: Option<NonZeroUsize>,
}

/// What the commands run against: the probe, the daemon's socket, the log
/// on standard error, the number of logical cores and the environment.
pub struct Machine<P> {
    /// The probe that reads the machine.
    pub probe: P,
    /// The path of the daemon's socket.
    pub socket: PathBuf,
    /// Where the daemon and `run` write their lines.
    pub log: Log,
    /// The number of logical cores, or `None` when it is unknown.
    pub cores: Option<NonZeroUsize>,
    /// What deadband read from the environment.
    pub environment: Environment,
}

impl<P: fmt::Debug> fmt::Debug for Machine<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Machine")
            .field("probe", &self.probe)
            .field("socket", &self.socket)
            .field("cores", &self.cores)
            .field("environment", &self.environment)
            .finish_non_exhaustive()
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
    pub daemon: Option<Jobs>,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.sample)?;
        let Some(jobs) = &self.daemon else {
            return f.write_str("daemon: not running");
        };
        write!(
            f,
            "daemon: {} running, {} waiting, at most {} at once",
            jobs.running.len(),
            jobs.waiting.len(),
            jobs.max_jobs
        )?;
        if let (Some(job), Some(hold)) = (jobs.waiting.first(), &jobs.held) {
            write!(f, "\n{} waits because: {hold}", job.label)?;
        }
        let running = jobs.running.iter().map(|job| ("running", job));
        for (state, job) in running.chain(jobs.waiting.iter().map(|job| ("waiting", job))) {
            write!(
                f,
                "\n{state}: {} (pid {}): {}",
                job.label, job.pid, job.command
            )?;
        }
        if let Some(reclaimed) = &jobs.reclaim {
            write!(f, "\n{reclaimed}")?;
        }
        Ok(())
    }
}

/// The document that `deadband status` prints.
pub const STATUS: Document<Prose<Status>> = Document::new(
    "deadband",
    "status",
    1,
    "One sample of the machine's 1-minute load average, memory pressure and swap, and the daemon's running and waiting jobs.",
);

/// The exit code of a run that could not read the machine.
pub const EXIT_MACHINE: u8 = 3;

/// The exit code of a daemon that could not listen on its socket.
pub const EXIT_DAEMON: u8 = 4;

/// The exit code of a `deadband run` whose program was found but could not
/// start, as `env` and `timeout` report it.
pub const EXIT_NOT_STARTED: u8 = 126;

/// The exit code of a `deadband run` whose program was not found, as `env`
/// and `timeout` report it.
pub const EXIT_NOT_FOUND: u8 = 127;

/// deadband's guide: the examples, effects and document of each command,
/// and the exit codes with the failures that produce them.
pub const GUIDE: Guide = Guide::new()
    .leaves(&[
        Leaf::new(&["status"])
            .examples(&[
                Example::new(&[], "Print the machine's load, memory pressure and swap"),
                Example::new(&["--json"], "Print the same sample as a JSON document"),
            ])
            .effects(Effects::read_only())
            .document(STATUS),
        Leaf::new(&["run"])
            .examples(&[Example::new(
                &["--label", "tests", "--", "cargo", "test"],
                "Run cargo test once the daemon admits it",
            )])
            .effects(Effects::additive().all_or_none().open_world())
            .prints_no_result(),
        Leaf::new(&["daemon"])
            .examples(&[Example::new(
                &["--max-jobs", "2"],
                "Admit jobs while at most 2 run at once",
            )])
            .effects(Effects::additive())
            .prints_no_result(),
    ])
    .exits(&[
        Exit::new(EXIT_MACHINE, "deadband could not read the machine.")
            .failures(&[&MACHINE_UNREADABLE, &MACHINE_UNSUPPORTED]),
        Exit::new(EXIT_DAEMON, "the daemon could not listen on its socket.")
            .failures(&[&DAEMON_RUNNING, &SOCKET_UNUSABLE]),
        Exit::new(
            EXIT_NOT_STARTED,
            "the program given to `run` was found but could not start.",
        )
        .commands(&[&["run"]])
        .failures(&[&JOB_NOT_STARTED]),
        Exit::new(EXIT_NOT_FOUND, "the program given to `run` was not found.")
            .commands(&[&["run"]])
            .failures(&[&JOB_NOT_FOUND]),
    ]);

/// Parses the process's command line, runs the command against `machine`
/// and prints the result to standard output and standard error.
pub fn run<P: Probe + Clone + Send + 'static>(machine: &Machine<P>) -> ExitCode {
    let (cli, mut output) = match crumbkit::parse::<Cli>(Cli::command(), &GUIDE) {
        Ok(parsed) => parsed,
        Err(stop) => return stop.exit_code(),
    };
    let exit = match cli.command {
        Command::Status => return crumbkit::finish(output, STATUS, status(machine)),
        Command::Run(args) => commands::run(&mut Sink::standard(), &mut output, machine, args),
        Command::Daemon(args) => {
            commands::daemon(&mut Sink::standard(), &mut output, machine, &args)
        }
    };
    crumbkit::events::ended(exit, None)
}

/// Parses `args`, runs the command against `machine` and prints the result
/// to `sink`. Returns the exit code.
pub fn run_to<O: Write, W: Write, T: Into<OsString>, P: Probe + Clone + Send + 'static>(
    sink: &mut Sink<O, W>,
    machine: &Machine<P>,
    args: impl IntoIterator<Item = T>,
) -> u8 {
    let parsed = crumbkit::parse_to::<Cli, _, _, _, _>(sink, Cli::command(), &GUIDE, args);
    let (cli, mut output) = match parsed {
        Ok(parsed) => parsed,
        Err(stop) => return stop.exit(),
    };
    match cli.command {
        Command::Status => crumbkit::finish_to(sink, output, STATUS, status(machine)),
        Command::Run(args) => commands::run(sink, &mut output, machine, args),
        Command::Daemon(args) => commands::daemon(sink, &mut output, machine, &args),
    }
}

/// Takes one sample with the machine's probe, and asks the daemon for its
/// jobs, for `deadband status`.
fn status<P: Probe>(machine: &Machine<P>) -> Result<Prose<Status>, Unreadable> {
    let sample = machine.probe.sample()?;
    let daemon = client::jobs(&machine.socket, ANSWER_WITHIN).ok();
    Ok(Prose::new(Status { sample, daemon }))
}
