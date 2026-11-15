//! Reads which signals this process inherited as ignored, so `deadband run`
//! leaves each of them ignored and its job inherits the ignore, as it does
//! under `nohup`.
//!
//! On Linux it reads the `SigIgn` mask of `/proc/self/status`, and on macOS
//! it asks `sigaction` through the probe's `macos` module, which only reads.
//! When the disposition cannot be read, the signal counts as not ignored, so
//! `deadband run` passes it on.

use std::ffi::c_int;

/// Returns the signals of `signals` that this process did not inherit as
/// ignored, in their order.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(super) fn not_ignored(signals: &[c_int]) -> Vec<c_int> {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let mask = ignored_mask(&status).unwrap_or(0);
    signals
        .iter()
        .copied()
        .filter(|&signal| !signal_in_mask(mask, signal))
        .collect()
}

/// Returns the signals of `signals` that this process did not inherit as
/// ignored, in their order.
#[cfg(target_os = "macos")]
pub(super) fn not_ignored(signals: &[c_int]) -> Vec<c_int> {
    signals
        .iter()
        .copied()
        .filter(|&signal| !crate::probe::signal_ignored(signal).unwrap_or(false))
        .collect()
}

/// Returns `signals`, because this platform has no reader of the
/// disposition, so no signal counts as ignored.
#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
pub(super) fn not_ignored(signals: &[c_int]) -> Vec<c_int> {
    signals.to_vec()
}

/// Returns the mask on the `SigIgn:` line of a `/proc/<pid>/status` text,
/// or `None` when the line is missing or is not hexadecimal.
#[cfg_attr(
    all(not(any(target_os = "linux", target_os = "android")), not(test)),
    expect(dead_code, reason = "only Linux reads /proc/self/status")
)]
pub(super) fn ignored_mask(status: &str) -> Option<u64> {
    let mask = status
        .lines()
        .find_map(|line| line.strip_prefix("SigIgn:"))?;
    u64::from_str_radix(mask.trim(), 16).ok()
}

/// Says whether `mask`, in which bit `n - 1` stands for signal `n`, holds
/// `signal`.
#[cfg_attr(
    all(not(any(target_os = "linux", target_os = "android")), not(test)),
    expect(dead_code, reason = "only Linux reads /proc/self/status")
)]
pub(super) fn signal_in_mask(mask: u64, signal: c_int) -> bool {
    signal
        .checked_sub(1)
        .and_then(|bit| u32::try_from(bit).ok())
        .and_then(|bit| mask.checked_shr(bit))
        .is_some_and(|shifted| shifted & 1 == 1)
}
