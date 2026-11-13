//! Tests of the `deadband` commands, run against fake probes and real
//! sockets.

use std::io::BufReader;
use std::num::NonZeroUsize;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crumbkit::Sink;

use super::{EXIT_DAEMON, EXIT_MACHINE, EXIT_NOT_FOUND, EXIT_NOT_STARTED, Machine, run_to};
use crate::admit::{GIB, Hold, Lane};
use crate::daemon::{Log, Settings, bind, serve};
use crate::environment::Environment;
use crate::probe::{Disk, Pressure, Probe, Sample, Unreadable};
use crate::probe::{ProcessUse, TreeUnreadable};
use crate::protocol::{
    Exit, Job, JobState, Jobs, Label, OutsideJobs, Reply, Request, Short, StuckJob, read_line,
    write_line,
};
use crate::reclaim::Reclaiming;
use crate::testing::{TestSocket, socket_path};

mod reclaim;
mod service;

/// How long a test waits for a reply that it expects.
const REPLY_WITHIN: Duration = Duration::from_secs(10);

/// A probe that returns one fixed sample, with normal memory pressure.
#[derive(Clone)]
struct Fixed;

impl Probe for Fixed {
    fn sample(&self) -> Result<Sample, Unreadable> {
        Ok(Sample {
            load_1m: 4.25,
            memory_pressure: Pressure::Normal,
            swap_used_bytes: 11_605 * 1_048_576,
            swap_total_bytes: 13_312 * 1_048_576,
        })
    }

    fn disk(&self, _path: &Path) -> std::io::Result<Disk> {
        Ok(Disk {
            free_bytes: 500 * GIB,
            total_bytes: 1000 * GIB,
        })
    }
}

/// A probe that cannot read the machine.
#[derive(Clone)]
struct Broken;

impl Probe for Broken {
    fn sample(&self) -> Result<Sample, Unreadable> {
        Err(Unreadable::Load)
    }

    fn disk(&self, _path: &Path) -> std::io::Result<Disk> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

/// A machine with `probe`, a socket that no daemon listens on yet, and a
/// log that the test reads.
struct Test<P> {
    machine: Machine<P>,
    log: Arc<Mutex<Vec<u8>>>,
    /// The machine's socket, which the test removes when it ends.
    _socket: TestSocket,
}

impl<P: Probe + Clone + Send + 'static> Test<P> {
    fn new(probe: P) -> Self {
        let log = Arc::new(Mutex::new(Vec::new()));
        let shared: Log = log.clone();
        let socket = socket_path();
        let machine = Machine {
            probe,
            socket: socket.to_path_buf(),
            log: shared,
            cores: NonZeroUsize::new(4),
            environment: Environment::default(),
            service: None,
        };
        Self {
            machine,
            log,
            _socket: socket,
        }
    }

    /// Starts a daemon on the machine's socket that runs at most
    /// `max_jobs` jobs at once.
    fn start_daemon(&self, max_jobs: usize) {
        let bound = bind(&self.machine.socket).expect("the test socket binds");
        let probe = self.machine.probe.clone();
        let log = Arc::clone(&self.machine.log);
        let max_jobs = NonZeroUsize::new(max_jobs).expect("positive");
        // The closure takes the whole `Bound`, so its lock lives as long as
        // the daemon's thread.
        thread::spawn(move || {
            let bound = bound;
            let settings = Settings::new(max_jobs, NonZeroUsize::new(4), None, None);
            serve(&bound.listener, probe, settings, &log)
        });
    }

    /// Runs `deadband` with `args`, and returns the exit code, standard
    /// output and standard error.
    fn run(&self, args: &[&str]) -> (u8, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = run_to(
            &mut Sink::new(&mut out, &mut err),
            &self.machine,
            std::iter::once("deadband").chain(args.iter().copied()),
        );
        (
            code,
            String::from_utf8(out).expect("the output is UTF-8"),
            String::from_utf8(err).expect("the error output is UTF-8"),
        )
    }

    fn log(&self) -> String {
        String::from_utf8(self.log.lock().expect("the log locks").clone()).expect("UTF-8")
    }
}

