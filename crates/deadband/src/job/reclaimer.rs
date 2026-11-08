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
//! Soft limit: `run` reads standard output to its end before it reaps the
//! reclaimer, so a reclaimer that leaves a background process holding its
//! standard output open counts as running until that process closes it.
//! A reader that stops at the reclaimer's exit would replace this.

use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

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
    thread::spawn(move || {
        let mut kept = Vec::new();
        if let Some(mut stdout) = stdout {
            let _ = (&mut stdout).take(MAX_OUTPUT).read_to_end(&mut kept);
            let _ = io::copy(&mut stdout, &mut io::sink());
        }
        let status = child.wait();
        let _ = sender.send(status.map(|status| Ended::Exited {
            code: status.code(),
            stdout: kept,
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
