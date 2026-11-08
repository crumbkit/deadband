//! The commands `deadband run` and `deadband daemon`, which print no result
//! on standard output.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::Path;

use crumbkit::{Code, Failure, Output, Recovery, Sink};

use super::{DaemonArgs, Machine, RunArgs};
use crate::admit::{Hold, Lane, Limits};
use crate::client::{self, ANSWER_WITHIN, Admission};
use crate::daemon::{self, Settings, Unbound, note};
use crate::job;
use crate::priority;
use crate::probe::Probe;
use crate::protocol::Job;
use crate::throttle::{self, NORMAL_NICE};

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

/// Returns the line that says why a job waits behind `ahead` jobs.
fn waits(ahead: usize, held: Option<Hold>) -> String {
    let why = held.map_or_else(
        || "for headroom".to_owned(),
        |hold| format!("because {hold}"),
    );
    match ahead {
        0 => format!("the job waits {why}, and no other job waits ahead of it"),
        1 => format!("the job waits {why}, and 1 job waits ahead of it"),
        n => format!("the job waits {why}, and {n} jobs wait ahead of it"),
    }
}

/// Returns the lane of the job labelled `label`: the one `--lane` named,
/// or the fast lane when the priority file matches the job. A priority
/// file that cannot be read matches nothing, and the log says why.
fn lane_of<P>(machine: &Machine<P>, asked: Option<Lane>, label: &str) -> Lane {
    if let Some(lane) = asked {
        return lane;
    }
    let environment = &machine.environment;
    let Some(file) = environment.priority_file() else {
        return Lane::Normal;
    };
    let words = match std::fs::read_to_string(&file) {
        Ok(words) => words,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Lane::Normal,
        Err(error) => {
            note(
                &machine.log,
                format_args!(
                    "deadband could not read {}: {error}, so the job waits in the normal lane",
                    file.display()
                ),
            );
            return Lane::Normal;
        }
    };
    priority::lane(
        &words,
        label,
        environment.cwd.as_deref(),
        environment.home.as_deref(),
    )
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
    if machine.environment.held || machine.environment.bypass {
        return finish(sink, output, program, job::run(&args.command, &[]));
    }
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
    let lane = lane_of(machine, args.lane, &label);
    let asked = Job {
        label,
        pid: std::process::id(),
        command: words.join(" "),
        lane,
    };
    let queued = |ahead: usize, held: Option<Hold>| {
        note(&machine.log, format_args!("{}", waits(ahead, held)));
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
    // A job that starts without a grant still gets the default share of
    // the cores, so it never runs with every core.
    let fallback = Limits {
        max_jobs: daemon::default_max_jobs(machine.cores),
        cores: machine.cores,
        reserve_lanes: false,
    };
    let variables = match &lease {
        Some(lease) => machine.environment.job_variables(lease.jobs(), true),
        None => machine
            .environment
            .job_variables(fallback.parallelism(lane), false),
    };
    if lane == Lane::Normal
        && let Err(error) = throttle::lower_own_priority(NORMAL_NICE)
    {
        note(
            &machine.log,
            format_args!(
                "deadband could not lower the job's priority: {error}, so the job runs at its caller's priority"
            ),
        );
    }
    let ended = job::run(&args.command, &variables);
    let exit_code = ended.as_ref().ok().and_then(|status| status.code());
    if let Some(lease) = lease {
        lease.end(exit_code);
    }
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
            let settings = Settings::new(
                max_jobs,
                machine.cores,
                machine.environment.home.clone(),
                machine.environment.priority_file(),
            );
            let error = daemon::serve(
                &bound.listener,
                machine.probe.clone(),
                settings,
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