/// Asks the daemon at `socket` to start a job labelled `label`, waits for
/// its first reply, and returns the open connection with that reply.
fn ask(socket: &Path, label: &str) -> (UnixStream, Reply) {
    let mut stream = UnixStream::connect(socket).expect("the daemon accepts");
    stream
        .set_read_timeout(Some(REPLY_WITHIN))
        .expect("the timeout sets");
    let job = Job {
        label: label.to_owned(),
        pid: 5,
        command: format!("make {label}"),
        lane: Lane::Normal,
        watched: false,
        cwd: None,
        workspace: None,
        short: None,
    };
    write_line(&mut stream, &Request::Ask(job)).expect("the request writes");
    let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
    let reply = read_line(&mut reader)
        .expect("the reply reads")
        .expect("a reply");
    (stream, reply)
}

#[test]
fn status_prints_load_memory_pressure_swap_and_that_the_daemon_is_not_running() {
    let (code, out, err) = Test::new(Fixed).run(&["status"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "load: 4.25 (1-minute average)\n\
         memory pressure: normal\n\
         swap: 11605 MiB used of 13312 MiB\n\
         daemon: not running\n"
    );
}

#[test]
fn status_lists_the_daemons_running_and_waiting_jobs() {
    let test = Test::new(Fixed);
    test.start_daemon(1);
    let (_first, reply) = ask(&test.machine.socket, "build");
    assert_eq!(reply, Reply::Granted { jobs: 4 });
    let (_second, reply) = ask(&test.machine.socket, "tests");
    assert!(matches!(reply, Reply::Queued { ahead: 0, .. }), "{reply:?}");
    let (code, out, err) = test.run(&["status"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.ends_with(
            "daemon: 1 running, 1 waiting, at most 1 at once\n\
             tests waits because: the daemon already runs 1 job, its most at once\n\
             running: build (pid 5): make\n\
             waiting: tests (pid 5): make\n"
        ),
        "{out}"
    );
}

#[test]
fn status_prints_no_reason_once_the_waiting_job_has_started() {
    let test = Test::new(Fixed);
    test.start_daemon(1);
    let (first, reply) = ask(&test.machine.socket, "build");
    assert_eq!(reply, Reply::Granted { jobs: 4 });
    let (second, reply) = ask(&test.machine.socket, "tests");
    assert!(matches!(reply, Reply::Queued { ahead: 0, .. }), "{reply:?}");
    let (_, out, _) = test.run(&["status"]);
    assert!(out.contains("tests waits because: "), "{out}");
    drop(first);
    let mut reader = BufReader::new(second);
    let granted = read_line::<Reply>(&mut reader).expect("the reply reads");
    assert_eq!(granted, Some(Reply::Granted { jobs: 4 }));
    let (code, out, err) = test.run(&["status"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("running: tests"), "{out}");
    assert!(!out.contains("waits because"), "{out}");
}

#[test]
fn status_with_json_prints_the_sample_and_the_daemon_in_the_status_document() {
    let (code, out, err) = Test::new(Fixed).run(&["status", "--json"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(r#""schema":"deadband/status/v2""#), "{out}");
    assert!(out.contains(r#""load_1m":4.25"#), "{out}");
    assert!(out.contains(r#""memory_pressure":"normal""#), "{out}");
    assert!(out.contains(r#""swap_used_bytes":12168724480"#), "{out}");
    assert!(out.contains(r#""swap_total_bytes":13958643712"#), "{out}");
    assert!(out.contains(r#""daemon":null"#), "{out}");
}

#[test]
fn status_says_in_one_line_why_the_probe_failed() {
    let (code, out, err) = Test::new(Broken).run(&["status"]);
    assert_eq!(code, EXIT_MACHINE);
    assert_eq!(out, "");
    assert!(
        err.contains("deadband could not read the 1-minute load average"),
        "{err}"
    );
    assert!(
        err.contains("code:  deadband::machine::unreadable"),
        "{err}"
    );
}

#[test]
fn run_without_a_daemon_runs_the_command_at_once_and_says_why_in_one_line() {
    let test = Test::new(Fixed);
    let (code, out, err) = test.run(&["run", "--", "sh", "-c", "exit 3"]);
    assert_eq!(code, 3, "{err}");
    assert_eq!(out, "");
    assert_eq!(
        test.log(),
        format!(
            "deadband: the daemon is not running (nothing listens on {}), so the job starts now\n",
            test.machine.socket.display()
        )
    );
}

#[test]
fn run_with_a_daemon_runs_the_command_and_frees_its_place_when_it_ends() {
    let test = Test::new(Fixed);
    test.start_daemon(1);
    let (code, _, err) = test.run(&["run", "--label", "first", "--", "true"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(test.log(), "");
    // The daemon frees the place on its own thread once it reads the end;
    // each status request is an observable round trip.
    let mut listed = crate::client::jobs(&test.machine.socket, REPLY_WITHIN);
    while listed
        .as_ref()
        .map_or(true, |jobs| !jobs.running.is_empty())
    {
        listed = crate::client::jobs(&test.machine.socket, REPLY_WITHIN);
    }
    let (_next, reply) = ask(&test.machine.socket, "next");
    assert_eq!(reply, Reply::Granted { jobs: 4 });
}

/// A shell script that exits 0 when the job's parallelism variables all
/// give `jobs` and the job is marked held.
fn checks_parallelism(jobs: usize) -> String {
    format!(
        "test \"$CARGO_BUILD_JOBS\" = {jobs} && test \"$NEXTEST_TEST_THREADS\" = {jobs} \
         && test \"$RUST_TEST_THREADS\" = {jobs} && test \"$CMAKE_BUILD_PARALLEL_LEVEL\" = {jobs} \
         && test \"$MAKEFLAGS\" = -j{jobs} && test \"$DEADBAND_HELD\" = 1"
    )
}

#[test]
fn run_with_a_daemon_gives_the_job_the_cores_divided_by_max_jobs_in_each_parallelism_variable() {
    let test = Test::new(Fixed);
    test.start_daemon(2);
    let script = checks_parallelism(2);
    let (code, _, err) = test.run(&["run", "--", "sh", "-c", &script]);
    assert_eq!(code, 0, "{err}{}", test.log());
}

#[test]
fn run_without_a_daemon_still_caps_the_jobs_threads_at_the_default_share() {
    let test = Test::new(Fixed);
    let script = "test \"$CARGO_BUILD_JOBS\" = 4 && test \"$NEXTEST_TEST_THREADS\" = 4 \
         && test \"$MAKEFLAGS\" = -j4";
    let (code, _, err) = test.run(&["run", "--", "sh", "-c", script]);
    assert_eq!(code, 0, "{err}{}", test.log());
}

#[test]
fn run_in_the_fast_lane_gives_the_job_one_more_thread() {
    let test = Test::new(Fixed);
    test.start_daemon(2);
    let script = checks_parallelism(3);
    let (code, _, err) = test.run(&["run", "--lane", "fast", "--", "sh", "-c", &script]);
    assert_eq!(code, 0, "{err}{}", test.log());
}

/// A directory in the temporary directory that holds a priority file with
/// `words`, removed when the test ends.
struct Config(PathBuf);

impl Config {
    fn with(words: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let directory = std::env::temp_dir().join(format!("db-config-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("the directory is made");
        #[expect(
            clippy::disallowed_methods,
            reason = "the test writes the priority file a person would write"
        )]
        std::fs::write(directory.join("priority"), words).expect("the file writes");
        Self(directory)
    }
}

impl Drop for Config {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn run_takes_the_fast_lane_when_its_label_is_in_the_priority_file() {
    let mut test = Test::new(Fixed);
    let config = Config::with("urgent\n");
    test.machine.environment.config = Some(config.0.clone());
    test.start_daemon(2);
    let script = checks_parallelism(3);
    let args = ["run", "--label", "urgent", "--", "sh", "-c", &script];
    let (code, _, err) = test.run(&args);
    assert_eq!(code, 0, "{err}{}", test.log());
}

#[test]
fn run_with_lane_normal_stays_in_the_normal_lane_whatever_the_priority_file_says() {
    let mut test = Test::new(Fixed);
    let config = Config::with("urgent\n");
    test.machine.environment.config = Some(config.0.clone());
    test.start_daemon(2);
    let script = checks_parallelism(2);
    let args = [
        "run", "--label", "urgent", "--lane", "normal", "--", "sh", "-c", &script,
    ];
    let (code, _, err) = test.run(&args);
    assert_eq!(code, 0, "{err}{}", test.log());
}

#[test]
fn run_inside_a_granted_job_runs_at_once_without_asking() {
    let mut test = Test::new(Fixed);
    test.machine.environment.held = true;
    test.start_daemon(1);
    let (_busy, reply) = ask(&test.machine.socket, "busy");
    assert_eq!(reply, Reply::Granted { jobs: 4 });
    let (code, _, err) = test.run(&["run", "--", "true"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(test.log(), "");
}

#[test]
fn run_with_bypass_runs_at_once_without_asking() {
    let mut test = Test::new(Fixed);
    test.machine.environment.bypass = true;
    test.start_daemon(1);
    let (_busy, reply) = ask(&test.machine.socket, "busy");
    assert_eq!(reply, Reply::Granted { jobs: 4 });
    let (code, _, err) = test.run(&["run", "--", "true"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(test.log(), "");
}

#[test]
fn run_says_in_one_line_why_its_job_waits() {
    let test = Test::new(Fixed);
    test.start_daemon(1);
    let (busy, reply) = ask(&test.machine.socket, "busy");
    assert_eq!(reply, Reply::Granted { jobs: 4 });
    thread::scope(|scope| {
        let waiting = scope.spawn(|| test.run(&["run", "--", "true"]));
        let mut listed = crate::client::jobs(&test.machine.socket, REPLY_WITHIN);
        // Each status request is an observable round trip to the daemon.
        while listed.as_ref().map_or(true, |jobs| jobs.waiting.is_empty()) {
            listed = crate::client::jobs(&test.machine.socket, REPLY_WITHIN);
        }
        drop(busy);
        let (code, _, err) = waiting.join().expect("the run ends");
        assert_eq!(code, 0, "{err}");
    });
    assert_eq!(
        test.log(),
        "deadband: the job waits because the daemon already runs 1 job, its most at once, and no other job waits ahead of it\n"
    );
}

/// Runs `sh -c script` through a daemon and returns its row on the timeline.
fn ended_row(script: &str) -> JobState {
    let test = Test::new(Fixed);
    test.start_daemon(1);
    test.run(&["run", "--label", "a", "--", "sh", "-c", script]);
    let begun = std::time::Instant::now();
    loop {
        let timeline = crate::client::timeline(&test.machine.socket, REPLY_WITHIN)
            .expect("the daemon answers");
        let state = timeline.jobs.first().map(|row| row.state);
        if let Some(state @ (JobState::Finished { .. } | JobState::Left { .. })) = state {
            return state;
        }
        assert!(begun.elapsed() < REPLY_WITHIN, "{timeline:?}");
    }
}

#[test]
fn run_tells_the_daemon_the_signal_that_ended_its_job() {
    let state = ended_row("kill -9 $$");
    assert!(
        matches!(
            state,
            JobState::Finished {
                exit: Exit::Signalled { signal: 9 },
                ..
            }
        ),
        "{state:?}"
    );
}

#[test]
fn run_tells_the_daemon_the_exit_code_of_its_job() {
    let state = ended_row("exit 3");
    assert!(
        matches!(
            state,
            JobState::Finished {
                exit: Exit::Exited { code: 3 },
                ..
            }
        ),
        "{state:?}"
    );
}

#[test]
fn run_exits_127_with_job_not_found_when_the_program_does_not_exist() {
    let test = Test::new(Fixed);
    let (code, out, err) = test.run(&["run", "--", "deadband-test-no-such-program"]);
    assert_eq!(code, EXIT_NOT_FOUND);
    assert_eq!(out, "");
    assert!(err.contains("code:  deadband::job::not_found"), "{err}");
}

#[test]
fn run_exits_126_with_job_not_started_when_the_program_cannot_start() {
    let test = Test::new(Fixed);
    let directory = std::env::temp_dir();
    let directory = directory
        .to_str()
        .expect("the temporary directory is UTF-8");
    let (code, out, err) = test.run(&["run", "--", directory]);
    assert_eq!(code, EXIT_NOT_STARTED, "{err}");
    assert_eq!(out, "");
    assert!(err.contains("code:  deadband::job::not_started"), "{err}");
}

#[test]
fn daemon_exits_0_with_daemon_running_while_another_daemon_answers_on_its_socket() {
    let test = Test::new(Fixed);
    test.start_daemon(1);
    let (code, out, err) = test.run(&["daemon"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "");
    assert!(err.contains("code:  deadband::daemon::running"), "{err}");
}

#[test]
fn daemon_exits_4_with_socket_unusable_when_a_file_that_is_not_a_socket_holds_its_path() {
    let test = Test::new(Fixed);
    #[expect(clippy::disallowed_methods, reason = "the test writes a fixture")]
    std::fs::write(&test.machine.socket, "").expect("writable");
    let (code, out, err) = test.run(&["daemon"]);
    assert_eq!(code, EXIT_DAEMON, "{err}");
    assert_eq!(out, "");
    assert!(err.contains("code:  deadband::socket::unusable"), "{err}");
}

#[cfg(feature = "tui")]
#[test]
fn top_with_json_refuses_with_not_a_terminal_and_names_status() {
    let (code, out, err) = Test::new(Fixed).run(&["top", "--json"]);
    assert_eq!(code, crate::cli::EXIT_TOP);
    let printed = out + &err;
    assert!(
        printed.contains("deadband::top::not_a_terminal"),
        "{printed}"
    );
    assert!(printed.contains("deadband status"), "{printed}");
}

/// A probe whose job trees never change.
#[derive(Clone)]
struct QuietTree;

impl Probe for QuietTree {
    fn sample(&self) -> Result<Sample, Unreadable> {
        Fixed.sample()
    }

    fn disk(&self, path: &Path) -> std::io::Result<Disk> {
        Fixed.disk(path)
    }

    fn tree(&self, root: u32) -> Result<Vec<ProcessUse>, TreeUnreadable> {
        Ok(vec![ProcessUse {
            pid: root,
            cpu: Duration::ZERO,
            disk_bytes: 0,
        }])
    }
}

/// Starts a daemon on `test`'s socket that flags a quiet job within
/// milliseconds.
fn start_watching(test: &Test<QuietTree>) {
    let bound = bind(&test.machine.socket).expect("the test socket binds");
    let probe = test.machine.probe.clone();
    let log = Arc::clone(&test.machine.log);
    thread::spawn(move || {
        let bound = bound;
        let mut settings = Settings::new(NonZeroUsize::MIN, NonZeroUsize::new(4), None, None);
        settings.reclaim.stuck.after = Some(Duration::from_millis(30));
        settings.watching.every = Duration::from_millis(5);
        settings.watching.census = vec!["/bin/sh".to_owned(), "-c".to_owned(), "exit 1".to_owned()];
        serve(&bound.listener, probe, settings, &log)
    });
    while !test.machine.socket.exists() {
        thread::yield_now();
    }
}

#[test]
fn a_job_flagged_stuck_still_runs_to_its_own_exit_code() {
    let test = Test::new(QuietTree);
    start_watching(&test);
    let (code, _, _) = test.run(&["run", "--", "sh", "-c", "sleep 1; exit 7"]);
    assert_eq!(code, 7);
    let log = test.log();
    assert!(log.contains("the job has shown no CPU use"), "{log}");
    assert!(log.contains("deadband leaves it running"), "{log}");
}

#[test]
fn a_job_that_ends_before_the_limit_is_not_flagged_and_keeps_its_exit_code() {
    let test = Test::new(QuietTree);
    start_watching(&test);
    let (code, _, _) = test.run(&["run", "--", "sh", "-c", "exit 7"]);
    assert_eq!(code, 7);
    assert!(!test.log().contains("no CPU use"), "{}", test.log());
}

/// Listens on `test`'s socket and answers one status request with `jobs`.
fn answer_status<P>(test: &Test<P>, jobs: Jobs) {
    let listener =
        std::os::unix::net::UnixListener::bind(&test.machine.socket).expect("the socket binds");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("the client connects");
        let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
        let _ = read_line::<Request>(&mut reader);
        write_line(&mut stream, &Reply::Jobs(jobs)).expect("the reply writes");
    });
}

fn listed(label: &str, pid: u32) -> Job {
    Job {
        label: label.to_owned(),
        pid,
        command: format!("make {label}"),
        lane: Lane::Normal,
        watched: true,
        cwd: None,
        workspace: None,
        short: None,
    }
}

/// Returns a job from a daemon that sends a workspace and a short command.
fn shown(label: &str, pid: u32, workspace: Option<&str>, short: &str) -> Job {
    Job {
        command: "tool --token s3cr3t deploy".to_owned(),
        workspace: workspace.map(Label::new),
        short: Some(Short::new(short)),
        ..listed(label, pid)
    }
}

/// Returns the jobs of a daemon that sends workspaces and short commands.
fn shown_jobs() -> Jobs {
    Jobs {
        running: vec![
            shown("reading-list", 11, Some("reading-list"), "cargo build"),
            shown("api-tests", 22, Some("triage"), "cargo nextest run"),
            shown("crumbkit-docs", 33, None, "cargo doc"),
        ],
        waiting: vec![
            shown("cargo", 44, Some("board-sync"), "cargo test"),
            shown("tool", 55, None, "tool"),
            shown("script", 66, Some("triage"), ""),
        ],
        held: Some(Hold::Full { max_jobs: 4 }),
        ..running_jobs()
    }
}

#[test]
fn status_names_each_job_by_workspace_and_label_and_ends_with_its_short_command() {
    let test = Test::new(Fixed);
    answer_status(&test, shown_jobs());
    let (code, out, err) = test.run(&["status"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        format!(
            "{STATUS_TOP}daemon: 3 running, 3 waiting, at most 4 at once\n\
             board-sync waits because: the daemon already runs 4 jobs, its most at once\n\
             running: reading-list (pid 11): cargo build\n\
             running: triage · api-tests (pid 22): cargo nextest run\n\
             running: crumbkit-docs (pid 33): cargo doc\n\
             waiting: board-sync (pid 44): cargo test\n\
             waiting: tool (pid 55): tool\n\
             waiting: triage · script (pid 66)\n"
        )
    );
    assert!(!out.contains("s3cr3t"), "{out}");
}

#[test]
fn status_names_a_stuck_job_as_its_running_line_does() {
    let test = Test::new(Fixed);
    answer_status(
        &test,
        Jobs {
            stuck: vec![StuckJob {
                label: "api-tests".to_owned(),
                pid: 22,
                quiet_seconds: 120,
            }],
            ..shown_jobs()
        },
    );
    let (_, out, _) = test.run(&["status"]);
    assert!(
        out.contains("\nstuck: triage · api-tests (pid 22): no CPU"),
        "{out}"
    );
}

#[test]
fn status_with_json_gives_each_job_its_workspace_and_short_command_and_no_command_line() {
    let test = Test::new(Fixed);
    answer_status(&test, shown_jobs());
    let (_, out, _) = test.run(&["status", "--json"]);
    let value: serde_json::Value = serde_json::from_str(&out).expect("JSON");
    assert_eq!(value["schema"], "deadband/status/v2");
    let job = &value["data"]["daemon"]["running"][1];
    assert_eq!(
        job,
        &serde_json::json!({
            "label": "api-tests",
            "workspace": "triage",
            "short": "cargo nextest run",
            "pid": 22,
            "lane": "normal",
            "watched": true,
        })
    );
    assert!(!out.contains("s3cr3t"), "{out}");
}

#[test]
fn status_with_json_from_an_older_daemon_leaves_out_the_command_line() {
    let test = Test::new(Fixed);
    answer_status(&test, running_jobs());
    let (_, out, _) = test.run(&["status", "--json"]);
    let value: serde_json::Value = serde_json::from_str(&out).expect("JSON");
    let job = &value["data"]["daemon"]["running"][0];
    assert_eq!(job["label"], "first");
    assert!(job.get("command").is_none(), "{job}");
    assert!(job.get("short").is_none(), "{job}");
}

fn running_jobs() -> Jobs {
    Jobs {
        max_jobs: 4,
        running: vec![listed("first", 11), listed("second", 22)],
        waiting: Vec::new(),
        held: None,
        reclaim: None,
        reclaiming: None,
        stuck: Vec::new(),
        busiest: None,
        unreadable: false,
    }
}

const STATUS_TOP: &str =
    "load: 4.25 (1-minute average)\nmemory pressure: normal\nswap: 11605 MiB used of 13312 MiB\n";

#[test]
fn status_lists_a_stuck_job_first_and_exits_1() {
    let test = Test::new(Fixed);
    answer_status(
        &test,
        Jobs {
            stuck: vec![StuckJob {
                label: "second".to_owned(),
                pid: 22,
                quiet_seconds: 46 * 60,
            }],
            busiest: Some(OutsideJobs::Busy {
                name: "syspolicyd".to_owned(),
                cpu_percent: 470,
            }),
            ..running_jobs()
        },
    );
    let (code, out, err) = test.run(&["status"]);
    assert_eq!(code, 1);
    assert_eq!(
        out,
        format!(
            "{STATUS_TOP}daemon: 2 running, 0 waiting, at most 4 at once\nstuck: second (pid 22): no CPU, new process or disk use for 46m\nbusiest process outside the jobs: syspolicyd at 470% CPU\nrunning: second (pid 22): make second\nrunning: first (pid 11): make first\n"
        )
    );
    assert!(err.contains("1 job shows no activity"), "{err}");
    assert_eq!(
        err.lines()
            .filter(|line| line.contains("no activity"))
            .count(),
        1,
        "{err}"
    );
}

#[test]
fn status_prints_a_reclaim_in_progress_after_the_jobs() {
    let test = Test::new(Fixed);
    let now = std::time::SystemTime::now();
    answer_status(
        &test,
        Jobs {
            reclaiming: Some(Box::new(Reclaiming {
                need: "memory=4G".to_owned(),
                since: crumbkit::Age::since(now - Duration::from_secs(185), now),
            })),
            ..running_jobs()
        },
    );
    let (_, out, _) = test.run(&["status"]);
    assert!(
        out.ends_with("\nreclaiming: memory=4G, started 3m ago\n"),
        "{out}"
    );
}

#[test]
fn status_json_holds_the_reclaim_in_progress() {
    let test = Test::new(Fixed);
    let now = std::time::SystemTime::now();
    answer_status(
        &test,
        Jobs {
            reclaiming: Some(Box::new(Reclaiming {
                need: "memory=4G".to_owned(),
                since: crumbkit::Age::since(now - Duration::from_secs(185), now),
            })),
            ..running_jobs()
        },
    );
    let (_, json, _) = test.run(&["status", "--json"]);
    let value: serde_json::Value = serde_json::from_str(&json).expect("JSON");
    let shown = &value["data"]["daemon"]["reclaiming"];
    assert_eq!(shown["need"], "memory=4G");
    assert!(shown["since"].is_string(), "{shown}");
}

#[test]
fn status_with_no_stuck_job_exits_0() {
    let test = Test::new(Fixed);
    answer_status(&test, running_jobs());
    let (code, out, err) = test.run(&["status"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!(
            "{STATUS_TOP}daemon: 2 running, 0 waiting, at most 4 at once\nrunning: first (pid 11): make first\nrunning: second (pid 22): make second\n"
        )
    );
    assert_eq!(err, "");
}

#[test]
fn status_says_when_no_process_outside_the_jobs_is_busy() {
    let test = Test::new(Fixed);
    answer_status(
        &test,
        Jobs {
            stuck: vec![StuckJob {
                label: "first".to_owned(),
                pid: 11,
                quiet_seconds: 6 * 60,
            }],
            busiest: Some(OutsideJobs::Quiet),
            ..running_jobs()
        },
    );
    let (code, out, _) = test.run(&["status"]);
    assert_eq!(code, 1);
    assert!(
        out.contains("\nno process outside the jobs is using more than 50% CPU\n"),
        "{out}"
    );
}

#[test]
fn status_exits_1_when_the_jobs_processes_cannot_be_read() {
    let test = Test::new(Fixed);
    answer_status(
        &test,
        Jobs {
            unreadable: true,
            ..running_jobs()
        },
    );
    let (code, out, err) = test.run(&["status"]);
    assert_eq!(code, 1);
    assert!(
        out.contains("deadband could not read the jobs' processes, so it cannot say whether any job is stuck\n"),
        "{out}"
    );
    assert!(err.contains("could not read the jobs' processes"), "{err}");
}

#[test]
fn status_as_json_lists_the_stuck_job_and_its_failure_code() {
    let test = Test::new(Fixed);
    answer_status(
        &test,
        Jobs {
            stuck: vec![StuckJob {
                label: "second".to_owned(),
                pid: 22,
                quiet_seconds: 120,
            }],
            ..running_jobs()
        },
    );
    let (code, out, _) = test.run(&["status", "--json"]);
    assert_eq!(code, 1);
    let value: serde_json::Value = serde_json::from_str(&out).expect("JSON");
    assert_eq!(value["data"]["daemon"]["stuck"][0]["label"], "second");
    assert_eq!(value["failures"][0]["code"], "deadband::job::stuck");
}

#[test]
fn run_with_json_names_the_program_that_was_not_found_in_its_fields() {
    let test = Test::new(Fixed);
    let (code, out, err) = test.run(&["run", "--json", "--", "deadband-test-no-such-program"]);
    assert_eq!(code, EXIT_NOT_FOUND);
    let printed = out + &err;
    assert!(
        printed.contains(r#""fields":{"program":"deadband-test-no-such-program"}"#),
        "{printed}"
    );
    assert!(
        printed.contains(r#""recovery":{"kind":"hint""#),
        "{printed}"
    );
}

#[test]
fn commands_list_lists_each_command_of_deadband() {
    let (code, out, err) = Test::new(Fixed).run(&["commands", "list", "--json"]);
    assert_eq!(code, 0, "{err}");
    for command in ["deadband status", "deadband run", "deadband daemon"] {
        assert!(out.contains(command), "{command}: {out}");
    }
}

#[test]
fn commands_list_refuses_a_path_that_names_no_command() {
    let (code, _, err) = Test::new(Fixed).run(&["commands", "list", "no-such-command"]);
    assert_ne!(code, 0, "{err}");
}

#[test]
fn schemas_list_lists_the_status_document_and_the_config_file() {
    let (code, out, err) = Test::new(Fixed).run(&["schemas", "list"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("deadband/status/v2"), "{out}");
    assert!(out.contains("deadband/config/v1"), "{out}");
}

#[test]
fn schemas_show_prints_the_keys_of_the_config_file() {
    let (code, out, err) = Test::new(Fixed).run(&["schemas", "show", "deadband/config/v1"]);
    assert_eq!(code, 0, "{err}");
    for key in ["reclaim_after", "reclaim", "stuck_ignore", "on_stuck"] {
        assert!(out.contains(key), "{key}: {out}");
    }
}

#[test]
fn schemas_show_refuses_a_document_deadband_does_not_print() {
    let (code, _, err) = Test::new(Fixed).run(&["schemas", "show", "deadband/nothing/v1"]);
    assert_ne!(code, 0, "{err}");
}

#[test]
fn the_exit_codes_topic_says_run_passes_on_the_jobs_exit_code() {
    let (code, out, err) = Test::new(Fixed).run(&["help", "exit-codes"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("`deadband run` passes on the exit code of the job."),
        "{out}"
    );
}

#[test]
fn the_exit_codes_topic_names_exit_4_only_for_the_daemon() {
    let (code, out, err) = Test::new(Fixed).run(&["help", "exit-codes"]);
    assert_eq!(code, 0, "{err}");
    let line = out
        .lines()
        .find(|line| line.trim_start().starts_with("4 "))
        .unwrap_or_else(|| panic!("no line for 4: {out}"));
    assert!(
        line.contains("`deadband daemon` exits 0 with `deadband::daemon::running`"),
        "{out}"
    );
    assert!(line.ends_with("Commands: `deadband daemon`."), "{out}");
}

#[cfg(feature = "tui")]
#[test]
fn the_exit_codes_topic_says_top_exits_130_on_ctrl_c() {
    let (code, out, err) = Test::new(Fixed).run(&["help", "exit-codes"]);
    assert_eq!(code, 0, "{err}");
    let line = out
        .lines()
        .find(|line| line.trim_start().starts_with("130"))
        .unwrap_or_else(|| panic!("no line for 130: {out}"));
    assert!(line.contains("Ctrl-C"), "{out}");
}
