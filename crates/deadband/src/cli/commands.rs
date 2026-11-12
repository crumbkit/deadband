//! The commands `deadband run` and `deadband daemon`, which print no result
//! on standard output, and the glue of `deadband service`.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use crumbkit::{Code, Failure, Field, Prose, Recovery, Run, Sink};

use super::service::{self, Install, Report, SERVICE_INSTALL, SERVICE_UNINSTALL, ServiceCommand};
use super::{DaemonArgs, Machine, RunArgs};
use crate::admission::{self, Asked, Notices};
use crate::client::{self, ANSWER_WITHIN};
use crate::daemon::note;
use crate::job;
use crate::probe::{Probe, child_usage};
use crate::protocol::Jobs;
use crate::serve;

/// The code of a program given to `run` that was found but could not start.
pub const JOB_NOT_STARTED: Code = Code::new("deadband", "job", "not_started");

/// The code of a program given to `run` that was not found.
pub const JOB_NOT_FOUND: Code = Code::new("deadband", "job", "not_found");

/// The code of a `status` that lists at least one stuck job.
pub const JOB_STUCK: Code = Code::new("deadband", "job", "stuck");

/// The code of a `status` whose daemon could not read the jobs' processes.
pub const JOB_UNREADABLE: Code = Code::new("deadband", "job", "unreadable");

/// What makes `status` exit 1 about the daemon's jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Flagged {
    /// This many jobs show no activity.
    Stuck(usize),
    /// The daemon could not read the jobs' processes.
    Unreadable,
}

impl Flagged {
    /// Returns what `jobs` flags. A stuck job comes before a failed read.
    pub(super) const fn of(jobs: &Jobs) -> Option<Self> {
        if !jobs.stuck.is_empty() {
            Some(Self::Stuck(jobs.stuck.len()))
        } else if jobs.unreadable {
            Some(Self::Unreadable)
        } else {
            None
        }
    }
}

impl fmt::Display for Flagged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stuck(1) => f.write_str("1 job shows no activity"),
            Self::Stuck(count) => write!(f, "{count} jobs show no activity"),
            Self::Unreadable => f.write_str("deadband could not read the jobs' processes"),
        }
    }
}

impl std::error::Error for Flagged {}

impl Failure for Flagged {
    fn code(&self) -> &'static Code {
        match self {
            Self::Stuck(_) => &JOB_STUCK,
            Self::Unreadable => &JOB_UNREADABLE,
        }
    }

    fn recovery(&self) -> Recovery {
        match self {
            Self::Stuck(_) => Recovery::none(
                "deadband leaves a stuck job running; a job that waits on purpose belongs in stuck_ignore in config.toml",
            ),
            Self::Unreadable => Recovery::Retry,
        }
    }
}

/// A command given to `run` that could not start.
#[derive(Debug)]
pub struct NotStarted {
    /// The command's program.
    pub program: OsString,
    /// The error of the start.
    pub error: io::Error,
}

impl fmt::Display for NotStarted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "deadband could not start {}: {}",
            Path::new(&self.program).display(),
            self.error
        )
    }
}

impl std::error::Error for NotStarted {}

impl Failure for NotStarted {
    fn code(&self) -> &'static Code {
        if self.error.kind() == io::ErrorKind::NotFound {
            &JOB_NOT_FOUND
        } else {
            &JOB_NOT_STARTED
        }
    }

    fn recovery(&self) -> Recovery {
        Recovery::hint("name a program on PATH that this user may run after `--`")
    }

    fn fields(&self) -> Vec<Field<'_>> {
        vec![Field::new("program", self.program.to_string_lossy())]
    }
}

