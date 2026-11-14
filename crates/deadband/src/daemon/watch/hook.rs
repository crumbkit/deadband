//! Runs the `on_stuck` command of the daemon's config file when a job becomes
//! stuck.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde::Serialize;

use super::super::{Log, note};
use crate::job::CommandLine;
use crate::job::on_stuck::{self, Ended};
use crate::protocol::OutsideJobs;
use crate::spawn;

/// The most `on_stuck` commands that run at once. A job that becomes stuck
/// while this many run gets none, and the log says so in one line.
const MAX_ON_STUCK: usize = 8;

/// What `on_stuck` reads on its standard input, as one JSON object.
#[derive(Serialize)]
struct Input<'a> {
    /// The job's label.
    label: &'a str,
    /// How long the job has shown no activity, in seconds.
    quiet_seconds: u64,
    /// The processes outside the jobs, or `null` when they could not be read.
    busiest: Option<&'a OutsideJobs>,
}

/// One of the [`MAX_ON_STUCK`] places, which it frees when it is dropped,
/// so a run that ends, a thread that is refused and a thread that panics
/// each free their place.
struct Place(Arc<AtomicUsize>);

impl Place {
    /// Takes a place, or returns `None` when all are taken.
    fn take(running: &Arc<AtomicUsize>) -> Option<Self> {
        let taken = running.fetch_add(1, Ordering::SeqCst);
        let place = Self(Arc::clone(running));
        (taken < MAX_ON_STUCK).then_some(place)
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Starts `on_stuck`, at most [`MAX_ON_STUCK`] at once.
pub(super) struct Hooks {
    /// The command, or `None` when the config names none.
    command: Option<CommandLine>,
    /// How long one run may take before the log says it is late.
    within: Duration,
    /// The daemon's log.
    log: Log,
    /// How many commands run.
    running: Arc<AtomicUsize>,
}

impl Hooks {
    /// Returns the hooks that run `command`.
    pub(super) fn new(command: Option<CommandLine>, within: Duration, log: &Log) -> Self {
        Self {
            command,
            within,
            log: Arc::clone(log),
            running: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Runs the command on a thread of its own, so a slow command holds up
    /// no sample. Its failure is one log line, and deadband never signals it.
    pub(super) fn run(&self, label: &str, quiet: Duration, busiest: Option<&OutsideJobs>) {
        let Some(command) = &self.command else {
            return;
        };
        let Some(place) = Place::take(&self.running) else {
            note(
                &self.log,
                format_args!(
                    "{MAX_ON_STUCK} on_stuck commands are running, so the daemon starts none for {label}"
                ),
            );
            return;
        };
        let input = serde_json::to_vec(&Input {
            label,
            quiet_seconds: quiet.as_secs(),
            busiest,
        })
        .unwrap_or_default();
        let command = command.clone();
        let within = self.within;
        let log = Arc::clone(&self.log);
        let started = spawn::thread("deadband-on-stuck", move || {
            let _place = place;
            let late = Arc::clone(&log);
            let ended = on_stuck::run(&command, input, within, move || {
                note(
                    &late,
                    format_args!(
                        "the on_stuck command has run past its deadline; the daemon leaves it running"
                    ),
                );
            });
            match ended {
                Ok(Ended::Exited(Some(0)) | Ended::TimedOut) => {}
                Ok(Ended::Exited(code)) => note(
                    &log,
                    format_args!("the on_stuck command failed with exit code {code:?}"),
                ),
                Err(error) => note(
                    &log,
                    format_args!(
                        "the on_stuck command {} could not start: {error}",
                        command.program
                    ),
                ),
            }
        });
        if let Err(error) = started {
            note(
                &self.log,
                format_args!("the on_stuck command for {label} could not start: {error}"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::Hooks;
    use crate::daemon::Log;
    use crate::job::CommandLine;

    #[test]
    fn an_on_stuck_whose_thread_cannot_start_is_one_log_line_and_frees_its_place() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let log: Log = written.clone();
        let hooks = Hooks::new(
            Some(CommandLine::of(&["true"])),
            Duration::from_secs(5),
            &log,
        );
        crate::spawn::refuse("deadband-on-stuck");
        hooks.run("cargo", Duration::from_secs(300), None);
        crate::spawn::allow();
        let text = String::from_utf8(written.lock().expect("the log").clone()).expect("UTF-8");
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.contains("could not start"), "{text}");
        assert_eq!(hooks.running.load(Ordering::SeqCst), 0);
    }

    /// A log that panics on every write, as a log whose disk has failed in
    /// some way might.
    struct Panics;

    impl std::io::Write for Panics {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            panic!("the test log refuses every line");
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Runs `program` as the `on_stuck` command once with `log`, and reports
    /// whether its place is free again within 10 seconds.
    fn frees_its_place(program: &str, log: &Log) -> bool {
        let hooks = Hooks::new(
            Some(CommandLine::of(&[program])),
            Duration::from_secs(5),
            log,
        );
        hooks.run("cargo", Duration::from_secs(300), None);
        let (done, finished) = std::sync::mpsc::channel();
        let running = Arc::clone(&hooks.running);
        std::thread::spawn(move || {
            while running.load(Ordering::SeqCst) != 0 {
                std::thread::yield_now();
            }
            let _ = done.send(());
        });
        finished.recv_timeout(Duration::from_secs(10)).is_ok()
    }

    #[test]
    fn an_on_stuck_thread_that_panics_frees_its_place() {
        let log: Log = Arc::new(Mutex::new(Panics));
        assert!(
            frees_its_place("false", &log),
            "the panicked on_stuck thread still holds its place"
        );
    }

    #[test]
    fn an_on_stuck_thread_that_ends_normally_frees_its_place() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        assert!(frees_its_place("true", &log));
    }
}
