//! The checks that `starmap.toml` and `mxv.toml` at the repository root say
//! what deadband does: `starmap.toml` declares the environment variables that
//! deadband reads, and `mxv.toml` declares the daemon as the process
//! `deadband`.

// clippy allows `expect` and `panic` only in code under `cfg(test)`, and an
// integration test's helper functions are not under it without this attribute.
#![cfg(test)]

use std::collections::BTreeSet;
use std::path::Path;

use deadband::{environment, socket};

/// Returns the table that the file `name` at the repository root holds.
fn declaration(name: &str) -> toml::Table {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(name);
    let text = std::fs::read_to_string(&path).expect("the file is at the repository root");
    text.parse().expect("the file is valid TOML")
}

/// Returns the `[variables.*]` tables of `starmap.toml`.
fn variables() -> toml::Table {
    match declaration("starmap.toml").remove("variables") {
        Some(toml::Value::Table(variables)) => variables,
        other => panic!("starmap.toml has no [variables] table: {other:?}"),
    }
}

/// Returns the process `deadband` of `mxv.toml`.
fn daemon() -> toml::Table {
    let mut file = declaration("mxv.toml");
    let Some(toml::Value::Table(mut processes)) = file.remove("processes") else {
        panic!("mxv.toml declares no [processes] table");
    };
    match processes.remove("deadband") {
        Some(toml::Value::Table(process)) => process,
        other => panic!("mxv.toml declares no process named deadband: {other:?}"),
    }
}

/// Returns the strings of the array that `table` holds under `key`.
fn strings(table: &toml::Table, key: &str) -> Vec<String> {
    table
        .get(key)
        .and_then(toml::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn starmap_toml_declares_exactly_the_variables_that_deadband_reads() {
    let declared: BTreeSet<String> = variables().keys().cloned().collect();
    let read: BTreeSet<String> = socket::VARIABLES
        .iter()
        .chain(environment::VARIABLES)
        .map(|variable| variable.name.to_owned())
        .collect();
    assert_eq!(declared, read);
}

#[test]
fn starmap_toml_gives_every_variable_the_keys_that_starmap_requires() {
    const KEYS: [&str; 6] = ["purpose", "effect", "type", "unset", "show", "source"];
    for (name, variable) in variables() {
        let table = variable.as_table().expect("a variable is a table");
        for key in KEYS {
            assert!(table.contains_key(key), "{name} has no `{key}`");
        }
    }
}

#[test]
fn starmap_toml_declares_no_secret_because_deadband_reads_none() {
    let withheld: Vec<String> = variables()
        .into_iter()
        .filter(|(_, variable)| variable.get("show").and_then(toml::Value::as_str) != Some("shown"))
        .map(|(name, _)| name)
        .collect();
    assert!(withheld.is_empty(), "withheld variables: {withheld:?}");
}

#[test]
fn starmap_toml_describes_each_variable_in_the_words_of_the_code() {
    let variables = variables();
    for variable in socket::VARIABLES.iter().chain(environment::VARIABLES) {
        let purpose = variables
            .get(variable.name)
            .and_then(|declared| declared.get("purpose"))
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        assert!(!purpose.is_empty(), "{} has no purpose", variable.name);
    }
}

#[test]
fn mxv_toml_starts_the_daemon_with_the_daemon_command() {
    let argv = strings(&daemon(), "argv");
    assert_eq!(argv.last().map(String::as_str), Some("daemon"));
    assert!(
        argv.first()
            .is_some_and(|program| program.ends_with("deadband")),
        "the program is deadband: {argv:?}"
    );
}

#[test]
fn mxv_toml_builds_the_daemon_that_it_starts() {
    let process = daemon();
    let build = strings(&process, "build");
    assert_eq!(build.first().map(String::as_str), Some("cargo"));
    let program = strings(&process, "argv").remove(0);
    assert!(
        program.starts_with("target/"),
        "mxv starts the binary that the build wrote: {program}"
    );
}

#[test]
fn mxv_toml_gives_the_daemon_a_socket_of_its_own_in_the_checkout() {
    let process = daemon();
    let socket = process
        .get("env")
        .and_then(|env| env.get("DEADBAND_SOCKET"))
        .and_then(toml::Value::as_str)
        .expect("the process sets DEADBAND_SOCKET");
    assert!(
        Path::new(socket).is_relative() && socket.starts_with("target/"),
        "the socket is a relative path in target/, which git ignores: {socket}"
    );
}

#[test]
fn mxv_toml_declares_no_port_because_the_daemon_listens_on_a_unix_socket() {
    let file = declaration("mxv.toml");
    assert!(!file.contains_key("ports"));
    assert!(!file.contains_key("routes"));
    assert!(!file.contains_key("wants"));
    let process = daemon();
    assert!(!process.contains_key("ports"));
    assert!(!process.contains_key("listen"));
}

#[test]
fn mxv_toml_is_ready_when_the_daemon_logs_that_it_listens() {
    let process = daemon();
    let log = process
        .get("ready")
        .and_then(|ready| ready.get("log"))
        .and_then(toml::Value::as_str)
        .expect("the process waits for a log line");
    assert_eq!(log, "the daemon listens on");
}

#[test]
fn mxv_toml_tasks_run_the_commands_of_the_pull_request_lane() {
    let Some(toml::Value::Table(tasks)) = declaration("mxv.toml").remove("tasks") else {
        panic!("mxv.toml declares no [tasks] table");
    };
    for (name, subcommand) in [("fmt", "fmt"), ("clippy", "clippy"), ("test", "nextest")] {
        let task = tasks
            .get(name)
            .and_then(toml::Value::as_table)
            .unwrap_or_else(|| panic!("no task named {name}"));
        let argv = strings(task, "argv");
        assert_eq!(
            argv.get(..2),
            Some(&["cargo".to_owned(), subcommand.to_owned()][..])
        );
    }
}
