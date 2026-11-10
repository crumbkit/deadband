//! Runs the `on_stuck` command of the daemon's config file. With the other
//! operations of the parent module, it is one of the operations of the module
//! that starts processes (PROC-1).
//!
//! The one operation, [`run`], states the five items of PROC-2:
//!
//! - Program: the first word of `on_stuck` in the config file, which the
//!   operating system finds on the daemon's `PATH`. The person who runs the
//!   daemon writes the config file, so deadband trusts the program as much
//!   as that person.
//! - Arguments: the remaining words, each passed as a separate value. No
//!   shell reads them.
//! - Environment: the program inherits the daemon's whole environment.
//! - Streams and deadline: the program reads the caller's bytes on standard
//!   input, and deadband discards its standard output and error. The
//!   deadline is the timeout the caller passes. When it passes, `run` calls
//!   the caller's `on_timeout` and then keeps waiting until the program
//!   exits.
//! - End: deadband waits for the program and reaps it. deadband never stops
//!   or signals it, even past its deadline.

use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

/// How the command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The command exited within its deadline, with this code, or `None`
    /// when a signal ended it.
    Exited(Option<i32>),
    /// The deadline passed before the command exited. `run` returns once it
    /// has exited.
    TimedOut,
}

/// Runs `command`, whose first word is the program, with `input` on its
/// standard input, and waits for it. When `timeout` passes first, it calls
/// `on_timeout`, waits until the command exits, and returns
/// [`Ended::TimedOut`].
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] for an empty command, and the
/// error of a program that could not start or be waited for.
pub fn run(
    command: &[String],
    input: Vec<u8>,
    timeout: Duration,
    on_timeout: impl FnOnce(),
) -> io::Result<Ended> {
    let Some((program, args)) = command.split_first() else {
        return Err(io::ErrorKind::InvalidInput.into());
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "crates/deadband/src/job/on_stuck.rs starts the on_stuck command that the daemon's config file names; the module states PROC-2's items"
    )]
    let spawned = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = spawned?;
    let stdin = child.stdin.take();
    let (sender, ended) = mpsc::channel();
    thread::spawn(move || {
        if let Some(mut stdin) = stdin {
            // A command that closes its input early loses the rest of it.
            let _ = stdin.write_all(&input);
        }
        let status = child.wait();
        let _ = sender.send(status.map(|status| Ended::Exited(status.code())));
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
            "the thread that waits for the on_stuck command stopped",
        )),
    }
}
