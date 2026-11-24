//! Tests of a `--label` that holds a control character, through the built
//! binary and a daemon that the test starts on a socket of its own.
#![cfg(test)]
#![expect(
    clippy::disallowed_methods,
    reason = "the test runs the built binary as a person would, and stops the daemon it started"
)]
#![expect(
    clippy::disallowed_types,
    reason = "the test holds the daemon it started, to stop it when it ends"
)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// The variables that the environment of the test's caller may set and
/// that change what `deadband run` does.
const CALLER_VARIABLES: [&str; 2] = ["DEADBAND_HELD", "DEADBAND_BYPASS"];

/// A daemon that the test started on a socket of its own. Dropping it stops
/// the daemon and removes the socket, its lock file and the configuration
/// directory.
struct Daemon {
    /// The daemon's process.
    child: Child,
    /// The daemon's socket.
    socket: PathBuf,
}

impl Daemon {
    /// Starts a daemon on a socket that only `name` uses, and waits until
    /// it listens.
    fn start(name: &str) -> Self {
        let socket = std::env::temp_dir().join(format!("db-lb-{}-{name}.sock", std::process::id()));
        let mut child = deadband(&socket, &["daemon", "--max-jobs", "1"])
            .stderr(Stdio::piped())
            .spawn()
            .expect("the daemon starts");
        let mut first = String::new();
        BufReader::new(child.stderr.take().expect("piped"))
            .read_line(&mut first)
            .expect("the log reads");
        assert!(first.contains("listens on"), "{first}");
        Self { child, socket }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_file(deadband::daemon::lock_path(&self.socket));
    }
}

/// Returns a configuration directory that no test makes, so a command of
/// the tests never reads the person's `config.toml`.
fn config_home() -> PathBuf {
    std::env::temp_dir().join(format!("db-lb-{}-config", std::process::id()))
}

/// Returns a `deadband` command whose daemon socket is `socket`, without
/// the variables of [`CALLER_VARIABLES`].
fn deadband(socket: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_deadband"));
    command
        .args(args)
        .env("DEADBAND_SOCKET", socket)
        .env("XDG_CONFIG_HOME", config_home());
    for name in CALLER_VARIABLES {
        command.env_remove(name);
    }
    command
}

#[test]
fn a_run_whose_label_holds_a_line_feed_is_admitted_and_status_lists_it_as_one_row() {
    let daemon = Daemon::start("newline");
    let mut run = deadband(
        &daemon.socket,
        &[
            "run",
            "--label",
            "a\nb",
            "--",
            "sh",
            "-c",
            "echo started; cat",
        ],
    )
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("the run starts");
    let mut started = String::new();
    BufReader::new(run.stdout.take().expect("piped"))
        .read_line(&mut started)
        .expect("the job's output reads");
    assert_eq!(started, "started\n");

    let status = deadband(&daemon.socket, &["status"])
        .output()
        .expect("status runs");
    let status = String::from_utf8(status.stdout).expect("UTF-8");
    drop(run.stdin.take().expect("piped"));
    let ended = run.wait_with_output().expect("the run ends");
    assert!(ended.status.success(), "{ended:?}");
    // A run that the daemon admits says nothing; one that starts without
    // asking says why in one line.
    assert_eq!(String::from_utf8_lossy(&ended.stderr), "");

    // `status` lists the jobs only where it can read the machine, which is
    // macOS. Elsewhere it prints no rows, and the test checks the admission
    // above alone.
    if !cfg!(target_os = "macos") {
        return;
    }
    let running: Vec<&str> = status
        .lines()
        .filter(|line| line.starts_with("running: "))
        .collect();
    assert_eq!(running.len(), 1, "{status}");
    let row = running.first().expect("one row");
    assert!(row.starts_with("running: a b (pid "), "{status}");
    assert!(
        status.contains("daemon: 1 running, 0 waiting, at most 1 at once"),
        "{status}"
    );
    assert!(
        !status
            .lines()
            .any(|line| line == "b" || line.starts_with("b ")),
        "{status}"
    );
}
