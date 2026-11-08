//! Tests of reading the environment snapshot and of the variables a granted
//! job receives.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{Environment, VARIABLES};

/// Returns a reader of the environment that holds only `pairs`.
fn environment(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let pairs: Vec<(String, OsString)> = pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
        .collect();
    move |name| {
        pairs
            .iter()
            .find(|(each, _)| each == name)
            .map(|(_, value)| value.clone())
    }
}

#[test]
fn the_catalog_lists_every_variable_that_read_reads_in_order() {
    let read = RefCell::new(Vec::new());
    Environment::read(
        |name: &str| {
            read.borrow_mut().push(name.to_owned());
            None
        },
        None,
    );
    let catalog: Vec<String> = VARIABLES.iter().map(|each| each.name.to_owned()).collect();
    assert_eq!(read.into_inner(), catalog);
}

#[test]
fn a_job_inside_a_granted_job_is_held_and_one_that_asks_to_bypass_bypasses() {
    let inside = Environment::read(environment(&[("DEADBAND_HELD", "1")]), None);
    assert!(inside.held);
    assert!(!inside.bypass);
    let bypass = Environment::read(environment(&[("DEADBAND_BYPASS", "1")]), None);
    assert!(bypass.bypass);
    assert!(!bypass.held);
}

#[test]
fn an_empty_or_zero_variable_neither_holds_nor_bypasses() {
    let empty = Environment::read(
        environment(&[("DEADBAND_HELD", ""), ("DEADBAND_BYPASS", "0")]),
        None,
    );
    assert!(!empty.held);
    assert!(!empty.bypass);
}

#[test]
fn the_priority_file_is_in_the_xdg_config_directory_when_it_is_set() {
    let read = Environment::read(
        environment(&[("HOME", "/home/me"), ("XDG_CONFIG_HOME", "/config")]),
        None,
    );
    assert_eq!(
        read.priority_file(),
        Some(PathBuf::from("/config/deadband/priority"))
    );
}

#[test]
fn the_priority_file_is_in_the_home_config_directory_otherwise() {
    let read = Environment::read(environment(&[("HOME", "/home/me")]), None);
    assert_eq!(
        read.priority_file(),
        Some(PathBuf::from("/home/me/.config/deadband/priority"))
    );
    assert_eq!(read.home.as_deref(), Some(Path::new("/home/me")));
    assert_eq!(Environment::read(environment(&[]), None).priority_file(), None);
}

#[test]
fn a_granted_job_gets_its_parallelism_in_every_variable_and_is_marked_held() {
    let read = Environment::read(environment(&[]), None);
    assert_eq!(
        read.job_variables(3),
        [
            ("DEADBAND_HELD", "1".to_owned()),
            ("CARGO_BUILD_JOBS", "3".to_owned()),
            ("NEXTEST_TEST_THREADS", "3".to_owned()),
            ("RUST_TEST_THREADS", "3".to_owned()),
            ("CMAKE_BUILD_PARALLEL_LEVEL", "3".to_owned()),
            ("MAKEFLAGS", "-j3".to_owned()),
        ]
    );
}

#[test]
fn a_granted_job_keeps_each_parallelism_variable_that_its_caller_set() {
    let read = Environment::read(
        environment(&[("CARGO_BUILD_JOBS", "1"), ("MAKEFLAGS", "-s")]),
        None,
    );
    assert_eq!(
        read.job_variables(3),
        [
            ("DEADBAND_HELD", "1".to_owned()),
            ("NEXTEST_TEST_THREADS", "3".to_owned()),
            ("RUST_TEST_THREADS", "3".to_owned()),
            ("CMAKE_BUILD_PARALLEL_LEVEL", "3".to_owned()),
        ]
    );
}
