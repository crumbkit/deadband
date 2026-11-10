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

#[test]
fn the_system_probe_reads_the_free_and_total_disk_of_a_directory() {
    use super::{Probe, System};

    let disk = System
        .disk(&std::env::temp_dir())
        .expect("the temporary directory's disk reads");
    assert!(disk.total_bytes > 0);
    assert!(disk.free_bytes <= disk.total_bytes);
}

#[test]
fn the_system_probe_says_why_it_cannot_read_a_missing_directory() {
    use super::{Probe, System};

    let missing = std::env::temp_dir().join("deadband-test-no-such-directory");
    let error = System
        .disk(&missing)
        .expect_err("a missing directory has no disk");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[cfg(target_os = "macos")]
mod tree {
    use std::time::{Duration, Instant};

    use crate::probe::{Probe, System};

    #[test]
    fn the_tree_of_a_process_holds_it_and_its_child() {
        #[expect(
            clippy::disallowed_methods,
            reason = "this test needs a real process tree"
        )]
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 2 & wait"])
            .spawn()
            .expect("sh starts");
        let root = child.id();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut tree = System.tree(root).expect("the tree reads");
        while tree.len() < 2 && Instant::now() < deadline {
            std::thread::yield_now();
            tree = System.tree(root).expect("the tree reads");
        }
        let _ = child.wait();
        assert_eq!(tree.len(), 2);
        assert!(tree.iter().any(|each| each.pid == root));
    }

    #[test]
    fn a_process_that_is_gone_reads_as_an_empty_tree() {
        // The highest process ID on macOS is 99998, so this one never exists.
        let tree = System.tree(1_000_000).expect("the read does not fail");
        assert!(tree.is_empty());
    }

    #[test]
    fn this_process_shows_cpu_time() {
        let tree = System.tree(std::process::id()).expect("the tree reads");
        let me = tree.iter().find(|each| each.pid == std::process::id());
        assert!(me.expect("the tree holds this process").cpu > Duration::ZERO);
    }

    #[test]
    fn a_process_owned_by_another_user_reads_as_zero() {
        // Process 1 belongs to root, and libproc refuses its usage to an
        // unprivileged test.
        let tree = System.tree(1).expect("the read does not fail");
        let init = tree.iter().find(|each| each.pid == 1);
        let init = init.expect("the tree holds process 1");
        assert_eq!((init.cpu, init.disk_bytes), (Duration::ZERO, 0));
    }
}

#[test]
fn mach_ticks_scale_by_the_timebase() {
    assert_eq!(
        super::sysctl::ticks(24, 125, 3),
        std::time::Duration::from_nanos(1000)
    );
    assert_eq!(super::sysctl::ticks(5, 1, 0), std::time::Duration::ZERO);
}

#[test]
fn a_failed_sysctl_names_the_sysctl_in_its_fields() {
    use crumbkit::{Failure, FieldValue};

    let failure = parse_pressure(&3_i32.to_ne_bytes()).expect_err("level 3 is unknown");
    let fields = failure.fields();
    assert_eq!(fields.len(), 1, "{fields:?}");
    assert_eq!(fields[0].name(), "sysctl");
    assert_eq!(
        fields[0].value(),
        &FieldValue::from("kern.memorystatus_vm_pressure_level")
    );
}

#[test]
fn a_failed_load_reading_has_no_fields() {
    use crumbkit::Failure;

    assert!(Unreadable::Load.fields().is_empty());
}
