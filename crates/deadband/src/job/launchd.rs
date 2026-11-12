//! Asks launchd about deadband's launch agent and loads or unloads it, by
//! running `/bin/launchctl`, and reads the per-user temporary directory by
//! running `/usr/bin/getconf`. With the other operations of the parent
//! module, these are operations of the module that starts processes
//! (PROC-1).
//!
//! Each run states the five items of PROC-2:
//!
//! - Program: `/bin/launchctl` or `/usr/bin/getconf`, by absolute path.
//! - Arguments: fixed words, the launchd domain or service target, and the
//!   plist's path, each passed as a separate value. No shell reads them.
//! - Environment: the program inherits deadband's whole environment.
//! - Streams and deadline: the program reads nothing on standard input, and
//!   deadband keeps its standard output and standard error to report a
//!   failure. There is no deadline, because both programs only ask launchd
//!   or the C library and return, and a person waits at the terminal.
//! - End: deadband waits for the program and reaps it, and never signals it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

#[cfg(test)]
mod tests;

/// The exit code of `launchctl print` for a service that launchd has not
/// loaded.
const NOT_LOADED: i32 = 113;

/// A launchctl or getconf run that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failed {
    /// The command line that failed, such as `launchctl bootstrap gui/501`.
    pub command: String,
    /// What the program printed, or why it could not start.
    pub detail: String,
}

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed: {}", self.command, self.detail)
    }
}

/// What deadband asks of launchd, behind a trait so tests use a fake.
pub trait Launchd: fmt::Debug + Send + Sync {
    /// Returns the per-user temporary directory that
    /// `getconf DARWIN_USER_TEMP_DIR` prints.
    ///
    /// # Errors
    ///
    /// Returns [`Failed`] when getconf fails or prints nothing.
    fn temp_dir(&self) -> Result<PathBuf, Failed>;

    /// Reports whether launchd has loaded the service `target`, such as
    /// `gui/501/io.github.crumbkit.deadband`.
    ///
    /// # Errors
    ///
    /// Returns [`Failed`] when launchctl fails for another reason.
    fn is_loaded(&self, target: &str) -> Result<bool, Failed>;

    /// Loads the agent at `plist` into `domain`, such as `gui/501`.
    ///
    /// # Errors
    ///
    /// Returns [`Failed`] when launchctl fails.
    fn bootstrap(&self, domain: &str, plist: &Path) -> Result<(), Failed>;

    /// Unloads the service `target`, which stops its daemon.
    ///
    /// # Errors
    ///
    /// Returns [`Failed`] when launchctl fails.
    fn bootout(&self, target: &str) -> Result<(), Failed>;

    /// Waits [`PAUSE`] before the next [`Launchd::is_loaded`] poll, while a
    /// booted-out daemon exits.
    fn pause(&self);
}

/// How long [`Launchd::pause`] waits between two polls.
pub const PAUSE: Duration = Duration::from_millis(100);

/// The launchd of the machine deadband runs on.
#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Launchd for System {
    fn temp_dir(&self) -> Result<PathBuf, Failed> {
        let words = ["DARWIN_USER_TEMP_DIR"];
        let output = output("/usr/bin/getconf", &words)?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !output.status.success() || text.is_empty() {
            return Err(failed("/usr/bin/getconf", &words, &output));
        }
        Ok(PathBuf::from(text))
    }

    fn is_loaded(&self, target: &str) -> Result<bool, Failed> {
        let words = ["print", target];
        let output = output("/bin/launchctl", &words)?;
        loaded_from(output.status.code()).ok_or_else(|| failed("/bin/launchctl", &words, &output))
    }

    fn bootstrap(&self, domain: &str, plist: &Path) -> Result<(), Failed> {
        let plist = plist.to_string_lossy();
        launchctl(&["bootstrap", domain, &plist])
    }

    fn bootout(&self, target: &str) -> Result<(), Failed> {
        launchctl(&["bootout", target])
    }

    fn pause(&self) {
        #[expect(
            clippy::disallowed_methods,
            reason = "launchd sends no event when a booted-out service has exited, so install polls launchctl print; PAUSE names the wait"
        )]
        std::thread::sleep(PAUSE);
    }
}

/// Returns what an exit code of `launchctl print` says: `Some(true)` for a
/// loaded service, `Some(false)` for one that launchd has not loaded, and
/// `None` for a failure.
fn loaded_from(code: Option<i32>) -> Option<bool> {
    match code {
        Some(0) => Some(true),
        Some(NOT_LOADED) => Some(false),
        _ => None,
    }
}

/// Runs `/bin/launchctl` with `words` and returns `Ok` when it succeeds.
fn launchctl(words: &[&str]) -> Result<(), Failed> {
    let output = output("/bin/launchctl", words)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(failed("/bin/launchctl", words, &output))
    }
}

/// Runs `program` with `words` and returns what it printed.
fn output(program: &str, words: &[&str]) -> Result<Output, Failed> {
    #[expect(
        clippy::disallowed_methods,
        reason = "crates/deadband/src/job/launchd.rs runs launchctl and getconf; the module states PROC-2's items"
    )]
    let output = Command::new(program)
        .args(words)
        .stdin(Stdio::null())
        .output();
    output.map_err(|error| Failed {
        command: command_line(program, words),
        detail: format!("it could not start: {error}"),
    })
}

/// Returns the failure of a run that printed `output`.
fn failed(program: &str, words: &[&str], output: &Output) -> Failed {
    let printed = [&output.stderr, &output.stdout]
        .into_iter()
        .map(|bytes| String::from_utf8_lossy(bytes).trim().to_owned())
        .find(|text| !text.is_empty());
    let status = output.status.code().map_or_else(
        || "a signal ended it".to_owned(),
        |code| format!("it exited with {code}"),
    );
    Failed {
        command: command_line(program, words),
        detail: printed.map_or(status.clone(), |text| format!("{status}: {text}")),
    }
}

/// Returns `program` and `words` as one line, with the program's name
/// alone.
fn command_line(program: &str, words: &[&str]) -> String {
    let name = Path::new(program).file_name().map_or_else(
        || program.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    std::iter::once(name.as_str())
        .chain(words.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}
