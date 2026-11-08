//! The commands `deadband run` and `deadband daemon`, which print no result
//! on standard output.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::Path;

use crumbkit::{Code, Failure, Output, Recovery, Sink};

use super::{DaemonArgs, Machine, RunArgs};
use crate::admission::{self, Asked};
use crate::daemon::note;
use crate::job;
use crate::probe::Probe;
use crate::serve;

/// The code of a program given to `run` that was found but could not start.
pub const JOB_NOT_STARTED: Code = Code::new("deadband", "job", "not_started");

/// The code of a program given to `run` that was not found.
pub const JOB_NOT_FOUND: Code = Code::new("deadband", "job", "not_found");

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
        Recovery::None {
            reason: "the command after `--` must name a program on PATH that this user may run"
                .to_owned(),
        }
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
    output: &mut Output,
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
    let asked = Asked {
        socket: &machine.socket,
        environment: &machine.environment,
        cores: machine.cores,
        label: &label,
        command: &command,
        lane: args.lane,
    };
    let mut notify = |line: &str| note(&machine.log, format_args!("{line}"));
    let admitted = admission::admit(&asked, &mut notify);
    admitted.lower_priority(&mut notify);
    let ended = job::run(&args.command, admitted.variables());
    let exit_code = ended.as_ref().ok().and_then(|status| status.code());
    admitted.end(exit_code);
    finish(sink, output, program, ended)
}

/// Returns the exit code of a job that `ended`, or prints why `program`
/// could not start and returns that failure's exit code.
fn finish<O: Write, W: Write>(
    sink: &mut Sink<O, W>,
    output: &mut Output,
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
    output: &mut Output,
    machine: &Machine<P>,
    args: &DaemonArgs,
) -> u8 {
    let failure = serve::run_daemon(machine, args.max_jobs);
    output.print_failure(sink, &failure);
    output.failure_exit(failure.code())
}
