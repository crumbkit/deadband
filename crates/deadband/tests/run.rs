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

/// Returns a configuration directory in the temporary directory that no
/// test makes, so a command of the tests never reads the person's
/// `config.toml`.
fn config_home() -> PathBuf {
    std::env::temp_dir().join(format!("db-it-{}-config", std::process::id()))
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
    command
        .args(args)
        .env("DEADBAND_SOCKET", socket)
        .env("XDG_CONFIG_HOME", config_home());
    for name in CALLER_VARIABLES {
        command.env_remove(name);
    }
    command
}

/// The helpers of the tests that start a daemon.
#[cfg(test)]
mod support {
    use std::io::{BufRead, BufReader, Read};
    use std::path::Path;
    #[expect(
        clippy::disallowed_types,
        reason = "the test holds the daemon it started, to stop it when it ends"
    )]
    use std::process::Child;
    use std::process::{ChildStderr, Command, Output, Stdio};

    use super::deadband;

    /// A daemon that the test started, which the test stops when it ends.
    #[expect(
        clippy::disallowed_types,
        reason = "the test holds the daemon it started, to stop it when it ends"
    )]
    pub(crate) struct Daemon {
        /// The daemon's process.
        child: Child,
        /// The daemon's log on standard error, after the first line.
        log: BufReader<ChildStderr>,
    }

    impl Daemon {
        /// Starts a daemon on `socket` that runs at most `max_jobs` jobs, and
        /// waits until it listens.
        pub(crate) fn start(socket: &Path, max_jobs: &str) -> Self {
            Self::started(deadband(socket, &["daemon", "--max-jobs", max_jobs]))
        }

        /// Starts the daemon that `command` runs, and waits until it listens.
        pub(crate) fn started(mut command: Command) -> Self {
            #[expect(
                clippy::disallowed_methods,
                reason = "the test runs the built binary as a person would"
            )]
            let mut daemon = command
                .stderr(Stdio::piped())
                .spawn()
                .expect("the daemon starts");
            let err = daemon.stderr.take().expect("piped");
            let mut log = BufReader::new(err);
            let mut first = String::new();
            log.read_line(&mut first).expect("reads");
            assert!(first.contains("listens on"), "{first}");
            Self { child: daemon, log }
        }

        /// Stops the daemon and returns what it logged after it began to
        /// listen.
        pub(crate) fn logged(&mut self) -> String {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let mut rest = String::new();
            self.log.read_to_string(&mut rest).expect("the log reads");
            rest
        }
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Returns the command that runs `script` under `deadband run` with
    /// its standard streams piped, so the test ends the job by closing its
    /// standard input.
    pub(crate) fn piped_job(socket: &Path, script: &str) -> Command {
        let mut run = deadband(socket, &["run", "--", "sh", "-c", script]);
        run.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        run
    }

    /// Reads one line from `stream`.
    pub(crate) fn line(stream: impl Read) -> std::io::Result<String> {
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        Ok(line)
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

use support::{Daemon, line, nice_of, output, piped_job};

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
    // The increment puts the job's caller at nice 6 or above, whatever the
    // test runner's own nice value is.
    let increment = (6 - caller).max(1);
    command
        .args(["-n", &increment.to_string()])
        .args([env!("CARGO_BIN_EXE_deadband"), "run", "--"])
        .args(["sh", "-c", "ps -o nice= -p $$"])
        .env("DEADBAND_SOCKET", &socket.0);
    let run = output(command);
    let nice = String::from_utf8(run.stdout.clone()).expect("UTF-8");
    assert_eq!(
        nice.trim().parse::<i32>().ok(),
        Some(caller + increment),
        "{run:?}"
    );
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

/// This test kills a daemon while a job runs. It needs no probe, because it
/// never asks `status`, and a daemon holds a job on the job count on every
/// system.
#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "the test runs the built binary as a person would"
)]
mod restart {
    use std::time::Duration;

    use super::{Daemon, line, piped_job, socket};

