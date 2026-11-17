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
//!   the timeout the caller passes. When it passes, [`run`] calls the
//!   caller's `on_timeout` and keeps waiting until the reclaimer exits or
//!   the caller's ceiling passes, so the daemon runs no second reclaimer
//!   before then. Past the ceiling it stops waiting and returns
//!   [`Ended::Abandoned`].
//! - End: deadband waits for the reclaimer and reaps it, also after it
//!   stopped waiting at the ceiling; an abandoned reclaimer's output is
//!   discarded and the caller's `on_late_exit` hears its exit. deadband
//!   never stops or signals a reclaimer, even past its ceiling: a reclaimer
//!   stopped halfway could leave what it was stopping half stopped.
//!
//! A reading thread reads standard output while the reclaimer runs. Once
//! the reclaimer has exited, everything it wrote is in the pipe, so the
//! reading thread reads what the pipe holds without blocking and hands
//! over the report, however late the operating system runs it. A
//! background process that holds standard output open therefore does not
//! hold the reclaim, and what it writes after the reclaimer exits is
//! dropped. When the operating system refuses the reading thread, no
//! reclaimer starts and `run` returns the operating system's error.
//!
//! Soft limit: a background process that keeps standard output open for
//! good leaves one reading thread and one pipe in the daemon until it
//! closes the pipe, because closing the pipe sooner would risk SIGPIPE for
//! that process. A limit on such threads would replace this if a reclaimer
//! ever does it on every run. Soft limit: when `poll` fails, the reading
//! thread reads standard output to its end, so in that case such a
//! background process holds the reclaim until it closes the pipe.

use std::io::{self, PipeReader, PipeWriter, Read};
use std::process::{ChildStdout, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
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
    /// The deadline passed before the reclaimer exited, and it exited
    /// before the ceiling.
    TimedOut,
    /// The ceiling passed before the reclaimer exited. It is left running.
    Abandoned {
        /// Its process ID.
        pid: u32,
    },
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

/// The reading thread, as the waiting thread holds it.
struct Reader {
    /// Where the waiting thread sends the reclaimer's standard output.
    stdout: Sender<ChildStdout>,
    /// The pipe whose closing tells the reading thread the reclaimer has
    /// exited.
    exited: PipeWriter,
    /// Where the reading thread sends what it kept.
    report: Receiver<Vec<u8>>,
}

impl Reader {
    /// Tells the reading thread the reclaimer has exited, and returns what
    /// it kept once it has read what the pipe holds.
    fn collect(self) -> Vec<u8> {
        let Self {
            stdout,
            exited,
            report,
        } = self;
        // A reading thread that never received standard output returns once
        // `stdout` is gone, so the receive below never waits on it.
        drop(stdout);
        drop(exited);
        report.recv().unwrap_or_default()
    }
}

/// Runs `command` with `args` added, and waits for it. When `timeout`
/// passes first, it calls `on_timeout` and waits until the reclaimer exits,
/// which returns [`Ended::TimedOut`], or until `ceiling`, counted from the
/// start, passes. Then it returns [`Ended::Abandoned`] and leaves the
/// reclaimer running; when it exits, its output is discarded and
/// `on_late_exit` hears its process ID and exit code, the code `None` when
/// a signal ended it or it could not be waited for.
///
/// # Errors
///
/// Returns the operating system's error when it refuses the reading or the
/// waiting thread, and the error of a program that could not start or be
/// waited for.
pub fn run(
    command: &CommandLine,
    args: &[&str],
    timeout: Duration,
    ceiling: Duration,
    on_timeout: impl FnOnce(),
    on_late_exit: impl FnOnce(u32, Option<i32>) + Send + 'static,
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
    // The waiting thread sends the process ID, then reaps the reclaimer
    // and drops `running`, which tells a caller past its timeout that the
    // reclaimer exited. Whichever of the reaping and the ceiling takes
    // `settled` first decides whether the reclaimer was abandoned.
    let (running, pid) = mpsc::channel::<u32>();
    let settled = Arc::new(AtomicBool::new(false));
    let reaped = Arc::clone(&settled);
    let mut abandoned = None;
    let waited = wait::start_and_wait(
        "deadband-reclaimer-wait",
        started,
        move |child| {
            if let Some(stdout) = child.stdout.take() {
                let _ = reader.stdout.send(stdout);
            }
            let id = child.id();
            let _ = running.send(id);
            // The waiting thread's own wait then returns this status.
            let code = child.wait().ok().and_then(|status| status.code());
            drop(running);
            if reaped.swap(true, Ordering::SeqCst) {
                on_late_exit(id, code);
            }
            reader
        },
        timeout,
        || {
            on_timeout();
            let id = pid.recv().ok();
            match pid.recv_timeout(ceiling.saturating_sub(timeout)) {
                Err(RecvTimeoutError::Timeout) if !settled.swap(true, Ordering::SeqCst) => {
                    abandoned = id;
                    AfterDeadline::StopWaiting
                }
                _ => AfterDeadline::KeepWaiting,
            }
        },
    )?;
    Ok(match (waited, abandoned) {
        (Waited::Exited(status, reader), _) => Ended::Exited {
            code: status.code(),
            stdout: reader.collect(),
        },
        (Waited::Late, Some(pid)) => Ended::Abandoned { pid },
        (Waited::Late, None) => Ended::TimedOut,
    })
}

#[cfg(test)]
thread_local! {
    /// How long each reading thread that a test's thread starts waits
    /// before its first read, as the operating system may run it late on a
    /// machine short of memory.
    static READING_DELAY: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
}

/// Makes each reading thread that the calling thread starts wait `delay`
/// before its first read.
#[cfg(test)]
pub(super) fn delay_reading(delay: Duration) {
    READING_DELAY.with(|cell| cell.set(delay));
}

/// Starts the reading thread.
///
/// # Errors
///
/// Returns the operating system's error when it cannot make a pipe or
/// refuses the thread.
fn start_reader() -> io::Result<Reader> {
    // std makes both ends close on exec. On macOS it sets that flag in a
    // separate step after pipe(), so a helper program that another thread
    // starts in that window can hold the write end, and the report then
    // waits until that helper exits. Every helper has a deadline, so the
    // wait is bounded.
    let (hears_exit, exited) = io::pipe()?;
    let (stdout, for_reader) = mpsc::channel::<ChildStdout>();
    let (to_waiter, report) = mpsc::channel();
    #[cfg(test)]
    let delay = READING_DELAY.with(std::cell::Cell::get);
    spawn::thread("deadband-reclaimer-read", move || {
        if let Ok(stdout) = for_reader.recv() {
            #[cfg(test)]
            #[expect(
                clippy::disallowed_methods,
                reason = "a test stands in for a reading thread that the operating system runs late"
            )]
            std::thread::sleep(delay);
            read_until_exit(stdout, &hears_exit, &to_waiter);
        }
    })?;
    Ok(Reader {
        stdout,
        exited,
        report,
    })
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
                // exits, so it reads to the end.
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
