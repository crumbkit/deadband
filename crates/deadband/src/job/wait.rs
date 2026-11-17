//! Starts a helper program and waits for it with a deadline. The
//! reclaimers, the `on_stuck` command and `/bin/ps` share this body; each
//! caller passes its command, what it does with the program's streams and
//! what it does when the deadline passes.
//!
//! The thread that waits starts before the program, so an operating system
//! that refuses the thread leaves no program unreaped. That thread runs the
//! caller's stream handling, then reaps the program. deadband never stops or
//! signals a helper program, even past its deadline.

use std::io;
use std::process::{Command, ExitStatus};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use crate::spawn;

/// What [`start_and_wait`] does once the deadline has passed and the
/// caller's action has run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AfterDeadline {
    /// Keep waiting until the program exits, so no second one starts.
    KeepWaiting,
    /// Return at once. The waiting thread keeps waiting and reaps the
    /// program when it exits.
    StopWaiting,
}

/// How a helper program ended.
#[derive(Debug)]
pub(super) enum Waited<T> {
    /// The program exited within its deadline, with what the caller's
    /// stream handling returned.
    Exited(ExitStatus, T),
    /// The deadline passed before the program exited.
    Late,
}

/// Starts `command` and waits up to `within` for it. A thread named
/// `thread` runs `streams` on the started program and then reaps it. When
/// `within` passes first, `at_deadline` runs once, and what it returns says
/// whether to keep waiting for the program to exit.
///
/// What `streams` returns is dropped once the program has been reaped when
/// this call has already returned, and before this call returns when the
/// waiting thread or the program cannot start, so a guard that it holds is
/// released on every path.
///
/// # Errors
///
/// Returns the operating system's error when it refuses the waiting
/// thread, when the program cannot start or cannot be waited for, and an
/// error when the waiting thread stops without saying how the program
/// ended.
#[expect(
    clippy::disallowed_types,
    reason = "crates/deadband/src/job/wait.rs hands the child to its own waiting thread, which lends it to the caller's stream handling and then reaps it"
)]
pub(super) fn start_and_wait<T: Send + 'static>(
    thread: &str,
    mut command: Command,
    streams: impl FnOnce(&mut std::process::Child) -> T + Send + 'static,
    within: Duration,
    at_deadline: impl FnOnce() -> AfterDeadline,
) -> io::Result<Waited<T>> {
    let (start, started) = mpsc::channel::<std::process::Child>();
    let (sender, ended) = mpsc::channel();
    let waiting = spawn::thread(thread, move || {
        let Ok(mut child) = started.recv() else {
            return;
        };
        let output = streams(&mut child);
        let status = child.wait();
        let _ = sender.send(status.map(|status| (status, output)));
    })?;
    #[expect(
        clippy::disallowed_methods,
        reason = "crates/deadband/src/job/wait.rs starts the helper program of the job module's caller; each caller's module states PROC-2's items"
    )]
    let spawned = command.spawn();
    let child = match spawned {
        Ok(child) => child,
        Err(error) => {
            // The waiting thread returns once `start` is gone, and dropping
            // `streams` there releases what it holds before this returns.
            drop(start);
            let _ = waiting.join();
            return Err(error);
        }
    };
    let _ = start.send(child);
    match ended.recv_timeout(within) {
        Ok(ended) => ended.map(|(status, output)| Waited::Exited(status, output)),
        Err(RecvTimeoutError::Timeout) => match at_deadline() {
            AfterDeadline::StopWaiting => Ok(Waited::Late),
            AfterDeadline::KeepWaiting => ended
                .recv()
                .map_err(io::Error::other)?
                .map(|_| Waited::Late),
        },
        Err(RecvTimeoutError::Disconnected) => Err(io::Error::other(format!(
            "the thread {thread} stopped before it said how the program ended"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::{AfterDeadline, Waited, start_and_wait};
    use crate::job::CommandLine;

    fn sh(script: &str) -> std::process::Command {
        CommandLine::of(&["/bin/sh", "-c", script]).command()
    }

    #[test]
    fn a_program_that_exits_in_time_returns_its_status_and_what_its_streams_gave() {
        let waited = start_and_wait(
            "deadband-test-wait",
            sh("exit 6"),
            |child| child.id(),
            Duration::from_secs(30),
            || panic!("the program exits within its deadline"),
        );
        let Ok(Waited::Exited(status, pid)) = waited else {
            panic!("{waited:?}");
        };
        assert_eq!(status.code(), Some(6));
        assert_ne!(pid, 0);
    }

    #[test]
    fn a_deadline_that_passes_runs_its_action_once_and_keeps_waiting_until_the_program_exits() {
        let calls = Cell::new(0);
        let handled = Arc::new(AtomicBool::new(false));
        let streams_done = Arc::clone(&handled);
        let (fire, fired) = std::sync::mpsc::channel::<()>();
        let waited = start_and_wait(
            "deadband-test-wait",
            sh("exit 0"),
            move |_| {
                // The program is reaped only after the deadline action has
                // run, so the wait outlasts the deadline.
                let _ = fired.recv();
                streams_done.store(true, Ordering::SeqCst);
            },
            Duration::from_millis(10),
            || {
                calls.set(calls.get() + 1);
                let _ = fire.send(());
                AfterDeadline::KeepWaiting
            },
        );
        assert!(matches!(waited, Ok(Waited::Late)), "{waited:?}");
        assert_eq!(calls.get(), 1);
        assert!(handled.load(Ordering::SeqCst));
    }

    #[test]
    fn a_deadline_that_stops_waiting_returns_while_the_stream_handling_still_holds_the_program() {
        let (release, released) = std::sync::mpsc::channel::<()>();
        let waited = start_and_wait(
            "deadband-test-wait",
            sh("exit 0"),
            move |_| {
                let _ = released.recv();
            },
            Duration::from_millis(10),
            || AfterDeadline::StopWaiting,
        );
        assert!(matches!(waited, Ok(Waited::Late)), "{waited:?}");
        drop(release);
    }

    #[test]
    fn a_waiting_thread_that_cannot_start_starts_no_program_and_reports_the_error() {
        crate::spawn::refuse("deadband-test-wait");
        let waited = start_and_wait(
            "deadband-test-wait",
            sh("exit 0"),
            |_| (),
            Duration::from_secs(30),
            || AfterDeadline::KeepWaiting,
        );
        crate::spawn::allow();
        assert!(waited.is_err(), "{waited:?}");
    }

    #[test]
    fn a_program_that_cannot_start_has_released_what_its_streams_held_when_the_error_returns() {
        let held = Arc::new(());
        let guard = Arc::clone(&held);
        let waited = start_and_wait(
            "deadband-test-wait",
            CommandLine::of(&["/nonexistent/helper"]).command(),
            move |_| drop(guard),
            Duration::from_secs(30),
            || AfterDeadline::KeepWaiting,
        );
        assert!(waited.is_err(), "{waited:?}");
        assert_eq!(Arc::strong_count(&held), 1);
    }
}
