//! Runs the job of `deadband run` and passes on to it the signals that
//! `deadband run`'s caller sends, unchanged. This is the one place where
//! deadband sends a signal to a job, and it sends only a signal that it
//! received.
//!
//! The signals are registered before the job starts, so one that arrives
//! between the start and the first wait is queued and then forwarded. A
//! forwarding thread sends each one to the job's process id while the job is
//! live. The main thread waits for the job to end without reaping it, marks
//! the job as no longer live under the same lock that the thread holds while
//! it sends, and only then reaps it, so no signal goes to a process id that
//! the operating system may have reused.

use std::ffi::{OsString, c_int};
use std::io;
use std::process::{Command, ExitStatus};
use std::sync::{Arc, Mutex};

use rustix::io::Errno;
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process, waitid};
use signal_hook::iterator::Signals;

/// Runs `command` with `variables` added to its environment, passes each of
/// `forwarded` that this process receives on to it, and returns how it
/// ended.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] for an empty command, and the
/// error of a program that could not start. A signal that cannot be
/// registered is not an error: the job runs and the signals are not passed
/// on. A thread that the operating system refuses is not an error either,
/// but the registered signals then stay registered until the job ends, so
/// the wrapper ignores them and does not pass them on.
pub(super) fn run_forwarding(
    command: &[OsString],
    variables: &[(&str, String)],
    forwarded: &[c_int],
) -> io::Result<ExitStatus> {
    let Some((program, args)) = command.split_first() else {
        return Err(io::ErrorKind::InvalidInput.into());
    };
    let signals = Signals::new(forwarded).ok();
    let handle = signals.as_ref().map(Signals::handle);
    #[expect(
        clippy::disallowed_methods,
        reason = "crates/deadband/src/job/forward.rs starts the job, as job.rs does"
    )]
    let spawned = Command::new(program)
        .args(args)
        .envs(variables.iter().map(|(name, value)| (name, value)))
        .spawn();
    let mut child = spawned?;
    let pid = Pid::from_child(&child);
    let live = Arc::new(Mutex::new(true));
    // A thread that the operating system refuses leaves the job running
    // without forwarding; the wrapper must not panic and leave the job
    // without a wrapper.
    let forwarder = signals.and_then(|signals| {
        let live = Arc::clone(&live);
        std::thread::Builder::new()
            .name("deadband-forward".to_owned())
            .spawn(move || forward(signals, pid, &live))
            .ok()
    });
    wait_for_exit(pid);
    if let Ok(mut live) = live.lock() {
        *live = false;
    }
    if let Some(handle) = handle {
        handle.close();
    }
    let status = child.wait();
    if let Some(forwarder) = forwarder {
        let _ = forwarder.join();
    }
    status
}

/// Sends each signal that arrives to `pid` while `live` is true. It returns
/// when the signals are closed.
fn forward(mut signals: Signals, pid: Pid, live: &Mutex<bool>) {
    for number in signals.forever() {
        let Ok(live) = live.lock() else { return };
        if !*live {
            continue;
        }
        if let Some(signal) = Signal::from_named_raw(number) {
            #[expect(
                clippy::disallowed_methods,
                reason = "crates/deadband/src/job/forward.rs passes a received signal on to the job"
            )]
            let _ = kill_process(pid, signal);
        }
    }
}

/// Waits until the process `pid` has ended, without reaping it.
fn wait_for_exit(pid: Pid) {
    loop {
        match waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        ) {
            Err(Errno::INTR) => {}
            _ => return,
        }
    }
}
