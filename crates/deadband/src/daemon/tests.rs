//! Tests of the daemon over real sockets, with a probe whose pressure each
//! test sets.

use std::io::BufReader;
use std::num::NonZeroUsize;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::{Log, Settings, Unbound, bind, default_max_jobs, lock_path, serve};
use crate::probe::{Pressure, Probe, Sample, Unreadable};
use crate::protocol::{Job, Jobs, Reply, Request, read_line, write_line};
use crate::testing::{TestSocket, socket_path};

/// How long a test waits for a reply that it expects.
const REPLY_WITHIN: Duration = Duration::from_secs(10);

/// How often the test daemon samples its probe.
const SAMPLE_EVERY: Duration = Duration::from_millis(10);

/// How long the test daemon waits for a connection's first line.
const FIRST_LINE_WITHIN: Duration = Duration::from_millis(100);

/// The value of a [`Dial`] that makes its probe fail.
const UNREADABLE: u8 = 3;

/// A probe whose pressure the test sets.
#[derive(Clone)]
struct Dial(Arc<AtomicU8>);

impl Dial {
    fn new(pressure: Pressure) -> Self {
        let dial = Self(Arc::new(AtomicU8::new(0)));
        dial.set(pressure);
        dial
    }

    fn set(&self, pressure: Pressure) {
        let value = match pressure {
            Pressure::Normal => 0,
            Pressure::Warn => 1,
            Pressure::Critical => 2,
        };
        self.0.store(value, Ordering::SeqCst);
    }

    fn break_it(&self) {
        self.0.store(UNREADABLE, Ordering::SeqCst);
    }
}

impl Probe for Dial {
    fn sample(&self) -> Result<Sample, Unreadable> {
        let memory_pressure = match self.0.load(Ordering::SeqCst) {
            0 => Pressure::Normal,
            1 => Pressure::Warn,
            2 => Pressure::Critical,
            _ => return Err(Unreadable::Load),
        };
        Ok(Sample {
            load_1m: 1.0,
            memory_pressure,
            swap_used_bytes: 0,
            swap_total_bytes: 0,
        })
    }
}

/// Starts a daemon with `max_jobs` and `dial` on a new socket, and returns
/// the socket and the daemon's log.
fn start(max_jobs: usize, dial: &Dial) -> (TestSocket, Arc<Mutex<Vec<u8>>>) {
    let path = socket_path();
    let bound = bind(&path).expect("the test socket binds");
    let written = Arc::new(Mutex::new(Vec::new()));
    let log: Log = written.clone();
    let settings = Settings {
        max_jobs: NonZeroUsize::new(max_jobs).expect("the test passes a positive maximum"),
        sample_every: SAMPLE_EVERY,
        first_line_within: FIRST_LINE_WITHIN,
    };
    let probe = dial.clone();
    // The closure takes the whole `Bound`, so its lock lives as long as the
    // daemon's thread.
    thread::spawn(move || {
        let bound = bound;
        serve(&bound.listener, probe, settings, &log)
    });
    (path, written)
}

/// A client connection that reads replies within [`REPLY_WITHIN`].
struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Client {
    fn connect(path: &Path) -> Self {
        let stream = UnixStream::connect(path).expect("the daemon accepts");
        stream
            .set_read_timeout(Some(REPLY_WITHIN))
            .expect("the timeout sets");
        let reader = BufReader::new(stream.try_clone().expect("the stream clones"));
        Self { stream, reader }
    }

    fn ask(path: &Path, label: &str) -> Self {
        let mut client = Self::connect(path);
        client.send(&Request::Ask(job(label)));
        client
    }

    fn send(&mut self, request: &Request) {
        write_line(&mut self.stream, request).expect("the request writes");
    }

    fn reply(&mut self) -> Reply {
        read_line(&mut self.reader)
            .expect("the daemon replies in time")
            .expect("the daemon replies before it closes")
    }
}

fn job(label: &str) -> Job {
    Job {
        label: label.to_owned(),
        pid: 7,
        command: format!("make {label}"),
    }
}

fn jobs(path: &Path) -> Jobs {
    let mut client = Client::connect(path);
    client.send(&Request::Status);
    match client.reply() {
        Reply::Jobs(jobs) => jobs,
        other => panic!("the daemon answered status with {other:?}"),
    }
}

#[test]
fn a_job_starts_at_once_when_nothing_runs() {
    let (path, _) = start(1, &Dial::new(Pressure::Critical));
    assert_eq!(Client::ask(&path, "a").reply(), Reply::Granted);
}

#[test]
fn a_second_job_waits_until_the_first_job_says_it_ended() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), Reply::Granted);
    let mut second = Client::ask(&path, "b");
    assert_eq!(second.reply(), Reply::Queued { ahead: 0 });
    first.send(&Request::Done { exit_code: Some(0) });
    assert_eq!(second.reply(), Reply::Granted);
}

#[test]
fn a_waiting_job_starts_when_the_client_of_the_running_job_disconnects() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), Reply::Granted);
    let mut second = Client::ask(&path, "b");
    assert_eq!(second.reply(), Reply::Queued { ahead: 0 });
    drop(first);
    assert_eq!(second.reply(), Reply::Granted);
}