    #[test]
    fn a_job_whose_daemon_never_returns_ends_normally_with_its_own_exit_code() {
        let socket = socket("gone");
        let socket = &socket.0;
        let daemon = Daemon::start(socket, "1");
        let mut run = piped_job(socket, "echo started; cat; exit 7")
            .spawn()
            .expect("the run starts");
        assert_eq!(
            line(run.stdout.take().expect("piped")).expect("reads"),
            "started\n"
        );

        drop(daemon);
        // Several connections fail in this time, one per 100 ms. The proof
        // is the job's own end below, so a slow machine costs nothing.
        std::thread::sleep(Duration::from_millis(300));
        drop(run.stdin.take().expect("piped"));

        let ended = run.wait_with_output().expect("the run ends");
        assert_eq!(ended.status.code(), Some(7), "{ended:?}");
        assert_eq!(String::from_utf8_lossy(&ended.stderr), "");
    }
}

/// These tests need the probe, which reads the machine only on macOS.
/// Elsewhere every sample fails, and the daemon holds jobs only on the job
/// count.
#[cfg(test)]
#[cfg(target_os = "macos")]
#[expect(
    clippy::disallowed_methods,
    reason = "the test runs the built binary as a person would, and stops the daemon it started"
)]
mod macos {
    use std::path::Path;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use super::{Daemon, deadband, line, piped_job, socket};

    /// How long a test waits for a new daemon to show the running job.
    const SHOWN_WITHIN: Duration = Duration::from_secs(20);

    /// Runs `deadband status` until its output holds `wanted`. Each run is
    /// an observable round trip with the daemon.
    fn status_until(socket: &Path, wanted: &str) {
        let begun = Instant::now();
        loop {
            let status = deadband(socket, &["status"]).output().expect("status runs");
            let status = String::from_utf8(status.stdout).expect("UTF-8");
            if status.contains(wanted) {
                return;
            }
            assert!(begun.elapsed() < SHOWN_WITHIN, "{status}");
        }
    }

    #[test]
    fn a_job_that_runs_when_its_daemon_is_killed_counts_on_the_next_daemon() {
        let socket = socket("restart");
        let socket = &socket.0;
        let daemon = Daemon::start(socket, "1");
        let mut first = piped_job(socket, "echo started; cat")
            .spawn()
            .expect("the first run starts");
        let first_out = first.stdout.take().expect("piped");
        assert_eq!(line(first_out).expect("reads"), "started\n");

        drop(daemon);
        let _next = Daemon::start(socket, "1");
        status_until(socket, "daemon: 1 running, 0 waiting, at most 1 at once");

        let mut second = deadband(socket, &["run", "--", "echo", "second"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the second run starts");
        let second_err = second.stderr.take().expect("piped");
        let waits = line(second_err).expect("reads");
        assert!(
            waits.contains("the job waits because the daemon already runs 1 job"),
            "{waits}"
        );

        drop(first.stdin.take().expect("piped"));
        assert!(first.wait().expect("the first run ends").success());
        let second = second.wait_with_output().expect("the second run ends");
        assert!(second.status.success(), "{second:?}");
        assert_eq!(second.stdout, b"second\n");
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
    fn a_job_run_inside_a_workspace_shows_its_workspace_and_short_command_in_status() {
        let socket = socket("workspace");
        let socket = &socket.0;
        let root = std::env::temp_dir().join(format!("db-it-{}-ws", std::process::id()));
        let checkout = root.join("triage").join("api");
        std::fs::create_dir_all(checkout.join(".git")).expect("the checkout is made");
        #[expect(
            clippy::disallowed_methods,
            reason = "the test writes the marker that a workspace tool writes"
        )]
        std::fs::write(root.join("triage").join(".deadband-workspace"), "")
            .expect("the marker writes");
        let _daemon = Daemon::start(socket, "1");
        let mut run = deadband(
            socket,
            &[
                "run",
                "--label",
                "tests",
                "--",
                "sh",
                "-c",
                "echo started; cat",
            ],
        );
        let mut job = run
            .current_dir(&checkout)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("the run starts");
        assert_eq!(
            line(job.stdout.take().expect("piped")).expect("reads"),
            "started\n"
        );
        let status = deadband(socket, &["status"]).output().expect("status runs");
        let status = String::from_utf8(status.stdout).expect("UTF-8");
        drop(job.stdin.take().expect("piped"));
        assert!(job.wait().expect("the run ends").success());
        let _ = std::fs::remove_dir_all(&root);
        let running = status
            .lines()
            .find(|line| line.starts_with("running: "))
            .unwrap_or_else(|| panic!("no running line: {status}"));
        assert!(
            running.starts_with("running: triage · tests (pid "),
            "{status}"
        );
        assert!(running.ends_with("): sh"), "{status}");
        assert!(!status.contains("echo started"), "{status}");
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

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "a test writes a config file, and a partial fixture only fails that test"
)]
mod reclaim_config {
    use std::path::PathBuf;

