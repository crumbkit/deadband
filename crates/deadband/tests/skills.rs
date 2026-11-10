//! End-to-end tests of the `deadband skills` commands. Each test runs the
//! built binary with `HOME` set to a temporary directory of its own, so no
//! test installs into the real home directory.

// clippy allows `expect` only in code under `cfg(test)`, and an integration
// test's helper functions are not under it without this attribute.
#![cfg(test)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A temporary home with an empty `.agents/skills` directory. Dropping it
/// removes the directory.
struct Home(PathBuf);

impl Home {
    /// Creates a home that only `name` uses.
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("db-skills-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join(".agents").join("skills")).expect("the home is writable");
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

/// Runs the built `deadband` binary with `args`, a cleared environment and
/// `HOME` set to `home`, and returns its output.
fn deadband(home: &Home, args: &[&str]) -> Output {
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
fn skills_install_writes_the_skill_with_metadata_naming_deadband() {
    let home = Home::new("install");

    let output = deadband(&home, &["skills", "install", "--json"]);

    assert!(output.status.success(), "{}", stdout(&output));
    assert!(
        stdout(&output).contains("\"written\""),
        "{}",
        stdout(&output)
    );
    let text = fs::read_to_string(home.path().join(".agents/skills/deadband/SKILL.md"))
        .expect("install wrote the skill");
    assert!(text.starts_with("---\nname: deadband\n"), "{text}");
    assert!(text.contains("metadata:\n  tool: \"deadband\"\n"), "{text}");
}

#[test]
fn skills_check_exits_with_1_and_names_a_missing_copy() {
    let home = Home::new("missing");

    let output = deadband(&home, &["skills", "check"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).contains("missing"), "{}", stdout(&output));
}

#[test]
fn skills_check_exits_with_0_and_prints_no_row_after_install() {
    let home = Home::new("current");
    assert!(deadband(&home, &["skills", "install"]).status.success());

    let output = deadband(&home, &["skills", "check"]);

    assert!(output.status.success());
    assert!(output.stdout.is_empty(), "{}", stdout(&output));
}

#[test]
fn skills_print_prints_the_skill_that_tells_agents_to_use_deadband_run() {
    let home = Home::new("print");

    let output = deadband(&home, &["skills", "print"]);

    assert!(output.status.success());
    assert!(
        stdout(&output).contains("deadband run --label"),
        "{}",
        stdout(&output)
    );
}
