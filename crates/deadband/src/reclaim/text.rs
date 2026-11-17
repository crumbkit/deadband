//! Reads the config file's TOML text into its serde type, reads the ages
//! its keys hold, and writes an age as the daemon's log and status print it.

use std::fmt;
use std::time::Duration;

use jiff::SpanRelativeTo;
use jiff::fmt::friendly::{SpanParser, SpanPrinter};
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

/// The length of a day in an age, which is always 24 hours.
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// Reads an age, such as "60s", "5m", "1h 30m", "2 hours" or "1d", with
/// days and weeks of 24 hours each. Space around the age is ignored, and a
/// bare "0" is no time.
///
/// # Errors
///
/// Returns, in one line, why `text` is not an age: an unknown unit, a
/// number without a unit, a negative age, or months or years.
pub(crate) fn parse_age(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    if text == "0" {
        return Ok(Duration::ZERO);
    }
    let span = SpanParser::new()
        .parse_span(text)
        .map_err(|error| error.to_string())?;
    let age = span
        .to_duration(SpanRelativeTo::days_are_24_hours())
        .map_err(|error| error.to_string())?;
    Duration::try_from(age).map_err(|_| format!("{text:?} is a negative age"))
}

/// An age as the daemon's log and `deadband status` print it, in its largest
/// units first, such as "1m", "50m", "1h 30m" or "1day 2h".
pub(crate) struct ShownAge(pub(crate) Duration);

impl fmt::Display for ShownAge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let days = self.0.as_secs() / DAY.as_secs();
        let rest = Duration::new(self.0.as_secs() % DAY.as_secs(), self.0.subsec_nanos());
        let printer = SpanPrinter::new();
        match days {
            0 => f.write_str(&printer.unsigned_duration_to_string(&rest)),
            _ => {
                let unit = if days == 1 { "day" } else { "days" };
                write!(f, "{days}{unit}")?;
                if rest.is_zero() {
                    return Ok(());
                }
                write!(f, " {}", printer.unsigned_duration_to_string(&rest))
            }
        }
    }
}
