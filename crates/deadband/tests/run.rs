//! Tests of `deadband run` and `deadband daemon` through the built binary,
//! which are the plan's acceptance checks.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A socket path in the temporary directory. Dropping it removes the
/// socket and its lock file.
struct Socket(PathBuf);

impl Drop for Socket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(deadband::daemon::lock_path(&self.0));
    }
}

/// Returns a socket path in the temporary directory that only `name` uses.
fn socket(name: &str) -> Socket {
    Socket(std::env::temp_dir().join(format!("db-it-{}-{name}.sock", std::process::id())))
}

/// The variables that the environment of the test's caller may set and
/// that change what `deadband run` does.
const CALLER_VARIABLES: [&str; 7] = [
    "DEADBAND_HELD",
    "DEADBAND_BYPASS",
    "CARGO_BUILD_JOBS",
    "NEXTEST_TEST_THREADS",
    "RUST_TEST_THREADS",
    "CMAKE_BUILD_PARALLEL_LEVEL",
    "MAKEFLAGS",
];

/// Returns a `deadband` command whose daemon socket is `socket`, without
/// the variables of [`CALLER_VARIABLES`].
fn deadband(socket: &Path, args: &[&str]) -> Command {
    #[expect(
        clippy::disallowed_methods,
        reason = "the test runs the built binary, as a person would"
    )]
    let mut command = Command::new(env!("CARGO_BIN_EXE_deadband"));
    command.args(args).env("DEADBAND_SOCKET", socket);
    for name in CALLER_VARIABLES {
        command.env_remove(name);
    }
    command
}

/// The helpers of the tests that start a daemon.
#[cfg(test)]
mod support {
    use std::io::{BufRead, BufReader};
    use std::path::Path;
    #[expect(
        clippy::disallowed_types,
        reason = "the test holds the daemon it started, to stop it when it ends"
    )]
    use std::process::Child;
    use std::process::{Command, Output, Stdio};

    use super::deadband;

    /// A daemon that the test started, which the test stops when it ends.
    #[expect(
        clippy::disallowed_types,
        reason = "the test holds the daemon it started, to stop it when it ends"
    )]
    pub(crate) struct Daemon(Child);

    impl Daemon {
        /// Starts a daemon on `socket` that runs at most `max_jobs` jobs, and
        /// waits until it listens.
        pub(crate) fn start(socket: &Path, max_jobs: &str) -> Self {
            #[expect(
                clippy::disallowed_methods,
                reason = "the test runs the built binary as a person would"
            )]
            let mut daemon = deadband(socket, &["daemon", "--max-jobs", max_jobs])
                .stderr(Stdio::piped())
                .spawn()
                .expect("the daemon starts");
            let err = daemon.stderr.take().expect("piped");
            let mut first = String::new();
            BufReader::new(err).read_line(&mut first).expect("reads");
            assert!(first.contains("listens on"), "{first}");
            Self(daemon)
        }
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Runs `command` and returns its output.
    pub(crate) fn output(mut command: Command) -> Output {
        #[expect(
            clippy::disallowed_methods,
            reason = "the test runs the built binary, as a person would"
        )]
        let output = command.output().expect("deadband runs");
        output
    }

    /// Returns the nice value of the process `pid`, as `ps` prints it.
    pub(crate) fn nice_of(pid: &str) -> i32 {
        #[expect(
            clippy::disallowed_methods,
            reason = "the test reads a nice value with ps"
        )]
        let ps = Command::new("ps")
            .args(["-o", "nice=", "-p", pid])
            .output()
            .expect("ps runs");
        String::from_utf8(ps.stdout)
            .expect("UTF-8")
            .trim()
            .parse()
            .expect("ps prints a number")
    }
}

use support::{Daemon, nice_of, output};

#[test]
fn a_granted_job_on_a_daemon_of_two_jobs_gets_half_the_cores_as_its_build_jobs() {
    let socket = socket("half");
    let _daemon = Daemon::start(&socket.0, "2");
    let run = output(deadband(
        &socket.0,
        &["run", "--", "sh", "-c", "echo $CARGO_BUILD_JOBS"],
    ));
    assert!(run.status.success(), "{run:?}");
    let cores = std::thread::available_parallelism().expect("the cores read");
    let half = (cores.get() / 2).max(1);
    assert_eq!(
        String::from_utf8(run.stdout).expect("UTF-8"),
        format!("{half}\n")
    );
}

