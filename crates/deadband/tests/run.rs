//! Tests of `deadband run` and `deadband daemon` through the built binary,
//! which are the plan's acceptance checks.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Returns a socket path in the temporary directory that only `name` uses.
fn socket(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("db-it-{}-{name}.sock", std::process::id()))
}

/// Returns a `deadband` command whose daemon socket is `socket`.
fn deadband(socket: &Path, args: &[&str]) -> Command {
    #[expect(
        clippy::disallowed_methods,
        reason = "the test runs the built binary, as a person would"
    )]
    let mut command = Command::new(env!("CARGO_BIN_EXE_deadband"));
    command.args(args).env("DEADBAND_SOCKET", socket);
    command
}

#[test]
fn run_without_a_daemon_exits_with_the_command_and_says_in_one_line_that_the_daemon_is_not_running()
{
    let mut run = deadband(&socket("none"), &["run", "--", "true"]);
    #[expect(
        clippy::disallowed_methods,
        reason = "the test runs the built binary, as a person would"
    )]
    let output = run.output().expect("deadband runs");
    assert!(output.status.success(), "{output:?}");
    let err = String::from_utf8(output.stderr).expect("UTF-8");
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.contains("the daemon is not running"), "{err}");
}

/// The daemon holds jobs back only on macOS, where the probe reads memory
/// pressure. Elsewhere every sample fails, so every job starts at once.
#[cfg(target_os = "macos")]
#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the test runs the built binary as a person would, and stops the daemon it started"
)]
mod macos {
    use std::io::{BufRead, BufReader, Read};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use super::{deadband, socket};

    /// Reads one line from `stream`.
    fn line(stream: impl Read) -> std::io::Result<String> {
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        Ok(line)
    }

    #[test]
    fn with_one_job_at_a_time_a_second_run_waits_for_the_first_and_status_shows_both() {
        let socket = socket("two");
        let mut daemon = deadband(&socket, &["daemon", "--max-jobs", "1"])
            .stderr(Stdio::piped())
            .spawn()
            .expect("the daemon starts");
        let daemon_err = daemon.stderr.take().expect("piped");
        assert!(line(daemon_err).expect("reads").contains("listens on"));

        let started = Instant::now();
        let script = "echo started; sleep 2";
        let mut first = deadband(&socket, &["run", "--", "sh", "-c", script])
            .stdout(Stdio::piped())
            .spawn()
            .expect("the first run starts");
        let first_out = first.stdout.take().expect("piped");
        assert_eq!(line(first_out).expect("reads"), "started\n");
        let mut second = deadband(&socket, &["run", "--", "sleep", "2"])
            .stderr(Stdio::piped())
            .spawn()
            .expect("the second run starts");
        let second_err = second.stderr.take().expect("piped");
        let waits = line(second_err).expect("reads");
        assert!(waits.contains("the job waits for headroom"), "{waits}");

        let status = deadband(&socket, &["status"])
            .output()
            .expect("status runs");
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
}
