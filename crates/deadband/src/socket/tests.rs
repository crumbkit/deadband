//! Tests of the socket's path and of the catalog of environment variables.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::Path;

use super::{VARIABLES, path};

/// Returns a reader of an environment that holds only `vars`.
fn environment(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
    let vars: Vec<(String, OsString)> = vars
        .iter()
        .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
        .collect();
    move |name| {
        vars.iter()
            .find(|(each, _)| each == name)
            .map(|(_, value)| value.clone())
    }
}

#[test]
fn deadband_socket_names_the_socket_when_it_is_set() {
    let socket = path(environment(&[
        ("DEADBAND_SOCKET", "/run/me/db.sock"),
        ("TMPDIR", "/var/folders/ab/T/"),
    ]));
    assert_eq!(socket, Path::new("/run/me/db.sock"));
}

#[test]
fn the_socket_is_in_the_temporary_directory_when_deadband_socket_is_not_set() {
    let socket = path(environment(&[
        ("TMPDIR", "/var/folders/ab/T/"),
        ("USER", "me"),
    ]));
    assert_eq!(socket, Path::new("/var/folders/ab/T/deadband.sock"));
}

#[test]
fn an_empty_variable_counts_as_absent() {
    let socket = path(environment(&[
        ("DEADBAND_SOCKET", ""),
        ("TMPDIR", "/tmp/me"),
    ]));
    assert_eq!(socket, Path::new("/tmp/me/deadband.sock"));
}

#[test]
fn the_socket_is_in_tmp_with_the_user_name_when_no_directory_is_set() {
    assert_eq!(
        path(environment(&[("USER", "me")])),
        Path::new("/tmp/deadband-me.sock")
    );
    assert_eq!(path(environment(&[])), Path::new("/tmp/deadband.sock"));
}

#[test]
fn the_catalog_lists_every_variable_that_path_reads_in_order() {
    let read = RefCell::new(Vec::new());
    path(|name: &str| {
        read.borrow_mut().push(name.to_owned());
        None
    });
    let catalog: Vec<String> = VARIABLES.iter().map(|each| each.name.to_owned()).collect();
    assert_eq!(read.into_inner(), catalog);
}
