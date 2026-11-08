//! Tests of starting a job and reading how it ended.

use std::ffi::OsString;
use std::io::ErrorKind;

use super::{exit_code, run};

fn words(command: &[&str]) -> Vec<OsString> {
    command.iter().map(OsString::from).collect()
}

#[test]
fn run_returns_the_exit_code_of_the_job() {
    let status = run(&words(&["sh", "-c", "exit 3"]), &[]).expect("sh starts");
    assert_eq!(exit_code(status), 3);
}

#[test]
fn run_passes_each_argument_as_a_separate_value() {
    let status = run(&words(&["test", "a b", "=", "a b"]), &[]).expect("test starts");
    assert_eq!(exit_code(status), 0);
}

#[test]
fn a_job_that_a_signal_ends_exits_with_128_plus_the_signal() {
    let status = run(&words(&["sh", "-c", "kill -TERM $$"]), &[]).expect("sh starts");
    assert_eq!(exit_code(status), 143);
}

#[test]
fn run_fails_with_not_found_for_a_program_that_does_not_exist() {
    let error = run(&words(&["deadband-test-no-such-program"]), &[]).expect_err("nothing starts");
    assert_eq!(error.kind(), ErrorKind::NotFound);
}

#[test]
fn run_refuses_an_empty_command() {
    let error = run(&[], &[]).expect_err("nothing starts");
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
}

#[test]
fn run_adds_the_variables_it_is_given_to_the_jobs_environment() {
    let variables = [("DEADBAND_TEST_VARIABLE", "7".to_owned())];
    let status = run(
        &words(&["sh", "-c", "test \"$DEADBAND_TEST_VARIABLE\" = 7"]),
        &variables,
    )
    .expect("sh starts");
    assert_eq!(exit_code(status), 0);
}