    use super::{Daemon, deadband, output, socket};

    /// A configuration directory whose `deadband/config.toml` lists one
    /// reclaimer. Dropping it removes the directory.
    struct Config(PathBuf);

    impl Config {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("db-rc-{}-{name}", std::process::id()));
            let directory = path.join("deadband");
            std::fs::create_dir_all(&directory).expect("the directory is made");
            std::fs::write(
                directory.join("config.toml"),
                "[[reclaim]]\nname = \"marker\"\ncommand = [\"/usr/bin/true\"]\n",
            )
            .expect("the config writes");
            Self(path)
        }
    }

    impl Drop for Config {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_daemon_started_with_its_own_socket_runs_no_reclaimer_from_the_config_file() {
        let config = Config::new("own");
        let socket = socket("own-reclaim");
        let mut command = deadband(&socket.0, &["daemon", "--max-jobs", "1"]);
        command.env("XDG_CONFIG_HOME", &config.0);
        let mut daemon = Daemon::started(command);
        // The daemon answers a request only after it has read its config.
        assert!(output(deadband(&socket.0, &["status"])).status.success());
        let log = daemon.logged();
        assert!(!log.contains("marker"), "{log}");
    }

    #[test]
    fn a_daemon_on_the_default_socket_asks_the_reclaimer_from_the_config_file() {
        let config = Config::new("default");
        let tmp = config.0.join("t");
        std::fs::create_dir_all(&tmp).expect("the directory is made");
        let mut command = deadband(
            std::path::Path::new("unused"),
            &["daemon", "--max-jobs", "1"],
        );
        command
            .env_remove("DEADBAND_SOCKET")
            .env("TMPDIR", &tmp)
            .env("XDG_CONFIG_HOME", &config.0);
        let mut daemon = Daemon::started(command);
        let mut status = deadband(std::path::Path::new("unused"), &["status"]);
        status.env_remove("DEADBAND_SOCKET").env("TMPDIR", &tmp);
        assert!(output(status).status.success());
        let log = daemon.logged();
        assert!(log.contains("asks marker to reclaim"), "{log}");
    }
}

#[test]
fn the_socket_helper_removes_the_socket_and_its_lock_file_when_it_ends() {
    let path = {
        let socket = socket("removed");
        let _daemon = Daemon::start(&socket.0, "1");
        assert!(socket.0.exists());
        socket.0.clone()
    };
    assert!(!path.exists());
    assert!(!deadband::daemon::lock_path(&path).exists());
}

#[cfg(feature = "tui")]
#[test]
fn top_refuses_a_pipe_and_names_status() {
    let socket = socket("top-pipe");
    #[expect(
        clippy::disallowed_methods,
        reason = "the test runs the built binary with its output piped, as a pipeline would"
    )]
    let output = deadband(&socket.0, &["top"])
        .output()
        .expect("the binary runs");
    assert_eq!(output.status.code(), Some(5));
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("deadband status"), "{err}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains('\u{1b}'));
}

/// The tests of the history file through the built binary.
#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "a test writes the file that blocks the state directory, and a partial fixture only fails that test"
)]
mod history {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    use super::{Daemon, deadband, output, socket};

