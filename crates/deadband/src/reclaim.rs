//! Asks external reclaimers to free memory or disk when the machine runs
//! short, and keeps the last reclaim for `deadband status`.
//!
//! The daemon's config file, `config.toml` in deadband's configuration
//! directory, lists the reclaimers as commands. deadband knows nothing of
//! what a reclaimer stops; the reclaimer chooses. [`Watch`] is the rule
//! that says when to reclaim, [`Report`] reads what a reclaimer printed,
//! and [`reclaim`] runs the reclaimers on the daemon's behalf.
//!
//! A config file that cannot be read turns reclaiming off, and the daemon
//! says why in one line. A reclaimer that fails never holds a job.

use std::fmt;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;
mod worker;

pub use self::worker::reclaim;

/// The name of the daemon's config file in deadband's configuration
/// directory.
pub const CONFIG_FILE: &str = "config.toml";

/// How long memory must stay short before the daemon reclaims, and how long
/// it waits after a reclaim before the next, when the config sets none.
pub const DEFAULT_AFTER: Duration = Duration::from_secs(60);

/// How long the daemon waits for one run of a reclaimer when the config
/// sets no timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// One reclaimer: a name for the log and `deadband status`, and the command
/// that the daemon runs with `--need` added.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reclaimer {
    /// The name the log and `deadband status` show.
    pub name: String,
    /// The program and its first arguments.
    pub command: Vec<String>,
}

/// What the daemon reads from its config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The reclaimers, in the order the daemon asks them.
    pub reclaimers: Vec<Reclaimer>,
    /// How long memory must stay short before a reclaim, and the wait
    /// after a reclaim before the next.
    pub after: Duration,
    /// How long the daemon waits for one run of a reclaimer.
    pub timeout: Duration,
    /// The memory a reclaim asks for, such as `4G`.
    pub memory: String,
    /// The disk a reclaim asks for, such as `20G`.
    pub disk: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            reclaimers: Vec::new(),
            after: DEFAULT_AFTER,
            timeout: DEFAULT_TIMEOUT,
            memory: "4G".to_owned(),
            disk: "20G".to_owned(),
        }
    }
}

/// The config file as it is written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    /// `Config::after`.
    reclaim_after: Option<String>,
    /// `Config::timeout`.
    reclaim_timeout: Option<String>,
    /// `Config::memory`.
    reclaim_memory: Option<String>,
    /// `Config::disk`.
    reclaim_disk: Option<String>,
    /// `Config::reclaimers`.
    #[serde(default)]
    reclaim: Vec<Reclaimer>,
}

/// Returns the age that `value` gives, or `default` when it is `None`.
fn age(value: Option<String>, default: Duration) -> Result<Duration, String> {
    value.map_or(Ok(default), |value| {
        humantime::parse_duration(&value)
            .map_err(|error| format!("{value:?} is not an age such as \"60s\": {error}"))
    })
}

/// Returns the size that `value` gives, or `default` when it is `None`.
fn size(value: Option<String>, default: String) -> Result<String, String> {
    let value = value.unwrap_or(default);
    match size_bytes(&value) {
        Some(_) => Ok(value),
        None => Err(format!(
            "{value:?} is not a size such as \"4G\": a whole number with K, M, G or T"
        )),
    }
}

impl Config {
    /// Reads a config file's `text`.
    ///
    /// # Errors
    ///
    /// Returns, in one line, why the text is not a config: a TOML error,
    /// an unknown key, an age or size that does not parse, or a reclaimer
    /// with an empty command.
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: File = basic_toml::from_str(text).map_err(|error| one_line(&error))?;
        if let Some(empty) = file.reclaim.iter().find(|each| each.command.is_empty()) {
            return Err(format!("the reclaimer {} has an empty command", empty.name));
        }
        let defaults = Self::default();
        Ok(Self {
            reclaimers: file.reclaim,
            after: age(file.reclaim_after, defaults.after)?,
            timeout: age(file.reclaim_timeout, defaults.timeout)?,
            memory: size(file.reclaim_memory, defaults.memory)?,
            disk: size(file.reclaim_disk, defaults.disk)?,
        })
    }

    /// Reads the config file at `path`. A missing file gives the default
    /// config, which names no reclaimer.
    ///
    /// # Errors
    ///
    /// Returns, in one line, why the file could not be read or parsed.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// Returns `error` as one line.
fn one_line(error: &impl fmt::Display) -> String {
    error
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Returns the bytes of a size such as `4G`: a whole number with an
/// optional `K`, `M`, `G` or `T`, each a power of 1024. Returns `None` for
/// any other text.
#[must_use]
pub fn size_bytes(text: &str) -> Option<u64> {
    let (number, power) = match text.char_indices().last() {
        Some((at, unit)) if unit.is_ascii_alphabetic() => {
            (text.get(..at)?, "KMGT".find(unit)?.checked_add(1)?)
        }
        _ => (text, 0),
    };
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let scale = 1024_u64.checked_pow(u32::try_from(power).ok()?)?;
    number.parse::<u64>().ok()?.checked_mul(scale)
}

/// What a reclaim frees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    /// Memory, while memory pressure stays above normal.
    Memory,
    /// Disk, while free disk is critically low.
    Disk,
}

