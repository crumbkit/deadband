//! Runs a reclaimer that the daemon's config file names. With `run` in
//! the parent module, it is one of the two operations of the module that
//! starts processes (PROC-1).
//!
//! The one operation, [`run`], states the five items of PROC-2:
//!
//! - Program: the first word of the reclaimer's `command` in the config
//!   file, which the operating system finds on the daemon's `PATH`. The
//!   person who runs the daemon writes the config file, so deadband trusts
//!   the program as much as that person.
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
//! `run` reads standard output while the reclaimer runs and stops waiting
//! for it `DRAIN_GRACE` after the reclaimer exits, so a background process
//! that holds standard output open does not hold the reclaim. Output that
//! such a process writes after that is dropped. Soft limit: a background
//! process that keeps standard output open for good leaves one reading
//! thread and one pipe in the daemon until it closes the pipe, because
//! closing the pipe sooner would risk SIGPIPE for that process. A limit on
//! such threads would replace this if a reclaimer ever does it on every run.

use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

/// The most bytes of a reclaimer's standard output that deadband keeps.
pub const MAX_OUTPUT: u64 = 1024 * 1024;

/// How long `run` waits for the output to be read after the reclaimer
/// exits.
const DRAIN_GRACE: Duration = Duration::from_millis(250);

/// Reads `stdout` to its end, keeping the first [`MAX_OUTPUT`] bytes in
/// `kept` as they arrive and dropping the rest.
fn drain_stdout(mut stdout: impl Read, kept: &Mutex<Vec<u8>>) {
    let mut chunk = [0_u8; 8192];
    while let Ok(read) = stdout.read(&mut chunk) {
        if read == 0 {
            return;
        }
        let mut kept = kept.lock().unwrap_or_else(PoisonError::into_inner);
        let room = usize::try_from(MAX_OUTPUT)
            .unwrap_or(usize::MAX)
            .saturating_sub(kept.len());
        if let Some(bytes) = chunk.get(..read.min(room)) {
            kept.extend_from_slice(bytes);
        }
    }
}

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

/// Runs `command`, whose first word is the program, with `args` added,
/// and waits for it. When `timeout` passes first, it calls `on_timeout`,
/// waits until the reclaimer exits, and returns [`Ended::TimedOut`].
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] for an empty command, and the
/// error of a program that could not start or be waited for.
pub fn run(
    command: &[String],
    args: &[&str],
    timeout: Duration,
    on_timeout: impl FnOnce(),
) -> io::Result<Ended> {
    let Some((program, first)) = command.split_first() else {
        return Err(io::ErrorKind::InvalidInput.into());
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "crates/deadband/src/job/reclaimer.rs starts the reclaimer that the daemon's config file names; the module states PROC-2's items"
    )]
    let spawned = Command::new(program)
        .args(first)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = spawned?;
    let stdout = child.stdout.take();
    let (sender, ended) = mpsc::channel();
    let kept = Arc::new(Mutex::new(Vec::new()));
    let (drained, drain) = mpsc::channel();
    if let Some(stdout) = stdout {
        let kept = Arc::clone(&kept);
        thread::spawn(move || {
            drain_stdout(stdout, &kept);
            let _ = drained.send(());
        });
    }
    thread::spawn(move || {
        let status = child.wait();
        // The reclaimer has exited. What it wrote is in the pipe already,
        // so a short wait collects it; a background process that still
        // holds the pipe open is not waited for, and the reading thread
        // ends when that process closes it.
        let _ = drain.recv_timeout(DRAIN_GRACE);
        let stdout = kept.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let _ = sender.send(status.map(|status| Ended::Exited {
            code: status.code(),
            stdout,
        }));
    });
    match ended.recv_timeout(timeout) {
        Ok(ended) => ended,
        Err(RecvTimeoutError::Timeout) => {
            on_timeout();
            ended
                .recv()
                .map_err(io::Error::other)?
                .map(|_| Ended::TimedOut)
        }
        Err(RecvTimeoutError::Disconnected) => Err(io::Error::other(
            "the thread that waits for the reclaimer stopped",
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Ended, run};

    #[test]
    fn a_background_child_that_holds_stdout_does_not_hold_the_reclaim_after_the_reclaimer_exits() {
        let command = ["sh", "-c", "sleep 4 & echo kept"].map(str::to_owned);
        let started = Instant::now();
        let ended = run(&command, &[], Duration::from_secs(30), || {})
            .expect("the reclaimer starts and is waited for");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the reclaim waited {:?} for the background child",
            started.elapsed()
        );
        assert_eq!(
            ended,
            Ended::Exited {
                code: Some(0),
                stdout: b"kept\n".to_vec()
            }
        );
    }
}
