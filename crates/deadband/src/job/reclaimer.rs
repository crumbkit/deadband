//! Runs a reclaimer that the daemon's config file names. With the other
//! operations of the parent module, it is one of the operations of the
//! module that starts processes (PROC-1).
//!
//! The one operation, [`run`], states the five items of PROC-2:
//!
//! - Program: the program of the reclaimer's `command` in the config file,
//!   which the operating system finds on the daemon's `PATH`. The person
//!   who runs the daemon writes the config file, so deadband trusts the
//!   program as much as that person.
//! - Arguments: the remaining words of `command`, then `--need` with its
//!   value, then `--dry-run` on a dry run, each passed as a separate value.
//!   No shell reads them.
//! - Environment: the reclaimer inherits the daemon's whole environment.
//! - Streams and deadline: the reclaimer reads nothing on standard input,
//!   deadband keeps the first [`MAX_OUTPUT`] bytes of its standard output
//!   and drops the rest, and discards its standard error. The deadline is
//!   the timeout the caller passes. When it passes, `run` calls the
//!   caller's `on_timeout` and then keeps waiting until the reclaimer
//!   exits, so the daemon never runs two reclaimers at once.
//! - End: deadband waits for the reclaimer and reaps it. deadband never
//!   stops or signals a reclaimer, even past its deadline: a reclaimer
//!   stopped halfway could leave what it was stopping half stopped.
//!
//! A reading thread reads standard output while the reclaimer runs. Once
//! the reclaimer has exited, everything it wrote is in the pipe, so the
//! reading thread reads what the pipe holds without blocking and hands
//! over the report, however late the operating system runs it. A
//! background process that holds standard output open therefore does not
//! hold the reclaim, and what it writes after the reclaimer exits is
//! dropped. Soft limit: a background process that keeps standard output
//! open for good leaves one reading thread and one pipe in the daemon until
//! it closes the pipe, because closing the pipe sooner would risk SIGPIPE
//! for that process. A limit on such threads would replace this if a
//! reclaimer ever does it on every run. Soft limit: when the operating
//! system refuses the reading thread, the waiting thread reads standard
//! output to its end before it waits, and the reading thread does the same
//! when `poll` fails, so in those cases such a background process holds the
//! reclaim until it closes the pipe.

use std::io::{self, PipeReader, PipeWriter, Read};
use std::process::{ChildStdout, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, poll};

use super::CommandLine;
use super::wait::{self, AfterDeadline, Waited};
use crate::spawn;

/// The most bytes of a reclaimer's standard output that deadband keeps.
pub const MAX_OUTPUT: u64 = 1024 * 1024;

/// How a reclaimer ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The reclaimer exited within its deadline.
    Exited {
        /// Its exit code, or `None` when a signal ended it.
        code: Option<i32>,
        /// The first [`MAX_OUTPUT`] bytes of its standard output.
        stdout: Vec<u8>,
    },
    /// The deadline passed before the reclaimer exited. `run` returns once
    /// it has exited.
    TimedOut,
}

/// The first [`MAX_OUTPUT`] bytes of a reclaimer's standard output.
#[derive(Default)]
struct Kept(Vec<u8>);

impl Kept {
    /// Keeps as much of `bytes` as fits under [`MAX_OUTPUT`].
    fn keep(&mut self, bytes: &[u8]) {
        let room = usize::try_from(MAX_OUTPUT)
            .unwrap_or(usize::MAX)
            .saturating_sub(self.0.len());
        if let Some(bytes) = bytes.get(..bytes.len().min(room)) {
            self.0.extend_from_slice(bytes);
        }
    }
}

/// Where the waiting thread leaves the reclaimer's output once it has
/// reaped the reclaimer.
enum Output {
    /// The waiting thread read it all, because the reading thread was
    /// refused.
    Read(Vec<u8>),
    /// The reading thread reads it. Dropping `exited` tells that thread the
    /// reclaimer has exited, and it then sends what it kept on `report`.
    Reading {
        /// The pipe whose closing says the reclaimer has exited.
        exited: PipeWriter,
        /// Where the reading thread sends what it kept.
        report: Receiver<Vec<u8>>,
    },
}

impl Output {
    /// Returns the kept output, waiting for the reading thread when it
    /// reads it.
    fn collect(self) -> Vec<u8> {
        match self {
            Self::Read(kept) => kept,
            Self::Reading { exited, report } => {
                drop(exited);
                report.recv().unwrap_or_default()
            }
        }
    }
}

