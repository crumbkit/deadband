//! Checks that the program a command names can start.
//!
//! The daemon runs the check once when it reads its config file, so a
//! reclaimer or `on_stuck` command that names a program that cannot start
//! shows in the log at start. The check never refuses a config; a program
//! installed later works without a restart.

use std::fmt;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

/// Why a program cannot start.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    /// Nothing exists at the path.
    Missing,
    /// The path cannot be examined, with the operating system's error.
    Unreadable(String),
    /// The path names something other than a regular file.
    NotAFile,
    /// The file has no execute permission bit.
    NotExecutable,
    /// The path is not absolute, so the daemon's `PATH` decides what runs.
    Relative,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("which does not exist"),
            Self::Unreadable(error) => write!(f, "which cannot be read: {error}"),
            Self::NotAFile => f.write_str("which is not a file"),
            Self::NotExecutable => f.write_str("which is not executable"),
            Self::Relative => f.write_str(
                "which is not an absolute path and may not be on the short PATH that launchd gives a daemon",
            ),
        }
    }
}

impl Reason {
    /// Reports whether the program cannot start until something changes.
    /// A relative program can start when the daemon's `PATH` holds it.
    pub(crate) fn always_fails(&self) -> bool {
        !matches!(self, Self::Relative)
    }
}

/// A program that cannot start, with the path the command names.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Unusable {
    /// The first word of the command.
    pub(crate) path: PathBuf,
    /// Why the program cannot start.
    pub(crate) reason: Reason,
}

/// Checks the first word of `command`. An empty command names no program
/// and passes.
///
/// # Errors
///
/// Returns the program's path and why it cannot start: a relative path, a
/// path with nothing at it, something that is not a regular file, or a
/// file with no execute bit.
pub(crate) fn check(command: &[String]) -> Result<(), Unusable> {
    let Some(program) = command.first() else {
        return Ok(());
    };
    let path = Path::new(program);
    let fail = |reason| {
        Err(Unusable {
            path: path.to_path_buf(),
            reason,
        })
    };
    if path.is_relative() {
        return fail(Reason::Relative);
    }
    match path.metadata() {
        Err(error) if error.kind() == io::ErrorKind::NotFound => fail(Reason::Missing),
        Err(error) => fail(Reason::Unreadable(error.to_string())),
        Ok(found) if !found.is_file() => fail(Reason::NotAFile),
        Ok(found) if found.permissions().mode() & 0o111 == 0 => fail(Reason::NotExecutable),
        Ok(_) => Ok(()),
    }
}
