//! The sysctls that the macOS reader reads, the parsing of their values, and
//! the scaling of Mach clock ticks, in safe code that runs on every platform.

use std::ffi::CStr;
use std::time::Duration;

use super::{Pressure, Unreadable};

/// The sysctl that holds the kernel's memory pressure level.
pub(crate) const PRESSURE_SYSCTL: &CStr = c"kern.memorystatus_vm_pressure_level";

/// The sysctl that holds the swap in use and the swap the machine has.
pub(crate) const SWAP_SYSCTL: &CStr = c"vm.swapusage";

/// The length of the value of `vm.swapusage`, the C struct `xsw_usage`:
/// three 64-bit counts of bytes (total, available and used), the page size
/// and whether swap is encrypted.
pub(crate) const SWAP_USAGE_BYTES: usize = 32;

/// The length of the value of `kern.memorystatus_vm_pressure_level`, a C
/// `int`.
pub(crate) const PRESSURE_BYTES: usize = 4;

/// The swap in use and the swap the machine has, in bytes, as
/// `vm.swapusage` gives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Swap {
    /// The bytes of swap in use.
    pub(crate) used_bytes: u64,
    /// The bytes of swap the machine has.
    pub(crate) total_bytes: u64,
}

/// Returns the duration of `ticks` of the Mach clock, whose timebase is
/// `numer` over `denom` nanoseconds a tick.
pub(crate) fn ticks(ticks: u64, numer: u32, denom: u32) -> Duration {
    let nanos = u128::from(ticks)
        .saturating_mul(u128::from(numer))
        .checked_div(u128::from(denom))
        .unwrap_or(0);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// Reads the value of `kern.memorystatus_vm_pressure_level`: 1 is normal, 2
/// is warn and 4 is critical.
///
/// # Errors
///
/// Returns [`Unreadable::Value`] for a value of the wrong length or an
/// unknown level.
pub(crate) fn parse_pressure(bytes: &[u8]) -> Result<Pressure, Unreadable> {
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
pub(crate) fn parse_swap(bytes: &[u8]) -> Result<Swap, Unreadable> {
    let value: [u8; SWAP_USAGE_BYTES] = bytes.try_into().map_err(|_| Unreadable::Value {
        name: SWAP_SYSCTL,
        problem: format!(
            "the value is {} bytes long, not {SWAP_USAGE_BYTES}",
            bytes.len()
        ),
    })?;
    // The total, the available and the used bytes are the first three
    // 8-byte words. The page size and the encryption flag follow.
    #[rustfmt::skip]
    let [
        t0, t1, t2, t3, t4, t5, t6, t7,
        _, _, _, _, _, _, _, _,
        u0, u1, u2, u3, u4, u5, u6, u7,
        ..
    ] = value;
    Ok(Swap {
        used_bytes: u64::from_ne_bytes([u0, u1, u2, u3, u4, u5, u6, u7]),
        total_bytes: u64::from_ne_bytes([t0, t1, t2, t3, t4, t5, t6, t7]),
    })
}
