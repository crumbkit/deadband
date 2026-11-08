//! Reads the machine's 1-minute load average, memory pressure level and swap
//! use.

use std::fmt;

use crumbkit::{Code, Failure, Recovery};

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

/// The kernel's memory pressure level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressure {
    /// Memory is plentiful.
    Normal,
    /// The kernel is asking processes to free memory.
    Warn,
    /// The kernel is out of memory to give.
    Critical,
}

impl fmt::Display for Pressure {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Ok(())
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

/// The probe of the machine deadband runs on.
#[derive(Debug, Clone, Copy)]
pub struct System;

impl Probe for System {
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
    /// A sysctl value had a form the probe does not know.
    Value {
        /// The sysctl's name.
        name: &'static str,
        /// What was wrong with the value.
        problem: String,
    },
}

impl fmt::Display for Unreadable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unreadable")
    }
}

impl std::error::Error for Unreadable {}

impl Failure for Unreadable {
    fn code(&self) -> &'static Code {
        &MACHINE_UNREADABLE
    }

    fn recovery(&self) -> Recovery {
        Recovery::Retry
    }
}

/// Reads the value of `kern.memorystatus_vm_pressure_level`.
///
/// # Errors
///
/// Returns [`Unreadable::Value`] for a value of the wrong length or an
/// unknown level.
pub fn parse_pressure(_bytes: &[u8]) -> Result<Pressure, Unreadable> {
    Err(Unreadable::Load)
}

/// Reads the value of `vm.swapusage`.
///
/// # Errors
///
/// Returns [`Unreadable::Value`] for a value of the wrong length.
pub fn parse_swap(_bytes: &[u8]) -> Result<Swap, Unreadable> {
    Err(Unreadable::Load)
}
