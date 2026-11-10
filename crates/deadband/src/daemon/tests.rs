//! Tests of the daemon over real sockets, with a probe whose pressure each
//! test sets.

use std::io::BufReader;
use std::num::NonZeroUsize;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::{Log, Settings, Unbound, bind, default_max_jobs, lock_path, serve};
use crate::admit::{GIB, Hold, Lane};
use crate::probe::{Disk, Pressure, Probe, Sample, Unreadable};
use crate::protocol::{Job, JobState, Jobs, Reply, Request, read_line, write_line};
use crate::testing::{TestSocket, socket_path};

mod timeline;
mod watch;

/// How long a test waits for a reply that it expects.
const REPLY_WITHIN: Duration = Duration::from_secs(10);

/// How often the test daemon samples its probe.
const SAMPLE_EVERY: Duration = Duration::from_millis(10);

/// How long the test daemon waits for a connection's first line.
const FIRST_LINE_WITHIN: Duration = Duration::from_millis(100);

/// The value of a [`Dial`] that makes its probe fail.
const UNREADABLE: u8 = 3;

/// The cores of the test daemon's machine.
const CORES: usize = 16;

/// A probe whose pressure and free disk the test sets. Its load is 1.
#[derive(Clone)]
struct Dial(Arc<AtomicU8>, Arc<AtomicBool>);

impl Dial {
    fn new(pressure: Pressure) -> Self {
        let dial = Self(Arc::new(AtomicU8::new(0)), Arc::new(AtomicBool::new(false)));
        dial.set(pressure);
        dial
    }

    /// Makes the disk's free space critically low, or not.
    fn fill_disk(&self, full: bool) {
        self.1.store(full, Ordering::SeqCst);
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

    fn disk(&self, _path: &Path) -> std::io::Result<Disk> {
        let free = if self.1.load(Ordering::SeqCst) {
            5
        } else {
            500
        };
        Ok(Disk {
            free_bytes: free * GIB,
            total_bytes: 1000 * GIB,
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
        cores: NonZeroUsize::new(CORES),
        home: Some(std::env::temp_dir()),
        priority_file: None,
        sample_every: SAMPLE_EVERY,
        first_line_within: FIRST_LINE_WITHIN,
        reclaim: crate::reclaim::Config::default(),
        watching: crate::daemon::Watching::default(),
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
        Self::ask_in(path, label, Lane::Normal)
    }

    fn ask_in(path: &Path, label: &str, lane: Lane) -> Self {
        let mut client = Self::connect(path);
        client.send(&Request::Ask(Job { lane, ..job(label) }));
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
        lane: Lane::Normal,
        watched: false,
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

/// The reply to a job that waits behind `ahead` jobs because the most jobs
/// run.
fn queued(ahead: usize, max_jobs: usize) -> Reply {
    Reply::Queued {
        ahead,
        held: Some(Hold::Full { max_jobs }),
    }
}

/// The grant of a normal-lane job on a daemon of `max_jobs` jobs.
fn granted(max_jobs: usize) -> Reply {
    Reply::Granted {
        jobs: CORES / max_jobs,
    }
}

/// Tells the daemon at `path` that the job `label` already runs.
fn seat(path: &Path, label: &str, claimed: usize, ran_ms: u64) -> Client {
    let mut client = Client::connect(path);
    client.send(&Request::Seat {
        job: job(label),
        jobs: claimed,
        ran_ms,
    });
    client
}

/// Asks the daemon until `count` jobs run. A seat has no reply, and each
/// request is an observable round trip.
fn until_running(path: &Path, count: usize) -> Jobs {
    let begun = Instant::now();
    loop {
        let listed = jobs(path);
        if listed.running.len() == count {
            return listed;
        }
        assert!(begun.elapsed() < REPLY_WITHIN, "{listed:?}");
    }
}

/// Returns how long ago the running job `label` started, as the timeline
/// says.
fn started_ago(path: &Path, label: &str) -> Duration {
    let mut client = Client::connect(path);
    client.send(&Request::Timeline);
    let Reply::Timeline(timeline) = client.reply() else {
        panic!("the daemon did not answer with a timeline");
    };
    timeline
        .jobs
        .iter()
        .find_map(|row| match row.state {
            JobState::Running { started } if row.label.as_str() == label => {
                Some(started.duration())
            }
            _ => None,
        })
        .expect("the timeline shows the job running")
}

#[test]
fn a_seated_job_shows_in_status_and_a_new_job_waits_behind_it() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let _seated = seat(&path, "a", 16, 0);
    let listed = until_running(&path, 1);
    assert_eq!(listed.running, [job("a")]);
    assert_eq!(Client::ask(&path, "b").reply(), queued(0, 1));
}

#[test]
fn a_seated_job_that_closes_its_connection_frees_its_place() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let seated = seat(&path, "a", 16, 0);
    until_running(&path, 1);
    let mut waiting = Client::ask(&path, "b");
    assert_eq!(waiting.reply(), queued(0, 1));
    drop(seated);
    assert_eq!(waiting.reply(), granted(1));
}

#[test]
fn a_seated_job_that_says_it_ended_frees_its_place() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut seated = seat(&path, "a", 16, 0);
    until_running(&path, 1);
    let mut waiting = Client::ask(&path, "b");
    assert_eq!(waiting.reply(), queued(0, 1));
    seated.send(&Request::Done {
        exit_code: Some(0),
        signal: None,
    });
    assert_eq!(waiting.reply(), granted(1));
}

#[test]
fn a_seat_that_claims_huge_threads_counts_as_one_grant() {
    let (path, _) = start(2, &Dial::new(Pressure::Normal));
    let _seated = seat(&path, "a", usize::MAX, 0);
    until_running(&path, 1);
    assert_eq!(Client::ask(&path, "b").reply(), granted(2));
}

#[test]
fn a_seated_job_keeps_the_time_it_has_run() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let _seated = seat(&path, "a", 1, 5000);
    until_running(&path, 1);
    assert!(started_ago(&path, "a") >= Duration::from_secs(5));
}

#[test]
fn a_seat_that_claims_the_longest_time_counts_as_running_and_holds_its_place() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let _seated = seat(&path, "a", 1, u64::MAX);
    until_running(&path, 1);
    assert_eq!(Client::ask(&path, "b").reply(), queued(0, 1));
}

