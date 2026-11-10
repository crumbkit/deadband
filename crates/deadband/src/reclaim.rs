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
use std::time::{Duration, SystemTime};

use crumbkit::{Age, Document};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::stuck;

#[cfg(test)]
mod tests;
mod watch;
mod worker;

pub use self::watch::{MAX_REST, Watch};
pub use self::worker::reclaim;

/// The name of the daemon's config file in deadband's configuration
/// directory.
pub const CONFIG_FILE: &str = "config.toml";

/// The format of the daemon's config file, which `deadband schemas show
/// deadband/config/v1` prints as a JSON Schema.
pub(crate) const CONFIG: Document<File> = Document::new(
    "deadband",
    "config",
    1,
    "The daemon's config.toml, which names the reclaimers and says when the daemon reclaims memory or disk, when a job counts as stuck, which directories show a warm build and which tools keep their subcommands in the job history.",
);

/// How long memory must stay short before the daemon reclaims, and how long
/// it waits after a reclaim before the next, when the config sets none.
pub const DEFAULT_AFTER: Duration = Duration::from_secs(60);

/// The shortest `reclaim_after` that the config file accepts. A shorter one
/// would ask a reclaimer again before it could have freed anything.
pub const MIN_AFTER: Duration = Duration::from_secs(10);

/// How long the daemon waits for one run of a reclaimer when the config
/// sets no timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// One reclaimer: a name for the log and `deadband status`, and the command
/// that the daemon runs with `--need` added.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reclaimer {
    /// The name that the daemon's log and `deadband status` show for the
    /// reclaimer.
    pub name: String,
    /// The program, as an absolute path, and its first arguments. The
    /// daemon adds `--need` and, for a dry run, `--dry-run`.
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
    /// The keys that say how the daemon detects stuck jobs.
    pub stuck: stuck::Config,
    /// The directories, relative to a job's checkout, whose having an entry
    /// makes the job's history line say it started warm.
    pub warm_dirs: Vec<String>,
    /// The program file names whose kind in the job history keeps up to
    /// two subcommand words.
    pub kind_tools: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            reclaimers: Vec::new(),
            after: DEFAULT_AFTER,
            timeout: DEFAULT_TIMEOUT,
            memory: "4G".to_owned(),
            disk: "20G".to_owned(),
            stuck: stuck::Config::default(),
            warm_dirs: Vec::new(),
            kind_tools: Vec::new(),
        }
    }
}

/// The config file as it is written. Every key is optional.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct File {
    /// How long memory stays short before a reclaim, and the wait between
    /// reclaims, as an age such as "60s". It is at least 10s, and 60s when
    /// the file leaves it out.
    reclaim_after: Option<String>,
    /// How long the daemon waits for one run of a reclaimer, as an age such
    /// as "5m". It is more than 0, and 5m when the file leaves it out.
    reclaim_timeout: Option<String>,
    /// The memory each reclaim asks for, as a whole number with an optional
    /// K, M, G or T, such as "4G". It is 4G when the file leaves it out.
    reclaim_memory: Option<String>,
    /// The disk each reclaim asks for, as a whole number with an optional
    /// K, M, G or T, such as "20G". It is 20G when the file leaves it out.
    reclaim_disk: Option<String>,
    /// The reclaimers, which the daemon asks in this order. Without one,
    /// the daemon never reclaims.
    #[serde(default)]
    reclaim: Vec<Reclaimer>,
    /// How long a job's process tree shows no activity before the job
    /// counts as stuck, as an age of at least 1m such as "5m", or "off" to
    /// turn detection off. It is 5m when the file leaves it out.
    stuck_after: Option<String>,
    /// Regular expressions. A job whose command line matches any of them is
    /// not watched for being stuck.
    #[serde(default)]
    stuck_ignore: Vec<String>,
    /// The program, as an absolute path, and its arguments, which the
    /// daemon runs once each time a job becomes stuck.
    on_stuck: Option<Vec<String>>,
    /// Directories, relative to a job's checkout, such as "target". The
    /// job history marks a job as warm when one of them has an entry under
    /// the job's checkout when the job asks.
    #[serde(default)]
    warm_dirs: Vec<String>,
    /// Program file names, such as "cargo". The job history's kind of a
    /// job whose program is listed keeps up to two plain lowercase words
    /// after it, such as "cargo nextest run", and the kind of any other job
    /// is the program's file name alone.
    #[serde(default)]
    kind_tools: Vec<String>,
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
    /// an unknown key, an age or size that does not parse, a
    /// `reclaim_after` under [`MIN_AFTER`], a zero `reclaim_timeout`, or a
    /// reclaimer with an empty command, or a stuck key that
    /// [`stuck::Config::parse`] refuses.
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: File = basic_toml::from_str(text).map_err(|error| one_line(&error))?;
        if let Some(empty) = file.reclaim.iter().find(|each| each.command.is_empty()) {
            return Err(format!("the reclaimer {} has an empty command", empty.name));
        }
        let defaults = Self::default();
        let after = age(file.reclaim_after, defaults.after)?;
        if after < MIN_AFTER {
            return Err(format!(
                "reclaim_after is {}, under the shortest it accepts, 10s",
                humantime::format_duration(after)
            ));
        }
        let timeout = age(file.reclaim_timeout, defaults.timeout)?;
        if timeout.is_zero() {
            return Err("reclaim_timeout is 0, and a reclaimer needs time to run".to_owned());
        }
        Ok(Self {
            reclaimers: file.reclaim,
            after,
            timeout,
            memory: size(file.reclaim_memory, defaults.memory)?,
            disk: size(file.reclaim_disk, defaults.disk)?,
            stuck: stuck::Config::parse(
                file.stuck_after.as_deref(),
                &file.stuck_ignore,
                file.on_stuck,
            )?,
            warm_dirs: file.warm_dirs,
            kind_tools: file.kind_tools,
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
    /// When the reclaim ended, which JSON holds in RFC 3339 and text shows
    /// as how long ago that was.
    #[serde(deserialize_with = "ended_at")]
    pub at: Age,
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
            "last reclaim: {} asked for {} {} ago",
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

/// Reads the RFC 3339 time at which a reclaim ended, as the daemon wrote it,
/// and measures its age now, when the client reads the daemon's answer.
fn ended_at<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Age, D::Error> {
    let text = String::deserialize(deserializer)?;
    let at = humantime::parse_rfc3339_weak(&text).map_err(serde::de::Error::custom)?;
    Ok(Age::since(at, SystemTime::now()))
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
