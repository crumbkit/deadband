//! Starts the job that `deadband run` was given, in [`reclaimer`] the
//! reclaimers that the daemon's config file names, in [`busiest`] `/bin/ps`,
//! in [`on_stuck`] the `on_stuck` command, and in [`launchd`] launchctl and
//! getconf. It is the only module that starts a process (PROC-1).
//!
//! The one operation, [`run`], states the five items of PROC-2:
//!
//! - Program: the first word of the command, which the operating system
//!   finds on `PATH`. The person or tool that calls `deadband run` supplies
//!   it, and could run it without deadband, so deadband trusts it as much
//!   as its caller does.
//! - Arguments: the remaining words, each passed as a separate value. No
//!   shell reads them.
//! - Environment: the job inherits deadband's whole environment, with the
//!   variables that its caller passes added: `DEADBAND_HELD` and the
//!   parallelism variables of a granted job.
//! - Streams and deadline: the job inherits deadband's standard input,
//!   output and error, deadband captures nothing, and there is no deadline.
//! - End: deadband waits for the job and reaps it, and reads its exit
//!   status from the wait. deadband never stops or signals the job. A
//!   Ctrl-C at the terminal reaches both deadband and the job, because they
//!   share a process group, so deadband can end before the job; the daemon
//!   then sees the connection close and frees the job's place.

use std::ffi::OsString;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitStatus};

pub mod busiest;
pub mod launchd;
pub mod on_stuck;
pub mod reclaimer;
#[cfg(test)]
mod tests;

/// The exit code a shell gives a process that a signal ended is this base
/// plus the signal's number.
const SIGNAL_BASE: i32 = 128;

/// Runs `command`, whose first word is the program, with `variables` added
/// to its environment, waits for it, and returns its exit status.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] for an empty command, and the
/// error of a program that could not start, such as
/// [`io::ErrorKind::NotFound`].
pub fn run(command: &[OsString], variables: &[(&str, String)]) -> io::Result<ExitStatus> {
    let Some((program, args)) = command.split_first() else {
        return Err(io::ErrorKind::InvalidInput.into());
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "crates/deadband/src/job.rs is the module that starts processes, and the job is the command its caller names"
    )]
    let status = Command::new(program)
        .args(args)
        .envs(variables.iter().map(|(name, value)| (name, value)))
        .status();
    status
}

/// Returns the exit code that `deadband run` ends with for a job that
/// ended with `status`: the job's own code, or 128 plus the signal's number
/// when a signal ended it, as a shell reports it.
#[must_use]
pub fn exit_code(status: ExitStatus) -> u8 {
    let code = status
        .code()
        .or_else(|| {
            status
                .signal()
                .map(|signal| SIGNAL_BASE.saturating_add(signal))
        })
        .unwrap_or(SIGNAL_BASE);
    u8::try_from(code & 0xff).unwrap_or(u8::MAX)
}