#[test]
fn a_job_starts_at_once_when_nothing_runs() {
    let (path, _) = start(1, &Dial::new(Pressure::Critical));
    assert_eq!(Client::ask(&path, "a").reply(), granted(1));
}

#[test]
fn a_second_job_waits_until_the_first_job_says_it_ended() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), granted(1));
    let mut second = Client::ask(&path, "b");
    assert_eq!(second.reply(), queued(0, 1));
    first.send(&Request::Done {
        exit_code: Some(0),
        signal: None,
    });
    assert_eq!(second.reply(), granted(1));
}

#[test]
fn a_waiting_job_starts_when_the_client_of_the_running_job_disconnects() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), granted(1));
    let mut second = Client::ask(&path, "b");
    assert_eq!(second.reply(), queued(0, 1));
    drop(first);
    assert_eq!(second.reply(), granted(1));
}

#[test]
fn status_lists_the_running_and_the_waiting_jobs() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), granted(1));
    let mut second = Client::ask(&path, "b");
    assert_eq!(second.reply(), queued(0, 1));
    assert_eq!(
        jobs(&path),
        Jobs {
            max_jobs: 1,
            running: vec![job("a")],
            waiting: vec![job("b")],
            held: Some(Hold::Full { max_jobs: 1 }),
            reclaim: None,
            stuck: Vec::new(),
            busiest: None,
            unreadable: false,
        }
    );
}

#[test]
fn a_job_waits_under_warn_pressure_and_starts_when_pressure_falls_to_normal() {
    let dial = Dial::new(Pressure::Warn);
    let (path, _) = start(2, &dial);
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), granted(2));
    let mut second = Client::ask(&path, "b");
    let held = Some(Hold::Memory {
        pressure: Pressure::Warn,
    });
    assert_eq!(second.reply(), Reply::Queued { ahead: 0, held });
    dial.set(Pressure::Normal);
    assert_eq!(second.reply(), granted(2));
}

