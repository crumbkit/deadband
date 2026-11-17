//! Reads the config file's TOML text into its serde type, reads the ages
//! its keys hold, and writes an age as the daemon's log and status print it.

use std::fmt;
use std::time::Duration;

use serde::de::DeserializeOwned;

/// Reads `text` as TOML into a `T`.
///
/// # Errors
///
/// Returns, in one line, the toml crate's message and the line and column
/// where the error starts, counted from 1. toml's own display spreads over
/// several lines with a snippet of the file, and the daemon logs a refusal
/// in one line.
pub(super) fn from_toml<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    toml::from_str(text).map_err(|error: toml::de::Error| {
        let message = error
            .message()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let Some(span) = error.span() else {
            return message;
        };
        let before = text.get(..span.start).unwrap_or(text);
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().unwrap_or(before).chars().count() + 1;
        format!("{message} at line {line} column {column}")
    })
}

/// Reads an age, such as "60s", "5m", "1h 30m" or "2 hours".
///
/// # Errors
///
/// Returns, in one line, why `text` is not an age.
pub(crate) fn parse_age(text: &str) -> Result<Duration, String> {
    humantime::parse_duration(text).map_err(|error| error.to_string())
}

/// An age as the daemon's log and `deadband status` print it, in its largest
/// units first, such as "1m", "50m" or "1h 30m".
pub(crate) struct ShownAge(pub(crate) Duration);

impl fmt::Display for ShownAge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", humantime::format_duration(self.0))
    }
}
