//! Chooses a job's lane from the words of the priority file.
//!
//! The priority file holds one word or path per line. A `#` starts a
//! comment that runs to the end of the line. A job runs in the fast lane
//! when one entry matches it:
//!
//! - A path that starts with `/`, or with `~` for the home directory,
//!   matches a job whose working directory is that directory or under it.
//! - Any other word matches a job whose label is that word, or whose
//!   working directory has that word as one of its parts.
//!
//! The word `exclusive`, which a shell gate's priority file may hold,
//! matches nothing.

use std::path::{Component, Path, PathBuf};

use crate::admit::Lane;

#[cfg(test)]
mod tests;

/// The word that matches nothing.
const EXCLUSIVE: &str = "exclusive";

/// Returns the lane of a job labelled `label` in the working directory
/// `cwd`, given the priority file's text `words` and the home directory.
#[must_use]
pub fn lane(words: &str, label: &str, cwd: Option<&Path>, home: Option<&Path>) -> Lane {
    let entries = words
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|entry| !entry.is_empty() && *entry != EXCLUSIVE);
    for entry in entries {
        if matches(entry, label, cwd, home) {
            return Lane::Fast;
        }
    }
    Lane::Normal
}

/// Reports whether one entry of the priority file matches the job.
fn matches(entry: &str, label: &str, cwd: Option<&Path>, home: Option<&Path>) -> bool {
    let directory = match entry.strip_prefix('~') {
        Some(rest) => home.map(|home| home.join(rest.trim_start_matches('/'))),
        None if entry.starts_with('/') => Some(PathBuf::from(entry)),
        None => {
            let part = cwd.is_some_and(|cwd| {
                cwd.components()
                    .any(|each| each == Component::Normal(entry.as_ref()))
            });
            return label == entry || part;
        }
    };
    match (directory, cwd) {
        (Some(directory), Some(cwd)) => cwd.starts_with(directory),
        _ => false,
    }
}
