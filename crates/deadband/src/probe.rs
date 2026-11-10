//! Reads the machine's 1-minute load average, memory pressure level and swap
//! use, and the free space of a disk.
//!
//! The command and, later, the daemon take any [`Probe`], and tests pass a
//! fake one. [`System`] reads the machine deadband runs on.
//! On macOS it calls `getloadavg` and `sysctlbyname` in the `macos` module,
//! which is the only module with unsafe code. The `sysctl` module
//! parses each sysctl value in safe code that runs on every platform.

use std::ffi::CStr;
use std::fmt;
use std::io;
use std::path::Path;
use std::time::Duration;

use crumbkit::{Code, Failure, Recovery};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "crates/deadband/src/probe/macos.rs calls getloadavg, sysctlbyname and libproc; rust-standards.toml's unsafe decision states its invariant"
)]
mod macos;
// The parsing of the sysctl values runs on every platform, so the tests
// check it everywhere, but only the macOS reader calls it.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(
        dead_code,
        reason = "only the macOS reader of the probe reads sysctl values"
    )
)]
mod sysctl;
#[cfg(test)]
mod tests;

/// The code of a failure to read the machine on a platform that deadband
/// reads.
pub const MACHINE_UNREADABLE: Code = Code::new("deadband", "machine", "unreadable");

/// The code of a failure to read the machine on a platform that deadband
/// does not read yet.
pub const MACHINE_UNSUPPORTED: Code = Code::new("deadband", "machine", "unsupported");

/// Something that takes one sample of the machine.
pub trait Probe {
    /// Takes one sample of the machine.
    ///
    /// # Errors
    ///
    /// Returns [`Unreadable`] when a value cannot be read.
    fn sample(&self) -> Result<Sample, Unreadable>;

    /// Reads the free and total space of the disk that holds `path`.
    ///
    /// # Errors
    ///
    /// Returns the error of a reading that failed, such as a missing path.
    fn disk(&self, path: &Path) -> io::Result<Disk>;

    /// Reads each process of the tree under `root`, `root` included, with
    /// its CPU time and disk bytes. A process that exits while the tree is
    /// read is left out, and a child whose usage this user may not read
    /// reads as zero. A probe that reads no trees returns
    /// [`TreeUnreadable::Unsupported`], which flags no job.
    ///
    /// # Errors
    ///
    /// Returns [`TreeUnreadable`] when the tree cannot be read.
    fn tree(&self, root: u32) -> Result<Vec<ProcessUse>, TreeUnreadable> {
        let _ = root;
        Err(TreeUnreadable::Unsupported)
    }
}

/// One process of a job's tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessUse {
    /// The process's ID.
    pub pid: u32,
    /// The CPU time the process has used.
    pub cpu: Duration,
    /// The bytes the process has read from and written to disk.
    pub disk_bytes: u64,
}

/// Why a process tree could not be read.
#[derive(Debug)]
pub enum TreeUnreadable {
    /// deadband reads process trees only on macOS.
    Unsupported,
    /// A call to read the process table failed.
    Failed(io::Error),
}

impl fmt::Display for TreeUnreadable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("deadband reads process trees only on macOS"),
            Self::Failed(error) => write!(f, "the process table could not be read: {error}"),
        }
    }
}

/// The space of one disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Disk {
    /// The bytes that a user without special privileges may still write.
    pub free_bytes: u64,
    /// The bytes the disk holds in all.
    pub total_bytes: u64,
}

/// One sample of the machine.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, JsonSchema)]
pub struct Sample {
    /// The 1-minute load average.
    pub load_1m: f64,
    /// The kernel's memory pressure level.
    pub memory_pressure: Pressure,
    /// The bytes of swap in use.
    pub swap_used_bytes: u64,
    /// The bytes of swap the machine has.
    pub swap_total_bytes: u64,
}

/// The number of bytes in a mebibyte, which the text output counts swap in.
const MEBIBYTE: u64 = 1_048_576;

impl fmt::Display for Sample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "load: {:.2} (1-minute average)", self.load_1m)?;
        writeln!(f, "memory pressure: {}", self.memory_pressure)?;
        write!(
            f,
            "swap: {} MiB used of {} MiB",
            self.swap_used_bytes / MEBIBYTE,
            self.swap_total_bytes / MEBIBYTE
        )
    }
}

/// The kernel's memory pressure level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Pressure {
    /// Memory is plentiful.
    Normal,
    /// The kernel is asking processes to free memory.
    Warn,
    /// The kernel's most severe pressure level.
    Critical,
}

impl Pressure {
    /// Returns the level's name: `normal`, `warn` or `critical`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Warn => "warn",
            Self::Critical => "critical",
        }
    }
}

impl fmt::Display for Pressure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The probe of the machine deadband runs on. It reads macOS, and on any
/// other platform every sample fails with [`Unreadable::Unsupported`].
#[derive(Debug, Clone, Copy)]
pub struct System;

impl Probe for System {
    #[cfg(target_os = "macos")]
    fn sample(&self) -> Result<Sample, Unreadable> {
        macos::sample()
    }

    #[cfg(not(target_os = "macos"))]
    fn sample(&self) -> Result<Sample, Unreadable> {
        Err(Unreadable::Unsupported)
    }

    fn disk(&self, path: &Path) -> io::Result<Disk> {
        let disk = rustix::fs::statvfs(path)?;
        Ok(Disk {
            free_bytes: disk.f_bavail.saturating_mul(disk.f_frsize),
            total_bytes: disk.f_blocks.saturating_mul(disk.f_frsize),
        })
    }

    #[cfg(target_os = "macos")]
    fn tree(&self, root: u32) -> Result<Vec<ProcessUse>, TreeUnreadable> {
        macos::tree(root)
    }
}

/// A reading of the machine that failed.
#[derive(Debug, Failure)]
pub enum Unreadable {
    /// deadband does not read the machine on this platform yet.
    #[failure(
        code = MACHINE_UNSUPPORTED,
        no_next_step = "deadband has no probe for this platform yet"
    )]
    Unsupported,
    /// `getloadavg` returned no 1-minute load average.
    #[failure(code = MACHINE_UNREADABLE, recovery = Recovery::Retry)]
    Load,
    /// A `sysctlbyname` call failed.
    #[failure(
        code = MACHINE_UNREADABLE,
        recovery = Recovery::Retry,
        field(sysctl = name.to_string_lossy())
    )]
    Sysctl {
        /// The sysctl's name.
        name: &'static CStr,
        /// The error the call set.
        error: io::Error,
    },
    /// A sysctl value had a form the probe does not know.
    #[failure(
        code = MACHINE_UNREADABLE,
        no_next_step = "this macOS gives the value in a form deadband does not know, so a retry reads the same form",
        field(sysctl = name.to_string_lossy())
    )]
    Value {
        /// The sysctl's name.
        name: &'static CStr,
        /// What was wrong with the value.
        problem: String,
    },
}

impl fmt::Display for Unreadable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => f.write_str("deadband reads the machine only on macOS"),
            Self::Load => f.write_str("deadband could not read the 1-minute load average"),
            Self::Sysctl { name, error } => write!(
                f,
                "deadband could not read the sysctl {}: {error}",
                name.to_string_lossy()
            ),
            Self::Value { name, problem } => write!(
                f,
                "deadband could not read the sysctl {}: {problem}",
                name.to_string_lossy()
            ),
        }
    }
}

impl std::error::Error for Unreadable {}