#[test]
fn status_reports_the_hold_that_follows_the_machine_and_none_once_the_queue_drains() {
    let dial = Dial::new(Pressure::Warn);
    let (path, _) = start(2, &dial);
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), granted(2));
    let mut second = Client::ask(&path, "b");
    let memory = Hold::Memory {
        pressure: Pressure::Warn,
    };
    assert_eq!(
        second.reply(),
        Reply::Queued {
            ahead: 0,
            held: Some(memory)
        }
    );
    assert_eq!(jobs(&path).held, Some(memory));
    dial.set(Pressure::Normal);
    assert_eq!(second.reply(), granted(2));
    let mut third = Client::ask(&path, "c");
    assert_eq!(third.reply(), queued(0, 2));
    assert_eq!(jobs(&path).held, Some(Hold::Full { max_jobs: 2 }));
    first.send(&Request::Done {
        exit_code: Some(0),
        signal: None,
    });
    assert_eq!(third.reply(), granted(2));
    let drained = jobs(&path);
    assert_eq!(drained.held, None);
    assert!(drained.waiting.is_empty(), "{drained:?}");
}

#[test]
fn a_job_waits_while_free_disk_is_critically_low_even_when_nothing_runs_and_starts_when_freed() {
    let dial = Dial::new(Pressure::Normal);
    dial.fill_disk(true);
    let (path, _) = start(2, &dial);
    let mut first = Client::ask(&path, "a");
    assert!(
        matches!(
            first.reply(),
            Reply::Queued {
                ahead: 0,
                held: Some(Hold::Disk { .. })
            }
        ),
        "the low disk holds the first job"
    );
    dial.fill_disk(false);
    assert_eq!(first.reply(), granted(2));
}

#[test]
fn a_second_job_starts_at_once_while_free_disk_is_ample() {
    let (path, _) = start(2, &Dial::new(Pressure::Normal));
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), granted(2));
    assert_eq!(Client::ask(&path, "b").reply(), granted(2));
}

#[test]
fn a_fast_job_gets_one_more_thread_than_a_normal_job() {
    let (path, _) = start(2, &Dial::new(Pressure::Normal));
    assert_eq!(
        Client::ask_in(&path, "a", Lane::Fast).reply(),
        Reply::Granted { jobs: 9 }
    );
}

#[test]
fn with_an_active_priority_file_a_fast_job_takes_the_place_that_normal_jobs_leave_open() {
    let file = std::env::temp_dir().join(format!("db-priority-{}", std::process::id()));
    #[expect(
        clippy::disallowed_methods,
        reason = "the test writes the priority file a person would write"
    )]
    std::fs::write(&file, "urgent\n").expect("the file writes");
    let path = socket_path();
    let bound = bind(&path).expect("the test socket binds");
    let mut settings = Settings::new(
        NonZeroUsize::new(2).expect("positive"),
        NonZeroUsize::new(CORES),
        None,
        Some(file.clone()),
    );
    settings.sample_every = SAMPLE_EVERY;
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let probe = Dial::new(Pressure::Normal);
    thread::spawn(move || {
        let bound = bound;
        serve(&bound.listener, probe, settings, &log)
    });
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), granted(2));
    let mut second = Client::ask(&path, "b");
    assert_eq!(
        second.reply(),
        Reply::Queued {
            ahead: 0,
            held: Some(Hold::Lane { places: 1 })
        }
    );
    let mut fast = Client::ask_in(&path, "fast", Lane::Fast);
    assert_eq!(fast.reply(), Reply::Granted { jobs: 9 });
    let _ = std::fs::remove_file(&file);
}

#[test]
fn a_waiting_fast_job_starts_before_a_normal_job_that_waited_longer() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), granted(1));
    let mut normal = Client::ask(&path, "normal");
    assert_eq!(normal.reply(), queued(0, 1));
    let mut fast = Client::ask_in(&path, "fast", Lane::Fast);
    assert_eq!(fast.reply(), queued(0, 1));
    first.send(&Request::Done {
        exit_code: Some(0),
        signal: None,
    });
    assert_eq!(fast.reply(), Reply::Granted { jobs: 17 });
    fast.send(&Request::Done {
        exit_code: Some(0),
        signal: None,
    });
    assert_eq!(normal.reply(), granted(1));
}

