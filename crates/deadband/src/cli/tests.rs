//! Tests of the `deadband` commands, run against fake probes and real
//! sockets, and of the command's crumbkit conventions.

use std::io::BufReader;
use std::num::NonZeroUsize;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use clap::CommandFactory;
use crumbkit::{Code, Sink};

use super::{
    Cli, EXIT_DAEMON, EXIT_MACHINE, EXIT_NOT_STARTED, GUIDE, JOB_NOT_STARTED, Machine, run_to,
};
use crate::daemon::{DAEMON_RUNNING, Log, SOCKET_UNUSABLE, Settings, bind, serve};
use crate::probe::{MACHINE_UNREADABLE, MACHINE_UNSUPPORTED, Pressure, Probe, Sample, Unreadable};
use crate::protocol::{Job, Reply, Request, read_line, write_line};
use crate::testing::socket_path;

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
}

/// A probe that cannot read the machine.
#[derive(Clone)]
struct Broken;

impl Probe for Broken {
    fn sample(&self) -> Result<Sample, Unreadable> {
        Err(Unreadable::Load)
    }
}

/// A machine with `probe`, a socket that no daemon listens on yet, and a
/// log that the test reads.
struct Test<P> {
    machine: Machine<P>,
    log: Arc<Mutex<Vec<u8>>>,
}

impl<P: Probe + Clone + Send + 'static> Test<P> {
    fn new(probe: P) -> Self {
        let log = Arc::new(Mutex::new(Vec::new()));
        let shared: Log = log.clone();
        let machine = Machine {
            probe,
            socket: socket_path(),
            log: shared,
            cores: NonZeroUsize::new(4),
        };
        Self { machine, log }
    }

    /// Starts a daemon on the machine's socket that runs at most
    /// `max_jobs` jobs at once.
    fn start_daemon(&self, max_jobs: usize) {
        let listener = bind(&self.machine.socket).expect("the test socket binds");
        let probe = self.machine.probe.clone();
        let log = Arc::clone(&self.machine.log);
        let max_jobs = NonZeroUsize::new(max_jobs).expect("positive");
        thread::spawn(move || serve(&listener, probe, Settings::new(max_jobs), &log));
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
    assert_eq!(reply, Reply::Granted);
    let (_second, reply) = ask(&test.machine.socket, "tests");
    assert_eq!(reply, Reply::Queued { ahead: 0 });
    let (code, out, err) = test.run(&["status"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.ends_with(
            "daemon: 1 running, 1 waiting, at most 1 at once\n\
             running: build (pid 5): make build\n\
             waiting: tests (pid 5): make tests\n"
        ),
        "{out}"
    );
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
    let (_next, reply) = ask(&test.machine.socket, "next");
    assert_eq!(reply, Reply::Granted);
}

#[test]
fn run_fails_with_job_not_started_when_the_program_does_not_exist() {
    let test = Test::new(Fixed);
    let (code, out, err) = test.run(&["run", "--", "deadband-test-no-such-program"]);
    assert_eq!(code, EXIT_NOT_STARTED);
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

#[test]
fn the_guide_gives_each_command_an_example_its_effects_and_its_document() {
    crumbkit_test::check_guide(&Cli::command(), &GUIDE, &[]);
    crumbkit_test::check_effects(&Cli::command(), &GUIDE, &[]);
    crumbkit_test::check_document_ids(&Cli::command(), &GUIDE, &[]);
}

#[test]
fn every_document_schema_matches_its_golden_file() {
    crumbkit_test::check_schema_goldens(&Cli::command(), &GUIDE, "tests/golden/schemas");
}

#[test]
fn every_exit_code_is_declared_with_its_failures() {
    crumbkit_test::check_exit_codes(&Cli::command(), &GUIDE, &[]);
}

#[test]
fn every_failure_code_is_distinct_and_printable() {
    let codes: &[&Code] = &[
        &MACHINE_UNREADABLE,
        &MACHINE_UNSUPPORTED,
        &DAEMON_RUNNING,
        &SOCKET_UNUSABLE,
        &JOB_NOT_STARTED,
    ];
    crumbkit_test::check_codes(&GUIDE, &[codes]);
}
