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

/// The `type` that `starmap.toml` gives each variable, as the TOML text of
/// the value, and what the code does with the variable that the type fits.
/// `DEADBAND_HELD` and `DEADBAND_BYPASS` are `string` because the code reads
/// any value but empty or 0 as on, and starmap's `boolean` refuses other
/// words. `HOME` is an absolute path because `deadband service` takes the
/// plist's path from it and crumbkit counts only an absolute value.
const TYPES: [(&str, &str); 14] = [
    ("DEADBAND_SOCKET", "\"path\""),
    ("DEADBAND_SOCKET_DIR", "\"path\""),
    ("TMPDIR", "\"path\""),
    ("XDG_RUNTIME_DIR", "\"path\""),
    ("DEADBAND_HELD", "\"string\""),
    ("DEADBAND_BYPASS", "\"string\""),
    ("HOME", "{ path = { absolute = true } }"),
    ("XDG_CONFIG_HOME", "\"path\""),
    ("XDG_STATE_HOME", "\"path\""),
    ("CARGO_BUILD_JOBS", "\"string\""),
    ("NEXTEST_TEST_THREADS", "\"string\""),
    ("RUST_TEST_THREADS", "\"string\""),
    ("CMAKE_BUILD_PARALLEL_LEVEL", "\"string\""),
    ("MAKEFLAGS", "\"string\""),
];

/// Returns the string that `table` holds under `key`, or an empty string.
fn text<'a>(table: &'a toml::Table, key: &str) -> &'a str {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .unwrap_or_default()
}

/// Returns the table that `starmap.toml` holds for the variable `name`.
fn variable(name: &str) -> toml::Table {
    match variables().remove(name) {
        Some(toml::Value::Table(table)) => table,
        other => panic!("starmap.toml has no table for {name}: {other:?}"),
    }
}

/// Returns the lines of AGENTS.md, which lists the commands of the
/// pull-request lane.
fn lane() -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../AGENTS.md");
    let text = std::fs::read_to_string(path).expect("AGENTS.md is at the repository root");
    text.lines().map(str::to_owned).collect()
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
fn starmap_toml_gives_every_variable_exactly_the_keys_that_deadband_uses() {
    let expected: BTreeSet<&str> = ["purpose", "effect", "type", "unset", "show", "source"]
        .into_iter()
        .collect();
    for (name, variable) in variables() {
        let table = variable.as_table().expect("a variable is a table");
        let keys: BTreeSet<&str> = table.keys().map(String::as_str).collect();
        assert_eq!(keys, expected, "the keys of {name}");
    }
}

#[test]
fn starmap_toml_gives_every_variable_a_purpose_and_an_effect() {
    for entry in socket::VARIABLES.iter().chain(environment::VARIABLES) {
        let table = variable(entry.name);
        assert!(
            !text(&table, "purpose").is_empty(),
            "{} has no purpose",
            entry.name
        );
        assert!(
            !text(&table, "effect").is_empty(),
            "{} has no effect",
            entry.name
        );
    }
}

#[test]
fn starmap_toml_effect_names_the_fallback_that_the_code_documents_for_the_socket_variables() {
    for (name, fallback) in [
        ("DEADBAND_SOCKET", "DEADBAND_SOCKET_DIR"),
        ("DEADBAND_SOCKET_DIR", "TMPDIR"),
        ("TMPDIR", "XDG_RUNTIME_DIR"),
        ("XDG_RUNTIME_DIR", "/tmp/deadband-<uid>"),
    ] {
        let effect = text(&variable(name), "effect").to_owned();
        assert!(effect.contains(fallback), "{name}: {effect}");
        let catalog = socket::VARIABLES
            .iter()
            .find(|entry| entry.name == name)
            .expect("the socket catalog lists the variable");
        assert!(
            catalog.absent.contains(fallback),
            "the catalog's absent text for {name} names {fallback}"
        );
    }
}

#[test]
fn starmap_toml_gives_every_variable_the_type_that_matches_how_the_code_reads_it() {
    for (name, expected) in TYPES {
        let wanted: toml::Table = format!("type = {expected}").parse().expect("valid TOML");
        let declared = variable(name);
        assert_eq!(
            declared.get("type"),
            wanted.get("type"),
            "the type of {name}"
        );
    }
}

#[test]
fn starmap_toml_marks_every_variable_optional_inherited_and_shown() {
    for (name, _) in variables() {
        let table = variable(&name);
        assert_eq!(text(&table, "unset"), "optional", "unset of {name}");
        assert_eq!(text(&table, "source"), "inherited", "source of {name}");
        assert_eq!(text(&table, "show"), "shown", "show of {name}");
    }
}