#[test]
fn jobs_start_while_places_are_free_when_the_probe_fails_and_the_log_says_why() {
    let dial = Dial::new(Pressure::Normal);
    dial.break_it();
    let (path, log) = start(2, &dial);
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), granted(2));
    let mut second = Client::ask(&path, "b");
    assert_eq!(second.reply(), granted(2));
    let mut third = Client::ask(&path, "c");
    assert_eq!(third.reply(), queued(0, 2));
    let log = String::from_utf8(log.lock().expect("the log locks").clone()).expect("UTF-8");
    assert_eq!(
        log,
        "deadband: deadband could not read the 1-minute load average, so each job starts once a place is free until it reads again\n"
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
    assert_eq!(first.reply(), granted(1));
    let mut second = Client::ask(&path, "b");
    assert_eq!(second.reply(), queued(0, 1));
    let mut third = Client::ask(&path, "c");
    assert_eq!(third.reply(), queued(1, 1));
    drop(second);
    let mut listed = jobs(&path);
    while listed.waiting.len() > 1 {
        // The daemon removes the job when it reads the closed connection,
        // which happens on its own thread; each status request is an
        // observable round trip.
        listed = jobs(&path);
    }
    assert_eq!(listed.waiting, vec![job("c")]);
    first.send(&Request::Done {
        exit_code: Some(0),
        signal: None,
    });
    assert_eq!(third.reply(), granted(1));
}

#[test]
fn a_line_the_daemon_cannot_read_after_an_ask_removes_that_job() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), granted(1));
    let mut second = Client::ask(&path, "b");
    assert_eq!(second.reply(), queued(0, 1));
    std::io::Write::write_all(&mut first.stream, b"not json\n").expect("the line writes");
    assert_eq!(second.reply(), granted(1));
}

#[test]
fn a_client_that_sends_nothing_is_closed_and_does_not_hold_up_others() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut silent = Client::connect(&path);
    assert_eq!(Client::ask(&path, "a").reply(), granted(1));
    let closed: Option<Reply> = read_line(&mut silent.reader).expect("the daemon closes it");
    assert_eq!(closed, None);
}

#[test]
fn the_default_maximum_is_a_quarter_of_the_cores_rounded_up_and_at_least_one() {
    assert_eq!(default_max_jobs(NonZeroUsize::new(14)).get(), 4);
    assert_eq!(default_max_jobs(NonZeroUsize::new(16)).get(), 4);
    assert_eq!(default_max_jobs(NonZeroUsize::new(10)).get(), 3);
    assert_eq!(default_max_jobs(NonZeroUsize::new(1)).get(), 1);
    assert_eq!(default_max_jobs(None).get(), 1);
}

#[test]
fn a_path_that_is_not_a_socket_is_named_in_the_fields_and_the_hint_says_the_repair() {
    use crumbkit::{Failure, FieldValue, Recovery};

    let path = socket_path();
    std::fs::create_dir(&*path).expect("the test directory is made");
    let failure = bind(&path).expect_err("a directory is not a socket");
    std::fs::remove_dir(&*path).expect("the test directory is removed");
    let fields = failure.fields();
    assert_eq!(fields.len(), 1, "{fields:?}");
    assert_eq!(fields[0].name(), "path");
    let shown = path.to_str().expect("the test path is UTF-8");
    assert_eq!(fields[0].value(), &FieldValue::from(shown));
    assert!(
        matches!(failure.recovery(), Recovery::Hint(ref text) if text.contains("DEADBAND_SOCKET")),
        "{:?}",
        failure.recovery()
    );
}

#[test]
fn a_running_daemon_names_its_path_and_offers_no_next_step() {
    use crumbkit::{Failure, Recovery};

    let path = socket_path();
    let first = bind(&path).expect("the first daemon binds");
    let failure = bind(&path).expect_err("the second daemon is refused");
    drop(first);
    assert_eq!(failure.fields().len(), 1);
    assert!(matches!(failure.recovery(), Recovery::None { .. }));
}
