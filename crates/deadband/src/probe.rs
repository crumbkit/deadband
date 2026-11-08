//! Reads the machine's 1-minute load average, memory pressure level and swap
//! use.
//!
//! [`Probe`] is the seam: the command and, later, the daemon take any probe,
//! and tests pass a fake one. [`System`] reads the machine deadband runs on.
//! On macOS it calls `getloadavg` and `sysctlbyname` in the `macos` module,
//! which is the only module with unsafe code. The parsing of each sysctl
//! value lives here, in safe code that runs on every platform.

use std::borrow::Cow;
use std::ffi::CStr;
use std::fmt;

use crumbkit::schemars::{self, JsonSchema, Schema, SchemaGenerator};
use crumbkit::serde::ser::SerializeStruct;
use crumbkit::serde::{Serialize, Serializer};
use crumbkit::{Code, Failure, Recovery};

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "crates/deadband/src/probe/macos.rs calls getloadavg and sysctlbyname; rust-standards.toml's unsafe decision states its invariant"
)]
mod macos;
#[cfg(test)]
mod tests;

/// The code of a failure to read the machine on a platform that deadband
/// reads.
pub const MACHINE_UNREADABLE: Code = Code::new("deadband", "machine", "unreadable");

/// The code of a failure to read the machine on a platform that deadband
/// does not read yet.
pub const MACHINE_UNSUPPORTED: Code = Code::new("deadband", "machine", "unsupported");

/// The sysctl that holds the kernel's memory pressure level.
pub const PRESSURE_SYSCTL: &CStr = c"kern.memorystatus_vm_pressure_level";

/// The sysctl that holds the swap in use and the swap the machine has.
pub const SWAP_SYSCTL: &CStr = c"vm.swapusage";

/// The length of the value of `vm.swapusage`, the C struct `xsw_usage`:
/// three 64-bit counts of bytes (total, available and used), the page size
/// and whether swap is encrypted.
pub const SWAP_USAGE_BYTES: usize = 32;

/// The length of the value of `kern.memorystatus_vm_pressure_level`, a C
/// `int`.
pub const PRESSURE_BYTES: usize = 4;

/// Something that takes one sample of the machine.
pub trait Probe {
    /// Takes one sample of the machine.
    ///
    /// # Errors
    ///
    /// Returns [`Unreadable`] when a value cannot be read.
    fn sample(&self) -> Result<Sample, Unreadable>;
}

/// One sample of the machine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// The 1-minute load average.
    pub load_1m: f64,
    /// The kernel's memory pressure level.
    pub memory_pressure: Pressure,
    /// The swap in use and the swap the machine has.
    pub swap: Swap,
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
            self.swap.used_bytes / MEBIBYTE,
            self.swap.total_bytes / MEBIBYTE
        )
    }
}

/// The sample's JSON form, which `deadband status --json` prints under
/// `data`.
impl Serialize for Sample {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut fields = serializer.serialize_struct("Sample", 4)?;
        fields.serialize_field("load_1m", &self.load_1m)?;
        fields.serialize_field("memory_pressure", self.memory_pressure.name())?;
        fields.serialize_field("swap_used_bytes", &self.swap.used_bytes)?;
        fields.serialize_field("swap_total_bytes", &self.swap.total_bytes)?;
        fields.end()
    }
}

/// The JSON Schema of the sample's JSON form.
impl JsonSchema for Sample {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("Sample")
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        schemars::json_schema!({
            "description": "One sample of the machine.",
            "type": "object",
            "properties": {
                "load_1m": {
                    "description": "The 1-minute load average.",
                    "type": "number",
                },
                "memory_pressure": {
                    "description": "The kernel's memory pressure level.",
                    "enum": ["normal", "warn", "critical"],
                },
                "swap_used_bytes": {
                    "description": "The bytes of swap in use.",
                    "type": "integer",
                    "minimum": 0,
                },
                "swap_total_bytes": {
                    "description": "The bytes of swap the machine has.",
                    "type": "integer",
                    "minimum": 0,
                },
            },
            "required": ["load_1m", "memory_pressure", "swap_used_bytes", "swap_total_bytes"],
            "additionalProperties": false,
        })
    }
}

