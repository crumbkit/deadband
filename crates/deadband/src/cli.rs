//! The `deadband` command line.

use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use crumbkit::{NoInput, Prose, Sink, ViewFlags};

use crate::admit::Lane;
use crate::client::{self, ANSWER_WITHIN};
use crate::daemon::Log;
use crate::environment::Environment;
use crate::probe::{Probe, Unreadable};

mod commands;
mod explore;
pub mod service;
mod setup;
mod status;
#[cfg(test)]
mod tests;
mod tool;

use self::commands::Flagged;
pub use self::commands::{JOB_NOT_FOUND, JOB_NOT_STARTED, JOB_STUCK, JOB_UNREADABLE, NotStarted};
pub use self::explore::{CommandsCommand, SchemasCommand};
pub use self::service::{LaunchAgent, ServiceCommand};
pub use self::setup::{CompletionCommand, SKILL};
pub use self::status::{STATUS, ShownJob, Status};
#[cfg(feature = "tui")]
pub use self::tool::EXIT_TOP;
pub use self::tool::{
    EXIT_DAEMON, EXIT_MACHINE, EXIT_NOT_FOUND, EXIT_NOT_STARTED, EXIT_SERVICE, TOOL,
};
pub use crumbkit_skills::SkillsCommand;

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

    /// The flag that says no person can answer a question.
    #[command(flatten)]
    pub no_input: NoInput,
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
    ///
    /// `daemon` exits with 0 when another daemon already serves the socket,
    /// so launchd does not start it again.
    Daemon(DaemonArgs),
    /// Install or uninstall the launchd agent that runs the daemon at login.
    Service {
        /// What to do with the agent.
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Show the last ten minutes of jobs, load and holds in the terminal.
    #[cfg(feature = "tui")]
    Top,
    /// Explore deadband's commands.
    Commands {
        /// The command to run.
        #[command(subcommand)]
        command: CommandsCommand,
    },
    /// Explore the JSON documents that deadband prints and the files it
    /// reads.
    Schemas {
        /// The command to run.
        #[command(subcommand)]
        command: SchemasCommand,
    },
    /// Give coding agents deadband's skill.
    Skills {
        /// What to do with the skill.
        #[command(subcommand)]
        command: SkillsCommand,
    },
    /// Set up deadband's completion in a shell.
    Completion {
        /// What to do with the completion.
        #[command(subcommand)]
        command: CompletionCommand,
    },
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
    /// The directory that deadband owns for the socket, which the daemon
    /// makes and checks before it listens, or `None` when a variable or the
    /// per-user directory named the socket.
    pub socket_directory: Option<PathBuf>,
    /// Where the daemon and `run` write their lines.
    pub log: Log,
    /// The number of logical cores, or `None` when it is unknown.
    pub cores: Option<NonZeroUsize>,
    /// What deadband read from the environment.
    pub environment: Environment,
    /// deadband's launchd agent, or `None` on a platform without launchd.
    pub service: Option<LaunchAgent>,
}

impl<P: fmt::Debug> fmt::Debug for Machine<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Machine")
            .field("probe", &self.probe)
            .field("socket", &self.socket)
            .field("socket_directory", &self.socket_directory)
            .field("cores", &self.cores)
            .field("environment", &self.environment)
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

/// Parses the process's command line, runs the command against `machine`
/// and prints the result to standard output and standard error.
pub fn run<P: Probe + Clone + Send + 'static>(machine: &Machine<P>) -> ExitCode {
    let (cli, mut output) = match crumbkit::parse::<Cli>(&TOOL) {
        Ok(parsed) => parsed,
        Err(stop) => return stop.exit_code(),
    };
    let exit = match cli.command {
        Command::Status => return crumbkit::finish(output, STATUS, status(machine)),
        Command::Run(args) => commands::run(&mut Sink::standard(), &mut output, machine, args),
        Command::Daemon(args) => {
            commands::daemon(&mut Sink::standard(), &mut output, machine, &args)
        }
        Command::Service { command } => {
            commands::service(&mut Sink::standard(), output, machine, &command, true)
        }
        #[cfg(feature = "tui")]
        Command::Top => commands::top(&mut Sink::standard(), &mut output, machine),
        Command::Commands { command } => explore::commands(&mut Sink::standard(), output, &command),
        Command::Schemas { command } => explore::schemas(&mut Sink::standard(), output, &command),
        Command::Skills { command } => {
            crumbkit_skills::run_to(&mut Sink::standard(), output, &[SKILL], &command)
        }
        Command::Completion { command } => {
            setup::completion(&mut Sink::standard(), output, &command)
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
    let parsed = crumbkit::parse_to::<Cli>(sink, &TOOL, args);
    let (cli, mut output) = match parsed {
        Ok(parsed) => parsed,
        Err(stop) => return stop.exit(),
    };
    match cli.command {
        Command::Status => crumbkit::finish_to(sink, output, STATUS, status(machine)),
        Command::Run(args) => commands::run(sink, &mut output, machine, args),
        Command::Daemon(args) => commands::daemon(sink, &mut output, machine, &args),
        Command::Service { command } => commands::service(sink, output, machine, &command, false),
        #[cfg(feature = "tui")]
        Command::Top => commands::top(sink, &mut output, machine),
        Command::Commands { command } => explore::commands(sink, output, &command),
        Command::Schemas { command } => explore::schemas(sink, output, &command),
        Command::Skills { command } => crumbkit_skills::run_to(sink, output, &[SKILL], &command),
        Command::Completion { command } => setup::completion(sink, output, &command),
    }
}

/// Takes one sample with the machine's probe, and asks the daemon for its
/// jobs, for `deadband status`.
fn status<P: Probe>(machine: &Machine<P>) -> Result<Prose<Status>, Unreadable> {
    let sample = machine.probe.sample()?;
    let daemon = client::jobs(&machine.socket, ANSWER_WITHIN).ok();
    let failures = daemon
        .as_ref()
        .and_then(Flagged::of)
        .into_iter()
        .map(Into::into)
        .collect();
    let daemon_state = match &daemon {
        Some(_) => Some(service::DaemonState::Running),
        None => machine.service.as_ref().and_then(service::state),
    };
    let daemon = daemon.map(|jobs| jobs.map(ShownJob::from));
    let status = Status {
        sample,
        daemon,
        daemon_state,
    };
    Ok(Prose::with_failures(status, failures))
}