#[test]
fn status_lists_the_running_and_the_waiting_jobs() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), Reply::Granted);
    let mut second = Client::ask(&path, "b");
    assert_eq!(second.reply(), Reply::Queued { ahead: 0 });
    assert_eq!(
        jobs(&path),
        Jobs {
            max_jobs: 1,
            running: vec![job("a")],
            waiting: vec![job("b")],
        }
    );
}

#[test]
fn a_job_waits_under_warn_pressure_and_starts_when_pressure_falls_to_normal() {
    let dial = Dial::new(Pressure::Warn);
    let (path, _) = start(2, &dial);
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), Reply::Granted);
    let mut second = Client::ask(&path, "b");
    assert_eq!(second.reply(), Reply::Queued { ahead: 0 });
    dial.set(Pressure::Normal);
    assert_eq!(second.reply(), Reply::Granted);
}

#[test]
fn every_job_starts_at_once_when_the_probe_fails_and_the_log_says_why() {
    let dial = Dial::new(Pressure::Normal);
    dial.break_it();
    let (path, log) = start(1, &dial);
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), Reply::Granted);
    let mut second = Client::ask(&path, "b");
    assert_eq!(second.reply(), Reply::Granted);
    let log = String::from_utf8(log.lock().expect("the log locks").clone()).expect("UTF-8");
    assert_eq!(
        log,
        "deadband: deadband could not read the 1-minute load average, so every job starts at once until it reads again\n"
    );
}

#[test]
fn bind_refuses_a_socket_that_a_daemon_answers_on() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    assert!(matches!(bind(&path), Err(Unbound::Running { .. })));
}

#[test]
fn bind_replaces_a_socket_file_that_no_daemon_answers_on() {
    let path = socket_path();
    drop(UnixListener::bind(&path).expect("the stale socket binds"));
    assert!(path.exists());
    let bound = bind(&path).expect("bind replaces the stale socket");
    drop(bound);
}

#[test]
fn bind_refuses_a_path_that_is_not_a_socket_and_leaves_it_in_place() {
    let path = socket_path();
    std::fs::create_dir(&*path).expect("the test directory is made");
    assert!(matches!(bind(&path), Err(Unbound::NotSocket { .. })));
    assert!(path.is_dir());
    std::fs::remove_dir(&*path).expect("the test directory is removed");
}

#[test]
fn bind_refuses_while_another_daemon_holds_the_lock_even_without_its_socket() {
    let path = socket_path();
    let first = bind(&path).expect("the first daemon binds");
    std::fs::remove_file(&*path).expect("the socket file is removed");
    assert!(matches!(bind(&path), Err(Unbound::Running { .. })));
    drop(first);
    assert!(lock_path(&path).exists());
    bind(&path).expect("a daemon binds once the lock is free");
}

#[test]
fn the_socket_and_its_lock_file_are_open_to_their_user_only() {
    use std::os::unix::fs::PermissionsExt;
    let path = socket_path();
    let _bound = bind(&path).expect("the test socket binds");
    for file in [path.to_path_buf(), lock_path(&path)] {
        let mode = std::fs::metadata(&file)
            .expect("the file exists")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{} has mode {mode:o}", file.display());
    }
}

#[test]
fn a_waiting_client_that_disconnects_leaves_the_queue_and_the_next_moves_up() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), Reply::Granted);
    let mut second = Client::ask(&path, "b");
    assert_eq!(second.reply(), Reply::Queued { ahead: 0 });
    let mut third = Client::ask(&path, "c");
    assert_eq!(third.reply(), Reply::Queued { ahead: 1 });
    drop(second);
    let mut listed = jobs(&path);
    while listed.waiting.len() > 1 {
        // The daemon removes the job when it reads the closed connection,
        // which happens on its own thread; each status request is an
        // observable round trip.
        listed = jobs(&path);
    }
    assert_eq!(listed.waiting, vec![job("c")]);
    first.send(&Request::Done { exit_code: Some(0) });
    assert_eq!(third.reply(), Reply::Granted);
}

#[test]
fn a_line_the_daemon_cannot_read_after_an_ask_removes_that_job() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), Reply::Granted);
    let mut second = Client::ask(&path, "b");
    assert_eq!(second.reply(), Reply::Queued { ahead: 0 });
    std::io::Write::write_all(&mut first.stream, b"not json\n").expect("the line writes");
    assert_eq!(second.reply(), Reply::Granted);
}

#[test]
fn a_client_that_sends_nothing_is_closed_and_does_not_hold_up_others() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut silent = Client::connect(&path);
    assert_eq!(Client::ask(&path, "a").reply(), Reply::Granted);
    let closed: Option<Reply> = read_line(&mut silent.reader).expect("the daemon closes it");
    assert_eq!(closed, None);
}

#[test]
fn the_default_maximum_is_half_the_cores_and_at_least_one() {
    assert_eq!(default_max_jobs(NonZeroUsize::new(10)).get(), 5);
    assert_eq!(default_max_jobs(NonZeroUsize::new(1)).get(), 1);
    assert_eq!(default_max_jobs(None).get(), 1);
}
