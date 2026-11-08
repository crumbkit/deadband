//! Tests of the probe's parsing of each sysctl value.

use super::sysctl::{Swap, parse_pressure, parse_swap};
use super::{Pressure, Unreadable};

/// The bytes that `vm.swapusage` returns: total, available and used bytes,
/// the page size and whether swap is encrypted.
fn swap_usage(total: u64, available: u64, used: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&total.to_ne_bytes());
    bytes.extend_from_slice(&available.to_ne_bytes());
    bytes.extend_from_slice(&used.to_ne_bytes());
    bytes.extend_from_slice(&16_384_u32.to_ne_bytes());
    bytes.extend_from_slice(&1_u32.to_ne_bytes());
    bytes
}

#[test]
fn pressure_level_1_is_normal() {
    assert_eq!(
        parse_pressure(&1_i32.to_ne_bytes()).ok(),
        Some(Pressure::Normal)
    );
}

#[test]
fn pressure_level_2_is_warn() {
    assert_eq!(
        parse_pressure(&2_i32.to_ne_bytes()).ok(),
        Some(Pressure::Warn)
    );
}

#[test]
fn pressure_level_4_is_critical() {
    assert_eq!(
        parse_pressure(&4_i32.to_ne_bytes()).ok(),
        Some(Pressure::Critical)
    );
}

#[test]
fn an_unknown_pressure_level_is_unreadable() {
    assert!(matches!(
        parse_pressure(&3_i32.to_ne_bytes()),
        Err(Unreadable::Value { .. })
    ));
}

#[test]
fn a_pressure_level_of_the_wrong_length_is_unreadable() {
    assert!(matches!(
        parse_pressure(&[1, 0]),
        Err(Unreadable::Value { .. })
    ));
}

#[test]
fn swap_usage_gives_the_used_and_total_bytes() {
    let swap = parse_swap(&swap_usage(13_958_643_712, 1_789_394_944, 12_169_248_768)).ok();
    assert_eq!(
        swap,
        Some(Swap {
            used_bytes: 12_169_248_768,
            total_bytes: 13_958_643_712,
        })
    );
}

#[test]
fn swap_usage_of_the_wrong_length_is_unreadable() {
    let mut bytes = swap_usage(1, 1, 0);
    bytes.truncate(24);
    assert!(matches!(parse_swap(&bytes), Err(Unreadable::Value { .. })));
}

#[test]
fn pressure_names_read_as_the_kernel_levels() {
    let names: Vec<String> = [Pressure::Normal, Pressure::Warn, Pressure::Critical]
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(names, ["normal", "warn", "critical"]);
}

#[cfg(target_os = "macos")]
#[test]
fn the_macos_probe_reads_the_machine() {
    use super::{Probe, System};

    let sample = System.sample().expect("the macOS probe reads the machine");
    assert!(sample.swap_used_bytes <= sample.swap_total_bytes);
    assert!(sample.load_1m >= 0.0);
}

#[test]
fn a_value_of_an_unknown_form_offers_no_retry() {
    use crumbkit::{Failure, Recovery};

    let failure = parse_pressure(&3_i32.to_ne_bytes()).expect_err("level 3 is unknown");
    assert!(matches!(failure.recovery(), Recovery::None { .. }));
}

#[test]
fn a_failed_load_reading_offers_a_retry() {
    use crumbkit::{Failure, Recovery};

    assert!(matches!(Unreadable::Load.recovery(), Recovery::Retry));
}
