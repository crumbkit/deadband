//! The commands `deadband run` and `deadband daemon`, which print no result
//! on standard output.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::Path;

use crumbkit::{Code, Failure, Output, Recovery, Sink};

use super::{DaemonArgs, Machine, RunArgs};
use crate::client::{self, ANSWER_WITHIN, Admission};
use crate::daemon::{self, Settings, Unbound, note};
use crate::job;
use crate::probe::Probe;
use crate::protocol::Job;

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

/// Runs `deadband run`: asks the daemon to admit the job, runs it, tells
/// the daemon it ended, and returns its exit code.
///
/// When the daemon cannot be asked, the job starts at once and the log
/// says why in one line.
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
    let asked = Job {
        label,
        pid: std::process::id(),
        command: words.join(" "),
    };
    let queued = |ahead: usize| {
        let line = match ahead {
            0 => "the job waits for headroom, and no other job waits ahead of it".to_owned(),
            1 => "the job waits for headroom, and 1 job waits ahead of it".to_owned(),
            n => format!("the job waits for headroom, and {n} jobs wait ahead of it"),
        };
        note(&machine.log, format_args!("{line}"));
    };
    let lease = match client::ask(&machine.socket, &asked, ANSWER_WITHIN, queued) {
        Admission::Granted(lease) => Some(lease),
        Admission::Open(unasked) => {
            note(
                &machine.log,
                format_args!("{unasked}, so the job starts now"),
            );
            None
        }
    };
    let ended = job::run(&args.command);
    let exit_code = ended.as_ref().ok().and_then(|status| status.code());
    if let Some(lease) = lease {
        lease.end(exit_code);
    }
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
    let max_jobs = args
        .max_jobs
        .unwrap_or_else(|| daemon::default_max_jobs(machine.cores));
    daemon::raise_descriptor_limit();
    let failure = match daemon::bind(&machine.socket) {
        Ok(bound) => {
            note(
                &machine.log,
                format_args!(
                    "the daemon listens on {} and runs at most {max_jobs} jobs at once",
                    machine.socket.display()
                ),
            );
            let error = daemon::serve(
                &bound.listener,
                machine.probe.clone(),
                Settings::new(max_jobs),
                &machine.log,
            );
            Unbound::Socket {
                path: machine.socket.clone(),
                error,
            }
        }
        Err(unbound) => unbound,
    };
    output.print_failure(sink, &failure);
    output.failure_exit(failure.code())
}