    /// A directory for the daemon's socket and state. Dropping it removes it.
    struct Home(PathBuf);

    impl Home {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("db-h-{}-{name}", std::process::id()));
            std::fs::create_dir_all(path.join("t")).expect("the directory is made");
            Self(path)
        }

        fn state(&self) -> PathBuf {
            self.0.join("s")
        }

        fn file(&self) -> PathBuf {
            self.state().join("deadband/jobs.jsonl")
        }

        /// Returns a `deadband` command that finds its socket through
        /// `DEADBAND_SOCKET_DIR`, which a daemon on the default socket reads
        /// before the per-user directory, and its history through
        /// `XDG_STATE_HOME`.
        fn command(&self, args: &[&str]) -> Command {
            let mut command = deadband(Path::new("unused"), args);
            command
                .env_remove("DEADBAND_SOCKET")
                .env("DEADBAND_SOCKET_DIR", self.0.join("t"))
                .env("XDG_STATE_HOME", self.state());
            command
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Reads the file at `path` until it holds a line, for at most ten
    /// seconds, and returns the first line as JSON.
    fn first_line(path: &Path) -> serde_json::Value {
        let begun = Instant::now();
        loop {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            if let Some(line) = text.lines().next() {
                return serde_json::from_str(line).expect("the line is JSON");
            }
            assert!(begun.elapsed() < Duration::from_secs(10), "no line came");
        }
    }

    #[test]
    fn a_daemon_on_the_default_socket_records_a_finished_job_and_the_job_keeps_its_exit_code() {
        let home = Home::new("rec");
        let _daemon = Daemon::started(home.command(&["daemon", "--max-jobs", "1"]));
        let run = output(home.command(&["run", "--", "sh", "-c", "exit 3"]));
        assert_eq!(run.status.code(), Some(3), "{run:?}");
        let line = first_line(&home.file());
        assert_eq!(line["type"], "job");
        assert_eq!(line["kind"], "sh");
        assert_eq!(line["exit"]["exited"]["code"], 3);
        if cfg!(target_os = "macos") {
            assert!(line["child_cpu_ms"].is_u64(), "{line}");
            assert!(line["child_peak_rss_bytes"].is_u64(), "{line}");
        } else {
            assert!(line["child_cpu_ms"].is_null(), "{line}");
        }
    }

    #[test]
    fn a_daemon_started_with_its_own_socket_runs_the_job_and_records_nothing() {
        let home = Home::new("own");
        let socket = socket("own-history");
        let mut daemon = deadband(&socket.0, &["daemon", "--max-jobs", "1"]);
        daemon.env("XDG_STATE_HOME", home.state());
        let _daemon = Daemon::started(daemon);
        let mut job = deadband(&socket.0, &["run", "--", "true"]);
        job.env("XDG_STATE_HOME", home.state());
        let run = output(job);
        assert!(run.status.success(), "{run:?}");
        // A daemon on the default socket records into the same state
        // directory. Had the first daemon recorded, its writer would hold the
        // file's lock, and the second daemon would record nothing.
        let _second = Daemon::started(home.command(&["daemon", "--max-jobs", "1"]));
        let run = output(home.command(&["run", "--label", "second", "--", "true"]));
        assert!(run.status.success(), "{run:?}");
        let line = first_line(&home.file());
        assert_eq!(line["label"], "second", "{line}");
        let text = std::fs::read_to_string(home.file()).expect("the file reads");
        assert_eq!(text.lines().count(), 1, "{text}");
    }

    #[test]
    fn a_state_directory_that_cannot_be_made_still_runs_the_job_with_its_output() {
        let home = Home::new("bad");
        std::fs::write(home.state(), "").expect("the file writes");
        let _daemon = Daemon::started(home.command(&["daemon", "--max-jobs", "1"]));
        let run = output(home.command(&["run", "--", "sh", "-c", "echo ran"]));
        assert!(run.status.success(), "{run:?}");
        assert_eq!(String::from_utf8(run.stdout).expect("UTF-8"), "ran\n");
    }
}
