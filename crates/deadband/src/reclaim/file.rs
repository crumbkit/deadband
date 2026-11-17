//! The daemon's config file as it is written, before [`super::Config`]
//! checks its values.

use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;

use super::size_bytes;
use super::text::parse_age;

/// One reclaimer: a name for the log and `deadband status`, and the command
/// that the daemon runs with `--need` added.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "Reclaimer")]
pub(super) struct Reclaimer {
    /// The name that the daemon's log and `deadband status` show for the
    /// reclaimer.
    pub(super) name: String,
    /// The program, as an absolute path, and its first arguments. The
    /// daemon adds `--need` and, for a dry run, `--dry-run`.
    pub(super) command: Vec<String>,
}

/// The config file as it is written. Every key is optional.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct File {
    /// How long memory stays short before a reclaim, and the wait between
    /// reclaims, as an age such as "60s". It is at least 10s, and 60s when
    /// the file leaves it out.
    pub(super) reclaim_after: Option<String>,
    /// How long the daemon waits for one run of a reclaimer, as an age such
    /// as "5m". It is more than 0, and 5m when the file leaves it out.
    pub(super) reclaim_timeout: Option<String>,
    /// The memory each reclaim asks for, as a whole number with an optional
    /// K, M, G or T, such as "4G". It is 4G when the file leaves it out.
    pub(super) reclaim_memory: Option<String>,
    /// The disk each reclaim asks for, as a whole number with an optional
    /// K, M, G or T, such as "20G". It is 20G when the file leaves it out.
    pub(super) reclaim_disk: Option<String>,
    /// The reclaimers, which the daemon asks in this order. Without one,
    /// the daemon never reclaims.
    #[serde(default)]
    pub(super) reclaim: Vec<Reclaimer>,
    /// How long a job's process tree shows no activity before the job
    /// counts as stuck, as an age of at least 1m such as "5m", or "off" to
    /// turn detection off. It is 5m when the file leaves it out.
    pub(super) stuck_after: Option<String>,
    /// Regular expressions. A job whose command line matches any of them is
    /// not watched for being stuck.
    #[serde(default)]
    pub(super) stuck_ignore: Vec<String>,
    /// The program, as an absolute path, and its arguments, which the
    /// daemon runs once each time a job becomes stuck.
    pub(super) on_stuck: Option<Vec<String>>,
    /// Directories, relative to a job's checkout, such as "target". The
    /// job history marks a job as warm when one of them has an entry under
    /// the job's checkout when the job asks.
    #[serde(default)]
    pub(super) warm_dirs: Vec<String>,
    /// Program file names, such as "cargo". The job history's kind of a
    /// job whose program is listed keeps up to two plain lowercase words
    /// after it, such as "cargo nextest run", and the kind of any other job
    /// is the program's file name alone.
    #[serde(default)]
    pub(super) kind_tools: Vec<String>,
}

/// Returns the age that `value` gives, or `default` when it is `None`.
pub(super) fn age(value: Option<String>, default: Duration) -> Result<Duration, String> {
    value.map_or(Ok(default), |value| {
        parse_age(&value)
            .map_err(|error| format!("{value:?} is not an age such as \"60s\": {error}"))
    })
}

/// Returns the size that `value` gives, or `default` when it is `None`.
pub(super) fn size(value: Option<String>, default: String) -> Result<String, String> {
    let value = value.unwrap_or(default);
    match size_bytes(&value) {
        Some(_) => Ok(value),
        None => Err(format!(
            "{value:?} is not a size such as \"4G\": a whole number with K, M, G or T"
        )),
    }
}
