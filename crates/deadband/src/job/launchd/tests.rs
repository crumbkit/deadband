//! Tests of the pure parts of the launchctl runs: what an exit code of
//! `launchctl print` means, and how a failed run reads.

use std::os::unix::process::ExitStatusExt;
use std::process::{ExitStatus, Output};

use super::{command_line, failed, loaded_from};

fn exited(code: i32, stderr: &str) -> Output {
    Output {
        status: ExitStatus::from_raw(code << 8),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn print_exiting_0_means_the_agent_is_loaded() {
    assert_eq!(loaded_from(Some(0)), Some(true));
}

#[test]
fn print_exiting_113_means_launchd_has_not_loaded_the_agent() {
    assert_eq!(loaded_from(Some(113)), Some(false));
}

#[test]
fn print_exiting_with_another_code_or_a_signal_is_a_failure() {
    assert_eq!(loaded_from(Some(5)), None);
    assert_eq!(loaded_from(None), None);
}

#[test]
fn a_command_line_names_the_program_without_its_directory() {
    assert_eq!(
        command_line(
            "/bin/launchctl",
            &["bootout", "gui/501/io.github.crumbkit.deadband"]
        ),
        "launchctl bootout gui/501/io.github.crumbkit.deadband"
    );
}

#[test]
fn a_failed_run_names_the_command_its_exit_code_and_what_it_printed() {
    let output = exited(5, "Bootstrap failed: 5: Input/output error\n");

    let failure = failed(
        "/bin/launchctl",
        &["bootstrap", "gui/501", "/p.plist"],
        &output,
    );

    assert_eq!(
        failure.to_string(),
        "launchctl bootstrap gui/501 /p.plist failed: it exited with 5: Bootstrap failed: 5: Input/output error"
    );
}

#[test]
fn a_failed_run_that_printed_nothing_names_only_its_exit_code() {
    let failure = failed(
        "/usr/bin/getconf",
        &["DARWIN_USER_TEMP_DIR"],
        &exited(1, ""),
    );

    assert_eq!(
        failure.to_string(),
        "getconf DARWIN_USER_TEMP_DIR failed: it exited with 1"
    );
}