#[test]
fn starmap_toml_effect_of_the_switches_says_that_any_other_value_turns_them_on() {
    for name in ["DEADBAND_HELD", "DEADBAND_BYPASS"] {
        let effect = text(&variable(name), "effect").to_owned();
        assert!(effect.contains("Any other value"), "{name}: {effect}");
    }
}

#[test]
fn starmap_toml_says_that_the_service_commands_read_home() {
    let purpose = text(&variable("HOME"), "purpose").to_owned();
    assert!(purpose.contains("deadband service"), "{purpose}");
}

#[test]
fn starmap_toml_says_that_only_the_daemon_makes_the_fallback_directory() {
    let effect = text(&variable("XDG_RUNTIME_DIR"), "effect").to_owned();
    assert!(effect.contains("the daemon makes"), "{effect}");
}

#[test]
fn mxv_toml_declares_only_the_tables_that_deadband_uses() {
    let file = declaration("mxv.toml");
    let tables: BTreeSet<&str> = file.keys().map(String::as_str).collect();
    assert_eq!(tables, BTreeSet::from(["processes", "provides", "tasks"]));
    let process = daemon();
    let keys: BTreeSet<&str> = process.keys().map(String::as_str).collect();
    assert_eq!(keys, BTreeSet::from(["argv", "build", "env", "ready"]));
}

#[test]
fn mxv_toml_provides_the_daemon_socket_at_the_path_that_the_daemon_gets() {
    let mut file = declaration("mxv.toml");
    let Some(toml::Value::Table(mut provides)) = file.remove("provides") else {
        panic!("mxv.toml declares no [provides] table");
    };
    let Some(toml::Value::Table(provide)) = provides.remove("deadband-socket") else {
        panic!("mxv.toml provides no deadband-socket");
    };
    let keys: BTreeSet<&str> = provide.keys().map(String::as_str).collect();
    assert_eq!(keys, BTreeSet::from(["kind", "path", "unit"]));
    assert_eq!(provide["kind"].as_str(), Some("socket"));
    assert_eq!(provide["unit"].as_str(), Some("deadband"));
    let daemon = daemon();
    let env = daemon["env"].as_table().expect("the daemon sets env");
    assert_eq!(provide["path"], env["DEADBAND_SOCKET"]);
}

#[test]
fn mxv_toml_starts_the_binary_that_cargo_build_writes_with_the_daemon_command() {
    assert_eq!(
        strings(&daemon(), "argv"),
        ["target/debug/deadband", "daemon"]
    );
}

#[test]
fn mxv_toml_builds_only_the_deadband_binary_from_the_lockfile() {
    assert_eq!(
        strings(&daemon(), "build"),
        ["cargo", "build", "--locked", "--bin", "deadband"]
    );
}

#[test]
fn mxv_toml_gives_the_daemon_a_socket_of_its_own_in_the_checkout_and_nothing_else() {
    let process = daemon();
    let env = process
        .get("env")
        .and_then(toml::Value::as_table)
        .expect("the process sets env");
    let socket = text(env, "DEADBAND_SOCKET");
    assert_eq!(socket, "target/deadband.sock");
    assert_eq!(env.len(), 1, "no other variable: {env:?}");
    assert!(socket.len() < 100, "the path fits in a socket address");
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
fn mxv_toml_is_ready_when_the_daemon_logs_that_it_listens_within_thirty_seconds() {
    let process = daemon();
    let ready = process
        .get("ready")
        .and_then(toml::Value::as_table)
        .expect("the process has a ready table");
    assert_eq!(text(ready, "log"), "the daemon listens on");
    assert_eq!(
        ready
            .get("timeout_seconds")
            .and_then(toml::Value::as_integer),
        Some(30)
    );
    assert_eq!(ready.len(), 2, "no other key: {ready:?}");
}

#[test]
fn mxv_toml_tasks_are_the_commands_of_the_pull_request_lane() {
    let Some(toml::Value::Table(tasks)) = declaration("mxv.toml").remove("tasks") else {
        panic!("mxv.toml declares no [tasks] table");
    };
    let names: BTreeSet<&str> = tasks.keys().map(String::as_str).collect();
    assert_eq!(names, BTreeSet::from(["clippy", "doctest", "fmt", "test"]));
    let lane = lane();
    for (name, task) in &tasks {
        let task = task.as_table().expect("a task is a table");
        assert_eq!(task.len(), 1, "{name} sets only argv");
        let command = strings(task, "argv").join(" ");
        assert!(
            lane.iter().any(|line| line.trim() == command),
            "AGENTS.md lists no lane command `{command}` for the task {name}"
        );
    }
}
