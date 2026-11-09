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
use crate::admit::{GIB, Lane};
use crate::daemon::{Log, Settings, bind, serve};
use crate::environment::Environment;
use crate::probe::{Disk, Pressure, Probe, Sample, Unreadable};
use crate::protocol::{Exit, Job, JobState, Reply, Request, read_line, write_line};
use crate::testing::{TestSocket, socket_path};

mod reclaim;

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
             running: build (pid 5): make build\n\
             waiting: tests (pid 5): make tests\n"
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
    assert!(out.contains(r#""schema":"deadband/status/v1""#), "{out}");
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
fn daemon_refuses_to_start_while_another_daemon_answers_on_its_socket() {
    let test = Test::new(Fixed);
    test.start_daemon(1);
    let (code, out, err) = test.run(&["daemon"]);
    assert_eq!(code, EXIT_DAEMON);
    assert_eq!(out, "");
    assert!(err.contains("code:  deadband::daemon::running"), "{err}");
}
