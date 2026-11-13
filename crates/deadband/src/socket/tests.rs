//! Tests of the socket's path and of the catalog of environment variables.

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{VARIABLES, path, path_in_user_directory};

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
fn the_per_user_directory_beats_the_callers_tmpdir_for_the_default_socket() {
    let socket = path_in_user_directory(
        environment(&[("TMPDIR", "/private/other/"), ("USER", "me")]),
        || Some(PathBuf::from("/var/folders/ab/T/")),
    );
    assert_eq!(socket, Path::new("/var/folders/ab/T/deadband.sock"));
}

#[test]
fn deadband_socket_beats_the_per_user_directory() {
    let socket = path_in_user_directory(
        environment(&[("DEADBAND_SOCKET", "/run/me/db.sock")]),
        || Some(PathBuf::from("/var/folders/ab/T/")),
    );
    assert_eq!(socket, Path::new("/run/me/db.sock"));
}

#[test]
fn the_callers_tmpdir_names_the_socket_when_no_per_user_directory_is_known() {
    let socket = path_in_user_directory(environment(&[("TMPDIR", "/tmp/me")]), || None);
    assert_eq!(socket, Path::new("/tmp/me/deadband.sock"));
}

#[test]
fn deadband_socket_dir_beats_the_per_user_directory() {
    let socket = path_in_user_directory(
        environment(&[("DEADBAND_SOCKET_DIR", "/run/me"), ("TMPDIR", "/tmp/me")]),
        || Some(PathBuf::from("/var/folders/ab/T/")),
    );
    assert_eq!(socket, Path::new("/run/me/deadband.sock"));
}

#[test]
fn deadband_socket_beats_deadband_socket_dir() {
    let socket = path_in_user_directory(
        environment(&[
            ("DEADBAND_SOCKET", "/run/me/db.sock"),
            ("DEADBAND_SOCKET_DIR", "/run/other"),
        ]),
        || None,
    );
    assert_eq!(socket, Path::new("/run/me/db.sock"));
}

#[test]
fn the_per_user_directory_is_not_looked_up_when_an_override_is_set() {
    let asked = std::cell::Cell::new(false);
    let lookup = || {
        asked.set(true);
        None
    };
    path_in_user_directory(environment(&[("DEADBAND_SOCKET_DIR", "/run/me")]), lookup);
    assert!(!asked.get());
}

#[test]
fn the_per_user_directory_is_not_looked_up_when_deadband_socket_is_set() {
    let asked = std::cell::Cell::new(false);
    let lookup = || {
        asked.set(true);
        None
    };
    path_in_user_directory(
        environment(&[("DEADBAND_SOCKET", "/run/me/db.sock")]),
        lookup,
    );
    assert!(!asked.get());
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
