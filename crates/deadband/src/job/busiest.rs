//! Names the busiest process outside the jobs by running `/bin/ps`. An
//! unprivileged daemon cannot read a root process's CPU through libproc, and
//! `/bin/ps` is setuid root. With the other operations of the parent module,
//! it is one of the operations of the module that starts processes (PROC-1).
//!
//! The one operation, [`Census::read`], states the five items of PROC-2:
//!
//! - Program: the first word of the command the daemon passes, which is
//!   always `/bin/ps` outside the tests.
//! - Arguments: the remaining words, each passed as a separate value. No
//!   shell reads them.
//! - Environment: the program inherits the daemon's whole environment.
//! - Streams and deadline: the program reads nothing on standard input,
//!   deadband keeps the first [`MAX_OUTPUT`] bytes of its standard output and
//!   discards its standard error. The deadline is the one the caller passes.
//!   When it passes, `read` returns and the thread that holds the process
//!   keeps waiting for it.
//! - End: deadband waits for the program and reaps it. deadband never stops
//!   or signals it, even past its deadline, and no second one starts until
//!   the first has exited.

use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use crate::protocol::{Label, OutsideJobs};
use crate::spawn;
use crate::stuck::BUSY_PERCENT;

/// The most bytes of the program's standard output that deadband keeps.
pub const MAX_OUTPUT: u64 = 4 * 1024 * 1024;

/// The command that lists every process with its share of one core.
pub const PS_COMMAND: [&str; 4] = ["/bin/ps", "-Ao", "pcpu=,pid=,comm=", "-r"];

/// Why no process could be named.
#[derive(Debug)]
pub enum Failed {
    /// An earlier run has not exited yet, so no new one started.
    Running,
    /// The program could not start or be waited for.
    Start(io::Error),
    /// The program exited with a failure, or a signal ended it.
    Exit(Option<i32>),
    /// The deadline passed before the program exited.
    TimedOut,
    /// The program printed something that is not a process list.
    Unparsable(String),
}

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => f.write_str("an earlier /bin/ps has not exited"),
            Self::Start(error) => write!(f, "/bin/ps could not start: {error}"),
            Self::Exit(Some(code)) => write!(f, "/bin/ps exited with {code}"),
            Self::Exit(None) => f.write_str("a signal ended /bin/ps"),
            Self::TimedOut => f.write_str("/bin/ps did not exit in time"),
            Self::Unparsable(problem) => write!(f, "/bin/ps printed {problem}"),
        }
    }
}

/// Runs the process list, at most one at a time.
#[derive(Debug, Clone, Default)]
pub struct Census {
    /// Whether a run has not exited yet.
    running: Arc<AtomicBool>,
}

impl Census {
    /// Runs `command`, and returns the busiest process that is not in
    /// `exclude` or the command itself.
    ///
    /// # Errors
    ///
    /// Returns [`Failed`] when the process list cannot be read within
    /// `within`.
    pub fn read(
        &self,
        command: &[String],
        within: Duration,
        exclude: &BTreeSet<u32>,
    ) -> Result<OutsideJobs, Failed> {
        let Some((program, args)) = command.split_first() else {
            return Err(Failed::Start(io::ErrorKind::InvalidInput.into()));
        };
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(Failed::Running);
        }
        // The thread that waits starts before the program, so a refused
        // thread leaves no program unreaped, and the flag clears either way.
        let running = Arc::clone(&self.running);
        #[expect(
            clippy::disallowed_types,
            reason = "crates/deadband/src/job/busiest.rs hands the child to its own waiting thread, which reaps it"
        )]
        let (start, started) = mpsc::channel::<std::process::Child>();
        let (sender, ended) = mpsc::channel();
        let waiting = spawn::thread("deadband-ps", move || {
            let Ok(mut child) = started.recv() else {
                return;
            };
            let mut kept = Vec::new();
            if let Some(mut stdout) = child.stdout.take() {
                let _ = (&mut stdout).take(MAX_OUTPUT).read_to_end(&mut kept);
                let _ = io::copy(&mut stdout, &mut io::sink());
            }
            let status = child.wait();
            running.store(false, Ordering::SeqCst);
            let _ = sender.send(status.map(|status| (status, kept)));
        });
        if let Err(error) = waiting {
            self.running.store(false, Ordering::SeqCst);
            return Err(Failed::Start(error));
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "crates/deadband/src/job/busiest.rs starts /bin/ps; the module states PROC-2's items"
        )]
        let spawned = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let child = match spawned {
            Ok(child) => child,
            Err(error) => {
                self.running.store(false, Ordering::SeqCst);
                return Err(Failed::Start(error));
            }
        };
        let pid = child.id();
        // The waiting thread holds `started` until it receives, so the send
        // reaches it.
        let _ = start.send(child);
        let (status, stdout) = match ended.recv_timeout(within) {
            Ok(Ok(ended)) => ended,
            Ok(Err(error)) => return Err(Failed::Start(error)),
            Err(_) => return Err(Failed::TimedOut),
        };
        if !status.success() {
            return Err(Failed::Exit(status.code()));
        }
        let mut exclude = exclude.clone();
        exclude.insert(pid);
        parse(&stdout, &exclude).map_err(Failed::Unparsable)
    }
}

/// One line of the process list.
struct Line<'a> {
    /// The whole percent of one core.
    percent: u32,
    /// The process ID.
    pid: u32,
    /// The executable's path, or its name.
    command: &'a str,
}

/// Reads one line of the process list, or returns `None` for another form.
fn line(text: &str) -> Option<Line<'_>> {
    let (percent, rest) = text.trim_start().split_once(char::is_whitespace)?;
    let (pid, command) = rest.trim_start().split_once(char::is_whitespace)?;
    let whole = percent.split(['.', ',']).next()?;
    Some(Line {
        percent: whole.parse().ok()?,
        pid: pid.parse().ok()?,
        command: command.trim(),
    })
}

/// Reads the output of the process list: one line per process, with the
/// share of one core, the process ID and the executable's path. A process
/// at or over [`BUSY_PERCENT`] that is not in `exclude` is named by the last
/// part of its path, in the form of a [`Label`].
///
/// # Errors
///
/// Returns why the output is not a process list.
pub fn parse(stdout: &[u8], exclude: &BTreeSet<u32>) -> Result<OutsideJobs, String> {
    let text = String::from_utf8_lossy(stdout);
    let lines = text
        .lines()
        .filter(|each| !each.trim().is_empty())
        .map(|each| line(each).ok_or_else(|| format!("the line {each:?}, which is not a process")))
        .collect::<Result<Vec<_>, _>>()?;
    if lines.is_empty() {
        return Err("nothing".to_owned());
    }
    let busiest = lines
        .iter()
        .filter(|each| !exclude.contains(&each.pid) && each.percent >= BUSY_PERCENT)
        .max_by_key(|each| each.percent);
    Ok(busiest.map_or(OutsideJobs::Quiet, |each| {
        let path = each.command.rsplit('/').next().unwrap_or(each.command);
        OutsideJobs::Busy {
            name: Label::new(path).as_str().to_owned(),
            cpu_percent: each.percent,
        }
    }))
}