/// Runs `command` with `args` added, and waits for it. When `timeout`
/// passes first, it calls `on_timeout`, waits until the reclaimer exits,
/// and returns [`Ended::TimedOut`].
///
/// # Errors
///
/// Returns the error of a program that could not start or be waited for.
pub fn run(
    command: &CommandLine,
    args: &[&str],
    timeout: Duration,
    on_timeout: impl FnOnce(),
) -> io::Result<Ended> {
    let mut started = command.command();
    started
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // The reading thread starts before the reclaimer, so a refused thread
    // leaves no reclaimer unreaped.
    let reader = start_reader()?;
    let waited = wait::start_and_wait(
        "deadband-reclaimer-wait",
        started,
        move |child| {
            let stdout = child.stdout.take();
            match (reader, stdout) {
                (Some((to_reader, exited, report)), Some(stdout)) => {
                    let _ = to_reader.send(stdout);
                    Output::Reading { exited, report }
                }
                (_, stdout) => {
                    let mut kept = Kept::default();
                    if let Some(stdout) = stdout {
                        read_all(stdout, &mut kept);
                    }
                    Output::Read(kept.0)
                }
            }
        },
        timeout,
        || {
            on_timeout();
            AfterDeadline::KeepWaiting
        },
    )?;
    Ok(match waited {
        Waited::Exited(status, output) => Ended::Exited {
            code: status.code(),
            stdout: output.collect(),
        },
        Waited::Late => Ended::TimedOut,
    })
}

/// The channel to a reading thread, the pipe whose closing says the
/// reclaimer has exited, and where the thread sends what it kept.
type Reader = (Sender<ChildStdout>, PipeWriter, Receiver<Vec<u8>>);

/// Starts the reading thread, or returns `None` when the operating system
/// refuses it.
///
/// # Errors
///
/// Returns the operating system's error when it cannot make a pipe.
fn start_reader() -> io::Result<Option<Reader>> {
    // std makes both ends close on exec, so the reclaimer holds neither.
    let (hears_exit, tells_exit) = io::pipe()?;
    let (to_reader, for_reader) = mpsc::channel::<ChildStdout>();
    let (report, reported) = mpsc::channel();
    let delay = seam::reading_delay();
    let started = spawn::thread("deadband-reclaimer-read", move || {
        if let Ok(stdout) = for_reader.recv() {
            seam::pause(delay);
            read_until_exit(stdout, &hears_exit, &report);
        }
    });
    Ok(started.ok().map(|_| (to_reader, tells_exit, reported)))
}

/// Reads `stdout` into what it keeps until `exited` closes, then reads
/// what the pipe holds without blocking, sends what it kept on `report`,
/// and reads and drops the rest until the pipe closes.
fn read_until_exit(mut stdout: ChildStdout, exited: &PipeReader, report: &Sender<Vec<u8>>) {
    let mut kept = Kept::default();
    let mut chunk = [0_u8; 8192];
    loop {
        let mut ready = [
            PollFd::new(&stdout, PollFlags::IN),
            PollFd::new(exited, PollFlags::IN),
        ];
        match poll(&mut ready, None) {
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => {
                // Without poll the thread cannot tell when the reclaimer
                // exits, so it reads to the end, as a refused thread does.
                read_all(stdout, &mut kept);
                let _ = report.send(kept.0);
                return;
            }
            Ok(_) => {}
        }
        let [output, exit] = ready.map(|each| !each.revents().is_empty());
        if exit {
            break;
        }
        if output {
            match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => {
                    let _ = report.send(kept.0);
                    return;
                }
                Ok(read) => kept.keep(chunk.get(..read).unwrap_or_default()),
            }
        }
    }
    // The reclaimer has exited, so everything it wrote is in the pipe.
    if rustix::io::ioctl_fionbio(&stdout, true).is_ok() {
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => kept.keep(chunk.get(..read).unwrap_or_default()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = rustix::io::ioctl_fionbio(&stdout, false);
    }
    let _ = report.send(kept.0);
    let _ = io::copy(&mut stdout, &mut io::sink());
}

/// Reads `stdout` to its end into `kept`.
fn read_all(mut stdout: impl Read, kept: &mut Kept) {
    let mut chunk = [0_u8; 8192];
    while let Ok(read) = stdout.read(&mut chunk) {
        if read == 0 {
            return;
        }
        kept.keep(chunk.get(..read).unwrap_or_default());
    }
}

/// A test seam that holds the reading thread back before its first read, as
/// the operating system may on a machine short of memory.
pub(super) mod seam {
    use std::time::Duration;

    #[cfg(test)]
    thread_local! {
        /// The delay that the test running on this thread set.
        static DELAY: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
    }

    /// Returns the delay that the calling test set, which is zero outside
    /// the tests.
    pub(super) fn reading_delay() -> Duration {
        #[cfg(test)]
        return DELAY.with(std::cell::Cell::get);
        #[cfg(not(test))]
        Duration::ZERO
    }

    /// Makes each reading thread that the calling thread starts wait
    /// `delay` before its first read.
    #[cfg(test)]
    pub(in crate::job) fn delay_reading(delay: Duration) {
        DELAY.with(|cell| cell.set(delay));
    }

    /// Waits `delay`, which only a test sets.
    pub(super) fn pause(delay: Duration) {
        if !delay.is_zero() {
            #[expect(
                clippy::disallowed_methods,
                reason = "the test seam stands in for a reading thread that the operating system runs late"
            )]
            std::thread::sleep(delay);
        }
    }
}