#[test]
fn a_normal_job_keeps_a_nice_value_above_5_that_its_caller_set() {
    let socket = socket("nicer");
    let caller = nice_of(&std::process::id().to_string());
    #[expect(
        clippy::disallowed_methods,
        reason = "the test runs the built binary under nice, as a person would"
    )]
    let mut command = Command::new("nice");
    command
        .args(["-n", "7", env!("CARGO_BIN_EXE_deadband"), "run", "--"])
        .args(["sh", "-c", "ps -o nice= -p $$"])
        .env("DEADBAND_SOCKET", &socket.0);
    let run = output(command);
    let nice = String::from_utf8(run.stdout.clone()).expect("UTF-8");
    assert_eq!(nice.trim().parse::<i32>().ok(), Some(caller + 7), "{run:?}");
}

#[test]
fn a_normal_job_runs_at_nice_5_and_a_fast_job_at_its_callers_nice_value() {
    let socket = socket("nice");
    let _daemon = Daemon::start(&socket.0, "2");
    let caller = nice_of(&std::process::id().to_string());
    let script = "ps -o nice= -p $$";
    let normal = output(deadband(&socket.0, &["run", "--", "sh", "-c", script]));
    let normal = String::from_utf8(normal.stdout).expect("UTF-8");
    assert_eq!(normal.trim().parse::<i32>().ok(), Some(caller.max(5)));
    let args = ["run", "--lane", "fast", "--", "sh", "-c", script];
    let fast = output(deadband(&socket.0, &args));
    let fast = String::from_utf8(fast.stdout).expect("UTF-8");
    assert_eq!(fast.trim().parse::<i32>().ok(), Some(caller));
}

#[test]
fn run_without_a_daemon_exits_with_the_command_and_says_in_one_line_that_the_daemon_is_not_running()
{
    let socket = socket("none");
    let mut run = deadband(&socket.0, &["run", "--", "true"]);
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

/// These tests need the probe, which reads the machine only on macOS.
/// Elsewhere every sample fails, and the daemon holds jobs only on the job
/// count.
#[cfg(target_os = "macos")]
#[expect(
    clippy::disallowed_methods,
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
        let socket = &socket.0;
        let mut daemon = deadband(socket, &["daemon", "--max-jobs", "1"])
            .stderr(Stdio::piped())
            .spawn()
            .expect("the daemon starts");
        let daemon_err = daemon.stderr.take().expect("piped");
        assert!(line(daemon_err).expect("reads").contains("listens on"));

        let started = Instant::now();
        let script = "echo started; sleep 2";
        let mut first = deadband(socket, &["run", "--", "sh", "-c", script])
            .stdout(Stdio::piped())
            .spawn()
            .expect("the first run starts");
        let first_out = first.stdout.take().expect("piped");
        assert_eq!(line(first_out).expect("reads"), "started\n");
        let mut second = deadband(socket, &["run", "--", "sleep", "2"])
            .stderr(Stdio::piped())
            .spawn()
            .expect("the second run starts");
        let second_err = second.stderr.take().expect("piped");
        let waits = line(second_err).expect("reads");
        assert!(
            waits.contains("the job waits because the daemon already runs 1 job"),
            "{waits}"
        );

        let status = deadband(socket, &["status"]).output().expect("status runs");
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

    #[test]
    fn with_one_place_a_fast_job_queued_after_a_normal_job_starts_first() {
        let socket = socket("lanes");
        let socket = &socket.0;
        let _daemon = super::Daemon::start(socket, "1");
        let order = socket.with_extension("order");
        let order_path = order.to_str().expect("UTF-8");

        let mut running = deadband(socket, &["run", "--", "sh", "-c", "echo started; sleep 3"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("the running job starts");
        let out = running.stdout.take().expect("piped");
        assert_eq!(line(out).expect("reads"), "started\n");

        let queue = |lane: &str| {
            let script = format!("echo {lane} >> {order_path}; sleep 1");
            let mut run = deadband(socket, &["run", "--lane", lane, "--", "sh", "-c", &script])
                .stderr(Stdio::piped())
                .spawn()
                .expect("the run starts");
            let err = run.stderr.take().expect("piped");
            let waits = line(err).expect("reads");
            assert!(waits.contains("the job waits"), "{waits}");
            run
        };
        let mut normal = queue("normal");
        let mut fast = queue("fast");

        assert!(running.wait().expect("the running job ends").success());
        assert!(normal.wait().expect("the normal job ends").success());
        assert!(fast.wait().expect("the fast job ends").success());
        let started = std::fs::read_to_string(&order).expect("the order reads");
        let _ = std::fs::remove_file(&order);
        assert_eq!(started, "fast\nnormal\n");
    }

    #[test]
    fn a_run_inside_the_job_that_holds_the_only_place_runs_at_once() {
        let socket = socket("nested");
        let socket = &socket.0;
        let _daemon = super::Daemon::start(socket, "1");
        let started = Instant::now();
        let deadband_path = env!("CARGO_BIN_EXE_deadband");
        let run = deadband(socket, &["run", "--", deadband_path, "run", "--", "true"])
            .output()
            .expect("deadband runs");
        assert!(run.status.success(), "{run:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