/// The kernel's memory pressure level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressure {
    /// Memory is plentiful.
    Normal,
    /// The kernel is asking processes to free memory.
    Warn,
    /// The kernel has run out of memory to give and is ending processes.
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

/// The swap in use and the swap the machine has, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swap {
    /// The bytes of swap in use.
    pub used_bytes: u64,
    /// The bytes of swap the machine has.
    pub total_bytes: u64,
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
}

/// A reading of the machine that failed.
#[derive(Debug)]
pub enum Unreadable {
    /// deadband does not read the machine on this platform yet.
    Unsupported,
    /// `getloadavg` returned no 1-minute load average.
    Load,
    /// A `sysctlbyname` call failed.
    Sysctl {
        /// The sysctl's name.
        name: &'static CStr,
        /// The error the call set.
        error: std::io::Error,
    },
    /// A sysctl value had a form the probe does not know.
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

impl Failure for Unreadable {
    fn code(&self) -> &'static Code {
        match self {
            Self::Unsupported => &MACHINE_UNSUPPORTED,
            Self::Load | Self::Sysctl { .. } | Self::Value { .. } => &MACHINE_UNREADABLE,
        }
    }

    fn recovery(&self) -> Recovery {
        match self {
            Self::Unsupported => Recovery::None {
                reason: "deadband has no probe for this platform yet".to_owned(),
            },
            Self::Load | Self::Sysctl { .. } | Self::Value { .. } => Recovery::Retry,
        }
    }
}

/// Reads the value of `kern.memorystatus_vm_pressure_level`: 1 is normal, 2
/// is warn and 4 is critical.
///
/// # Errors
///
/// Returns [`Unreadable::Value`] for a value of the wrong length or an
/// unknown level.
pub fn parse_pressure(bytes: &[u8]) -> Result<Pressure, Unreadable> {
    let level: [u8; PRESSURE_BYTES] = bytes.try_into().map_err(|_| Unreadable::Value {
        name: PRESSURE_SYSCTL,
        problem: format!(
            "the value is {} bytes long, not {PRESSURE_BYTES}",
            bytes.len()
        ),
    })?;
    match i32::from_ne_bytes(level) {
        1 => Ok(Pressure::Normal),
        2 => Ok(Pressure::Warn),
        4 => Ok(Pressure::Critical),
        other => Err(Unreadable::Value {
            name: PRESSURE_SYSCTL,
            problem: format!("the level {other} is none of 1, 2 or 4"),
        }),
    }
}

/// Reads the value of `vm.swapusage`, the C struct `xsw_usage`, whose total
/// bytes start at offset 0 and used bytes at offset 16.
///
/// # Errors
///
/// Returns [`Unreadable::Value`] for a value of the wrong length.
pub fn parse_swap(bytes: &[u8]) -> Result<Swap, Unreadable> {
    let wrong_length = || Unreadable::Value {
        name: SWAP_SYSCTL,
        problem: format!(
            "the value is {} bytes long, not {SWAP_USAGE_BYTES}",
            bytes.len()
        ),
    };
    if bytes.len() != SWAP_USAGE_BYTES {
        return Err(wrong_length());
    }
    match (u64_at(bytes, 0), u64_at(bytes, 16)) {
        (Some(total_bytes), Some(used_bytes)) => Ok(Swap {
            used_bytes,
            total_bytes,
        }),
        _ => Err(wrong_length()),
    }
}

/// Returns the native-endian `u64` at `offset` in `bytes`, or `None` when
/// `bytes` ends first.
fn u64_at(bytes: &[u8], offset: usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    let word: [u8; 8] = bytes.get(offset..end)?.try_into().ok()?;
    Some(u64::from_ne_bytes(word))
}
