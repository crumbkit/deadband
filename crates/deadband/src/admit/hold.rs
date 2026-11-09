//! Why a waiting job does not start yet, in a form the daemon sends and
//! `deadband run` prints in one line.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{DISK_CRITICAL_BYTES, DISK_CRITICAL_PERCENT, GIB};
use crate::probe::Pressure;

/// Why a waiting job does not start yet.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "hold", rename_all = "snake_case")]
pub enum Hold {
    /// The most jobs run already.
    Full {
        /// The most jobs that run at once.
        max_jobs: usize,
    },
    /// The job's lane runs all but one place, which stays open for the
    /// other lane.
    Lane {
        /// The most jobs that one lane runs.
        places: usize,
    },
    /// The disk is critically short of free space.
    Disk {
        /// The disk's free bytes.
        free_bytes: u64,
        /// The disk's total bytes.
        total_bytes: u64,
    },
    /// Memory pressure is above normal.
    Memory {
        /// The memory pressure level.
        pressure: Pressure,
    },
    /// The load, with the threads of the jobs started in the last minute,
    /// exceeds the cores.
    Load {
        /// The 1-minute load plus those threads.
        projected: f64,
        /// The number of logical cores.
        cores: usize,
    },
}

impl fmt::Display for Hold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full { max_jobs: 1 } => {
                f.write_str("the daemon already runs 1 job, its most at once")
            }
            Self::Full { max_jobs } => {
                write!(
                    f,
                    "the daemon already runs {max_jobs} jobs, its most at once"
                )
            }
            Self::Lane { places } => write!(
                f,
                "the job's lane already runs {places} {}, and the last place stays open for the other lane",
                if *places == 1 { "job" } else { "jobs" }
            ),
            Self::Disk {
                free_bytes,
                total_bytes,
            } => write!(
                f,
                "free disk is {} GiB, {}% of the disk, under {} GiB or {DISK_CRITICAL_PERCENT}%",
                free_bytes / GIB,
                percent(*free_bytes, *total_bytes),
                DISK_CRITICAL_BYTES / GIB
            ),
            Self::Memory { pressure } => write!(f, "memory pressure is {pressure}"),
            Self::Load { projected, cores } => write!(
                f,
                "the 1-minute load with the jobs started in the last minute is {projected:.1}, over the {cores} cores"
            ),
        }
    }
}

/// Returns `part` as a whole percentage of `whole`, rounded down, or 100
/// for an empty whole.
pub(super) fn percent(part: u64, whole: u64) -> u64 {
    part.saturating_mul(100).checked_div(whole).unwrap_or(100)
}
