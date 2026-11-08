//! Tests of `deadband run` and `deadband daemon` through the built binary,
//! which are the plan's acceptance checks.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Returns a socket path in the temporary directory that only `name` uses.
fn socket(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("db-it-{}-{name}.sock", std::process::id()))
}

/// Returns a `deadband` command whose daemon socket is `socket`.
fn deadband(socket: &PathBuf, args: &[&str]) -> Command {
    #[expect(
        clippy::disallowed_methods,
        reason = "the test runs the built binary, as a person would"
    )]
    let mut command = Command::new(env!("CARGO_BIN_EXE_deadband"));
    command.args(args).env("DEADBAND_SOCKET", socket);
    command
}

/// Starts `command` with piped output.
fn spawn(mut command: Command) -> Child {
    #[expect(
        clippy::disallowed_methods,
        reason = "the test runs the built binary, as a person would"
    )]
    let child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("deadband starts");
    child
}

/// Runs `command` to its end and returns its output.
fn output(mut command: Command) -> Output {
    #[expect(
        clippy::disallowed_methods,
        reason = "the test runs the built binary, as a person would"
    )]
    let output = command.output().expect("deadband runs");
    output
}

/// Reads one line from `stream`.
fn line(stream: &mut impl Read) -> String {
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .expect("the line reads");
    line
}

#[test]
fn run_without_a_daemon_exits_with_the_command_and_says_in_one_line_that_the_daemon_is_not_running()
{
    let output = output(deadband(&socket("none"), &["run", "--", "true"]));
    assert!(output.status.success(), "{output:?}");
    let err = String::from_utf8(output.stderr).expect("UTF-8");
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.contains("the daemon is not running"), "{err}");
}

/// The daemon admits jobs only on macOS, where the probe reads memory
/// pressure. Elsewhere every sample fails, so every job starts at once.
#[cfg(target_os = "macos")]
#[test]
fn with_one_job_at_a_time_a_second_run_waits_for_the_first_and_status_shows_both() {
    let socket = socket("two");
    let mut daemon = spawn(deadband(&socket, &["daemon", "--max-jobs", "1"]));
    let mut daemon_err = daemon.stderr.take().expect("piped");
    assert!(line(&mut daemon_err).contains("listens on"));

    let started = Instant::now();
    let script = "echo started; sleep 2";
    let mut first = spawn(deadband(&socket, &["run", "--", "sh", "-c", script]));
    let mut first_out = first.stdout.take().expect("piped");
    assert_eq!(line(&mut first_out), "started\n");
    let mut second = spawn(deadband(&socket, &["run", "--", "sleep", "2"]));
    let mut second_err = second.stderr.take().expect("piped");
    assert!(line(&mut second_err).contains("the job waits for headroom"));

    let status = output(deadband(&socket, &["status"]));
    let status = String::from_utf8(status.stdout).expect("UTF-8");
    assert!(
        status.contains("daemon: 1 running, 1 waiting, at most 1 at once"),
        "{status}"
    );

    assert!(first.wait().expect("the first run ends").success());
    assert!(second.wait().expect("the second run ends").success());
    assert!(started.elapsed() >= Duration::from_secs(4));
    daemon.kill().expect("the test stops its daemon");
    daemon.wait().expect("the daemon is reaped");
}
