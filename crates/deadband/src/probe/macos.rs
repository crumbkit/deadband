//! The macOS reader. It is the only module with unsafe code.
//!
//! The invariant: each unsafe call writes only into a buffer that this
//! module owns, and passes that buffer's exact length, so the kernel or libc
//! never writes past its end. Every name passed to `sysctlbyname` is a
//! NUL-terminated `CStr`, and no call passes a new value to the kernel.

use std::ffi::CStr;

use super::{
    PRESSURE_BYTES, PRESSURE_SYSCTL, SWAP_SYSCTL, SWAP_USAGE_BYTES, Sample, Unreadable,
    parse_pressure, parse_swap,
};

// The parser reads `vm.swapusage` at the length of macOS's `xsw_usage`.
const _: () = assert!(size_of::<libc::xsw_usage>() == SWAP_USAGE_BYTES);
const _: () = assert!(size_of::<libc::c_int>() == PRESSURE_BYTES);

/// Takes one sample of the machine.
pub(super) fn sample() -> Result<Sample, Unreadable> {
    Ok(Sample {
        load_1m: load_1m()?,
        memory_pressure: parse_pressure(&sysctl(PRESSURE_SYSCTL, PRESSURE_BYTES)?)?,
        swap: parse_swap(&sysctl(SWAP_SYSCTL, SWAP_USAGE_BYTES)?)?,
    })
}

/// Returns the 1-minute load average.
fn load_1m() -> Result<f64, Unreadable> {
    let mut averages = [0.0_f64; 1];
    // SAFETY: `averages` holds one f64, and getloadavg writes at most the
    // one value that `nelem` asks for.
    let written = unsafe { libc::getloadavg(averages.as_mut_ptr(), 1) };
    match (written, averages.first()) {
        (1, Some(load)) => Ok(*load),
        _ => Err(Unreadable::Load),
    }
}

/// Returns the value of the sysctl `name`, which is at most `len` bytes.
fn sysctl(name: &'static CStr, len: usize) -> Result<Vec<u8>, Unreadable> {
    let mut value = vec![0_u8; len];
    let mut written = value.len();
    // SAFETY: `name` is NUL-terminated. `value` is writable for `written`
    // bytes, and sysctlbyname writes at most that many and stores the count
    // it wrote back in `written`. The new value is null with length 0, so
    // the call changes nothing in the kernel.
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            &raw mut written,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return Err(Unreadable::Sysctl {
            name,
            error: std::io::Error::last_os_error(),
        });
    }
    value.truncate(written);
    Ok(value)
}
