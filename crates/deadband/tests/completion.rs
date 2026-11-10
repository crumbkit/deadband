//! End-to-end tests of the `deadband completion` commands and of the
//! completion request that a shell makes. Each test runs the built binary
//! with `HOME` and the XDG directories set to a temporary directory of its
//! own, so no test writes into the real home directory.

// clippy allows `expect` only in code under `cfg(test)`, and an integration
// test's helper functions are not under it without this attribute.
#![cfg(test)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// An empty temporary home. Dropping it removes the directory.
struct Home(PathBuf);

impl Home {
    /// Creates a home that only `name` uses.
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("db-compl-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the home is writable");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Runs the built `deadband` binary with `args`, a cleared environment,
/// `HOME` set to `home` and the XDG directories under it. `extra` adds
/// environment variables to the run.
fn deadband(home: &Home, args: &[&str], extra: &[(&str, &str)]) -> Output {
    #[expect(
        clippy::disallowed_methods,
        reason = "the test runs the built binary, as a person would"
    )]
    let mut command = Command::new(env!("CARGO_BIN_EXE_deadband"));
    command
        .args(args)
        .current_dir(home.path())
        .env_clear()
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("DEADBAND_SOCKET", home.path().join("db.sock"));
    for (name, value) in extra {
        command.env(name, value);
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "the test runs the built binary, as a person would"
    )]
    let output = command.output();
    output.expect("the binary starts")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn completion_install_writes_fish_completion_naming_deadband_under_the_config_directory() {
    let home = Home::new("install");

    let output = deadband(
        &home,
        &["completion", "install", "--shell", "fish", "--json"],
        &[],
    );

    assert!(output.status.success(), "{}", stdout(&output));
    assert!(
        stdout(&output).contains("\"written\""),
        "{}",
        stdout(&output)
    );
    let script = fs::read_to_string(home.path().join("config/fish/completions/deadband.fish"))
        .expect("install wrote the file");
    assert!(script.contains("deadband"), "{script}");
}

#[test]
fn completion_check_exits_with_1_and_names_a_missing_file() {
    let home = Home::new("missing");

    let output = deadband(&home, &["completion", "check", "--shell", "fish"], &[]);

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("missing"), "{}", stdout(&output));
}

#[test]
fn completion_check_exits_with_0_after_install() {
    let home = Home::new("current");
    assert!(
        deadband(&home, &["completion", "install", "--shell", "fish"], &[])
            .status
            .success()
    );

    let output = deadband(&home, &["completion", "check", "--shell", "fish"], &[]);

    assert!(output.status.success());
    assert!(output.stdout.is_empty(), "{}", stdout(&output));
}

#[test]
fn completion_print_writes_a_fish_registration_that_runs_deadband() {
    let home = Home::new("print");

    let output = deadband(&home, &["completion", "print", "--shell", "fish"], &[]);

    assert!(output.status.success());
    let script = stdout(&output);
    assert!(script.contains("COMPLETE=fish"), "{script}");
    assert!(script.contains("deadband"), "{script}");
    assert!(!home.path().join("config/fish").exists());
}

#[test]
fn completion_install_reads_the_shell_from_the_shell_variable() {
    let home = Home::new("shell");

    let output = deadband(
        &home,
        &["completion", "install"],
        &[("SHELL", "/usr/bin/fish")],
    );

    assert!(output.status.success(), "{}", stdout(&output));
    assert!(
        home.path()
            .join("config/fish/completions/deadband.fish")
            .exists()
    );
}

#[test]
fn the_complete_variable_makes_deadband_print_a_fish_registration() {
    let home = Home::new("request");

    let output = deadband(&home, &[], &[("COMPLETE", "fish")]);

    assert!(output.status.success());
    let script = stdout(&output);
    assert!(script.contains("complete"), "{script}");
    assert!(script.contains("COMPLETE=fish"), "{script}");
}
