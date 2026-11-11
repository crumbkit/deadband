//! Reads the config file's TOML text into its serde type.

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