impl Resource {
    /// Returns the value of `--need` that asks for this resource with the
    /// size that `config` gives, such as `memory=4G`.
    #[must_use]
    pub fn need(self, config: &Config) -> String {
        match self {
            Self::Memory => format!("memory={}", config.memory),
            Self::Disk => format!("disk={}", config.disk),
        }
    }

    /// Returns the bytes that `config` asks for this resource.
    fn need_bytes(self, config: &Config) -> u64 {
        let size = match self {
            Self::Memory => &config.memory,
            Self::Disk => &config.disk,
        };
        size_bytes(size).unwrap_or(u64::MAX)
    }
}

/// The rule that says when the daemon starts a reclaim, fed with what the
/// daemon reads each second. It reads no clock, so the tests check each
/// case directly.
#[derive(Debug, Default, Clone, Copy)]
pub struct Watch {
    /// Since when memory has been short without a break.
    short_since: Option<Instant>,
    /// Whether a reclaim runs now.
    running: bool,
    /// When the last reclaim ended.
    ended: Option<Instant>,
}

impl Watch {
    /// Records that at `now` memory was short, or not, and free disk was
    /// critically low, or not, and returns what to reclaim now. A returned
    /// resource counts as a running reclaim until [`Watch::ended`].
    ///
    /// Nothing is reclaimed without a reclaimer in `config`, while a
    /// reclaim runs, or within `config.after` of the last one's end.
    /// Critically low disk is reclaimed at once, and before memory. Short
    /// memory is reclaimed once it has lasted `config.after`.
    pub fn observe(
        &mut self,
        config: &Config,
        memory_short: bool,
        disk_critical: bool,
        now: Instant,
    ) -> Option<Resource> {
        self.short_since = memory_short.then(|| self.short_since.unwrap_or(now));
        let resting = self
            .ended
            .is_some_and(|ended| now.saturating_duration_since(ended) < config.after);
        if config.reclaimers.is_empty() || self.running || resting {
            return None;
        }
        let lasted = self
            .short_since
            .is_some_and(|since| now.saturating_duration_since(since) >= config.after);
        let resource = if disk_critical {
            Resource::Disk
        } else if lasted {
            Resource::Memory
        } else {
            return None;
        };
        self.running = true;
        Some(resource)
    }

    /// Records that the running reclaim ended at `now`.
    pub const fn ended(&mut self, now: Instant) {
        self.running = false;
        self.ended = Some(now);
    }
}

/// What a reclaimer printed with `--json`: the bytes it freed, or would
/// free on a dry run, and the items that free any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The sum of each item's `freed`.
    pub freed_bytes: u64,
    /// The `name` of each item whose `freed` is above 0.
    pub items: Vec<String>,
}

/// The fields of a reclaimer's JSON that deadband reads. It ignores every
/// other field.
#[derive(Deserialize)]
struct Printed {
    /// The items the reclaimer considered.
    data: Vec<Item>,
}

/// One item of a reclaimer's `data`.
#[derive(Deserialize)]
struct Item {
    /// The item's name, for the log.
    #[serde(default)]
    name: Option<String>,
    /// The bytes the item freed, or would free on a dry run.
    freed: u64,
}

impl Report {
    /// Reads a reclaimer's standard output: one JSON object whose `data`
    /// is an array of objects, each with a `freed` number of bytes and an
    /// optional `name`.
    ///
    /// # Errors
    ///
    /// Returns, in one line, why the output is not such an object.
    pub fn parse(stdout: &[u8]) -> Result<Self, String> {
        let printed: Printed = serde_json::from_slice(stdout).map_err(|error| one_line(&error))?;
        let freed_bytes = printed
            .data
            .iter()
            .fold(0_u64, |sum, item| sum.saturating_add(item.freed));
        let items = printed
            .data
            .into_iter()
            .filter(|item| item.freed > 0)
            .filter_map(|item| item.name)
            .collect();
        Ok(Self { freed_bytes, items })
    }
}

/// The last reclaim, as `deadband status` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Reclaimed {
    /// When the reclaim ended, in RFC 3339 form.
    pub at: String,
    /// The reclaimer's name.
    pub reclaimer: String,
    /// The `--need` that the daemon asked for, such as `memory=4G`.
    pub need: String,
    /// The bytes the reclaimer reported freeing, or `null` when it did not
    /// report them.
    pub freed_bytes: Option<u64>,
    /// Why the reclaim failed, or `null` when it did not.
    pub failed: Option<String>,
}

impl fmt::Display for Reclaimed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "last reclaim: {} asked for {} at {}",
            self.reclaimer, self.need, self.at
        )?;
        if let Some(freed) = self.freed_bytes {
            write!(f, ", freed {}", Bytes(freed))?;
        }
        if let Some(failed) = &self.failed {
            write!(f, ", failed: {failed}")?;
        }
        Ok(())
    }
}

/// A number of bytes, which prints in whole MiB below 1 GiB and in whole
/// GiB from there.
struct Bytes(u64);

impl fmt::Display for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MIB: u64 = 1024 * 1024;
        if self.0 < crate::admit::GIB {
            write!(f, "{} MiB", self.0 / MIB)
        } else {
            write!(f, "{} GiB", self.0 / crate::admit::GIB)
        }
    }
}