/// Runs `deadband run`: asks the daemon to admit the job, runs it with its
/// threads in the parallelism variables, tells the daemon it ended, and
/// returns its exit code. A normal-lane job runs at nice 5.
///
/// When the daemon cannot be asked, the job starts at once and the log
/// says why in one line. A job inside a granted job, or one that asks to
/// bypass, starts at once without asking.
pub(super) fn run<O: Write, W: Write, P>(
    sink: &mut Sink<O, W>,
    output: &mut Run,
    machine: &Machine<P>,
    args: RunArgs,
) -> u8 {
    let program = args.command.first().cloned().unwrap_or_default();
    let label = args.label.unwrap_or_else(|| {
        Path::new(&program)
            .file_name()
            .unwrap_or(program.as_os_str())
            .to_string_lossy()
            .into_owned()
    });
    let words: Vec<_> = args
        .command
        .iter()
        .map(|word| word.to_string_lossy())
        .collect();
    let command = words.join(" ");
    let log = Arc::clone(&machine.log);
    let notices = Notices::new(move |line| note(&log, format_args!("{line}")));
    let asked = Asked {
        socket: &machine.socket,
        environment: &machine.environment,
        cores: machine.cores,
        label: &label,
        command: &command,
        workspace: None,
        lane: args.lane,
        notices: Some(&notices),
    };
    let mut notify = |line: &str| note(&machine.log, format_args!("{line}"));
    let admitted = admission::admit(&asked, &mut notify);
    admitted.lower_priority(&mut notify);
    let ended = job::run(&args.command, admitted.variables());
    match &ended {
        Ok(status) => admitted.end_measured(*status, child_usage()),
        Err(_) => admitted.end(None),
    }
    finish(sink, output, program, ended)
}

/// Returns the exit code of a job that `ended`, or prints why `program`
/// could not start and returns that failure's exit code.
fn finish<O: Write, W: Write>(
    sink: &mut Sink<O, W>,
    output: &mut Run,
    program: OsString,
    ended: io::Result<std::process::ExitStatus>,
) -> u8 {
    match ended {
        Ok(status) => job::exit_code(status),
        Err(error) => {
            let failure = NotStarted { program, error };
            output.print_failure(sink, &failure);
            output.failure_exit(failure.code())
        }
    }
}

/// Runs `deadband daemon`: listens on the machine's socket and admits jobs
/// until accepting a connection fails, which it prints as a failure.
pub(super) fn daemon<O: Write, W: Write, P: Probe + Clone + Send + 'static>(
    sink: &mut Sink<O, W>,
    output: &mut Run,
    machine: &Machine<P>,
    args: &DaemonArgs,
) -> u8 {
    let failure = serve::run_daemon(machine, args.max_jobs);
    output.print_failure(sink, &failure);
    output.failure_exit(failure.code())
}

/// Runs `deadband top` until the person quits, or prints why it could not.
#[cfg(feature = "tui")]
pub(super) fn top<O: Write, W: Write, P: Probe>(
    sink: &mut Sink<O, W>,
    output: &mut Run,
    machine: &Machine<P>,
) -> u8 {
    match crate::tui::run(machine, output.is_json()) {
        Ok(code) => code,
        Err(failure) => {
            output.print_failure(sink, &failure);
            output.failure_exit(failure.code())
        }
    }
}

/// The question that `deadband service uninstall` asks before it acts.
const UNINSTALL_QUESTION: &str = "Unload deadband's launchd agent and delete its plist?";

/// Runs `deadband service install|uninstall` and prints the result to
/// `sink`. Uninstall asks at the terminal when `terminal` is true, and
/// through `sink` otherwise, which a test passes.
pub(super) fn service<O: Write, W: Write, P>(
    sink: &mut Sink<O, W>,
    mut output: Run,
    machine: &Machine<P>,
    command: &ServiceCommand,
    terminal: bool,
) -> u8 {
    let agent = machine.service.as_ref();
    match command {
        ServiceCommand::Install(args) => {
            let install = Install {
                max_jobs: args.max_jobs,
                restart: args.restart,
                dry_run: output.is_dry_run(),
            };
            // A daemon that does not answer counts as having no jobs.
            let jobs = |socket: &Path| {
                client::jobs(socket, ANSWER_WITHIN)
                    .ok()
                    .map(|jobs| jobs.running.len() + jobs.waiting.len())
            };
            let result = service::install(agent, &install, jobs);
            crumbkit::finish_to(sink, output, SERVICE_INSTALL, result)
        }
        ServiceCommand::Uninstall(args) => {
            let confirmed = if terminal {
                output.confirm(args.yes, UNINSTALL_QUESTION)
            } else {
                output.confirm_to(sink, args.yes, UNINSTALL_QUESTION)
            };
            if let Err(refused) = confirmed {
                let refused = Err::<Prose<Report>, _>(refused);
                return crumbkit::finish_to(sink, output, SERVICE_UNINSTALL, refused);
            }
            let result = service::uninstall(agent, output.is_dry_run());
            crumbkit::finish_to(sink, output, SERVICE_UNINSTALL, result)
        }
    }
}
