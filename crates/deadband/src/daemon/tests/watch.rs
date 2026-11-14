//! Tests of the watch thread, one for each failure in the plan and a twin
//! that is not flagged for each flagged case.

use std::net::Shutdown;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::super::{Log, Settings, Watching, bind, serve};
use super::timeline::{ended, row, timeline_until};
use super::{Client, REPLY_WITHIN, SAMPLE_EVERY, job, jobs};
use crate::admit::{GIB, Threads};
use crate::probe::{Disk, Pressure, Probe, ProcessUse, Sample, TreeUnreadable, Unreadable};
use crate::protocol::{Ago, Exit, Job, JobState, Jobs, OutsideJobs, Reply, Request};
use crate::reclaim::Config;
use crate::stuck;
use crate::testing::{TestSocket, socket_path};

/// The process ID of the test's watched job.
const ROOT: u32 = 4242;

/// A tree that never changes.
const QUIET: u8 = 0;
/// A tree whose CPU time grows by a second at each read.
const BUSY: u8 = 1;
/// A tree that cannot be read.
const FAILS: u8 = 2;
/// A platform with no reader.
const UNSUPPORTED: u8 = 3;

/// A probe whose tree the test sets, and that counts its reads.
#[derive(Clone)]
struct Trees {
    mode: Arc<AtomicU8>,
    reads: Arc<AtomicUsize>,
    /// How long each read of a tree takes.
    slow: Duration,
}

impl Trees {
    fn new(mode: u8) -> Self {
        Self {
            mode: Arc::new(AtomicU8::new(mode)),
            reads: Arc::new(AtomicUsize::new(0)),
            slow: Duration::ZERO,
        }
    }

    fn set(&self, mode: u8) {
        self.mode.store(mode, Ordering::SeqCst);
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

impl Probe for Trees {
    fn sample(&self) -> Result<Sample, Unreadable> {
        Ok(Sample {
            load_1m: 1.0,
            memory_pressure: Pressure::Normal,
            swap_used_bytes: 0,
            swap_total_bytes: 0,
        })
    }

    fn disk(&self, _path: &Path) -> std::io::Result<Disk> {
        Ok(Disk {
            free_bytes: 500 * GIB,
            total_bytes: 1000 * GIB,
        })
    }

    fn tree(&self, root: u32) -> Result<Vec<ProcessUse>, TreeUnreadable> {
        let reads = self.reads.fetch_add(1, Ordering::SeqCst);
        let begun = Instant::now();
        while begun.elapsed() < self.slow {
            thread::yield_now();
        }
        let seconds = match self.mode.load(Ordering::SeqCst) {
            QUIET => 0,
            BUSY => u64::try_from(reads).expect("the count fits") + 1,
            FAILS => {
                return Err(TreeUnreadable::Failed(std::io::Error::from_raw_os_error(5)));
            }
            _ => return Err(TreeUnreadable::Unsupported),
        };
        Ok(vec![ProcessUse {
            pid: root,
            cpu: Duration::from_secs(seconds),
            disk_bytes: 0,
        }])
    }
}

/// What a test daemon uses.
struct Options {
    stuck: stuck::Config,
    census: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            stuck: stuck::Config {
                after: Some(Duration::from_millis(30)),
                ..stuck::Config::default()
            },
            census: sh("echo '470.0 9 /usr/libexec/syspolicyd'"),
        }
    }
}

fn sh(script: &str) -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()]
}

fn start(probe: &Trees, options: Options) -> (TestSocket, Arc<Mutex<Vec<u8>>>) {
    let path = socket_path();
    let bound = bind(&path).expect("the test socket binds");
    let written = Arc::new(Mutex::new(Vec::new()));
    let log: Log = written.clone();
    let settings = Settings {
        sample_every: SAMPLE_EVERY,
        reclaim: Config {
            stuck: options.stuck,
            ..Config::default()
        },
        watching: Watching {
            every: Duration::from_millis(5),
            census: options.census,
            census_within: Duration::from_secs(5),
            census_every: Duration::from_millis(50),
            on_stuck_within: Duration::from_secs(5),
        },
        ..Settings::new(NonZeroUsize::new(4).expect("positive"), None, None, None)
    };
    let probe = probe.clone();
    thread::spawn(move || {
        let bound = bound;
        serve(&bound.listener, probe, settings, &log)
    });
    (path, written)
}

fn watched(label: &str, pid: u32) -> Job {
    Job {
        pid,
        watched: true,
        ..job(label)
    }
}

fn ask(path: &Path, job: Job) -> Client {
    let mut client = Client::connect(path);
    client.send(&Request::Ask(job));
    assert!(matches!(client.reply(), Reply::Granted { .. }));
    client
}

/// Asks the daemon until `done` accepts its jobs.
fn jobs_until(path: &Path, done: impl Fn(&Jobs) -> bool) -> Jobs {
    let begun = Instant::now();
    loop {
        let answer = jobs(path);
        if done(&answer) {
            return answer;
        }
        assert!(begun.elapsed() < REPLY_WITHIN, "last answer: {answer:?}");
    }
}

/// Asks until the reader has done at least `reads` more reads, so a test
/// that expects nothing has given the thread time to do something.
fn after_reads(probe: &Trees, path: &Path, reads: usize) {
    let target = probe.reads() + reads;
    let begun = Instant::now();
    while probe.reads() < target {
        let _ = jobs(path);
        assert!(
            begun.elapsed() < REPLY_WITHIN,
            "the watch thread stopped reading"
        );
    }
}

fn log_text(written: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8(written.lock().expect("the log locks").clone()).expect("UTF-8")
}

#[test]
fn a_quiet_watched_job_is_listed_as_stuck() {
    let probe = Trees::new(QUIET);
    let (path, _log) = start(&probe, Options::default());
    let _client = ask(&path, watched("quiet", ROOT));
    let answer = jobs_until(&path, |jobs| !jobs.stuck.is_empty());
    assert_eq!(answer.stuck[0].label.as_str(), "quiet");
    assert_eq!(answer.stuck[0].pid, ROOT);
}

#[test]
fn a_watched_job_that_uses_cpu_is_not_listed_as_stuck() {
    let probe = Trees::new(BUSY);
    let (path, _log) = start(&probe, Options::default());
    let _client = ask(&path, watched("busy", ROOT));
    after_reads(&probe, &path, 20);
    assert!(jobs(&path).stuck.is_empty());
}

#[test]
fn a_quiet_job_that_is_not_marked_watched_has_no_reading() {
    let probe = Trees::new(QUIET);
    let (path, _log) = start(&probe, Options::default());
    let _client = ask(&path, job("unwatched"));
    // The status round trips give the watch thread many chances to read.
    for _ in 0..200 {
        let _ = jobs(&path);
    }
    assert_eq!(probe.reads(), 0);
    assert!(jobs(&path).stuck.is_empty());
}

#[test]
fn a_job_that_matches_stuck_ignore_is_not_flagged() {
    let probe = Trees::new(QUIET);
    let mut options = Options::default();
    options.stuck.ignore = vec![regex_lite::Regex::new("^make ignored").expect("valid")];
    let (path, _log) = start(&probe, options);
    let _client = ask(&path, watched("ignored", ROOT));
    for _ in 0..200 {
        let _ = jobs(&path);
    }
    assert_eq!(probe.reads(), 0);
    assert!(jobs(&path).stuck.is_empty());
}

#[test]
fn a_job_that_matches_no_stuck_ignore_is_flagged() {
    let probe = Trees::new(QUIET);
    let mut options = Options::default();
    options.stuck.ignore = vec![regex_lite::Regex::new("^make ignored").expect("valid")];
    let (path, _log) = start(&probe, options);
    let _client = ask(&path, watched("other", ROOT));
    let answer = jobs_until(&path, |jobs| !jobs.stuck.is_empty());
    assert_eq!(answer.stuck[0].label.as_str(), "other");
}

#[test]
fn detection_turned_off_reads_no_tree() {
    let probe = Trees::new(QUIET);
    let mut options = Options::default();
    options.stuck.after = None;
    let (path, _log) = start(&probe, options);
    let _client = ask(&path, watched("off", ROOT));
    for _ in 0..200 {
        let _ = jobs(&path);
    }
    assert_eq!(probe.reads(), 0);
}

#[test]
fn the_busiest_process_is_named_while_a_job_is_stuck() {
    let probe = Trees::new(QUIET);
    let (path, _log) = start(&probe, Options::default());
    let _client = ask(&path, watched("named", ROOT));
    let answer = jobs_until(&path, |jobs| jobs.busiest.is_some());
    assert_eq!(
        answer.busiest,
        Some(OutsideJobs::Busy {
            name: "syspolicyd".to_owned(),
            cpu_percent: 470
        })
    );
}

#[test]
fn a_failed_process_list_names_nothing_and_still_flags_the_job() {
    let probe = Trees::new(QUIET);
    let options = Options {
        census: sh("exit 1"),
        ..Options::default()
    };
    let (path, log) = start(&probe, options);
    let _client = ask(&path, watched("flagged", ROOT));
    let answer = jobs_until(&path, |jobs| !jobs.stuck.is_empty());
    assert_eq!(answer.busiest, None);
    after_reads(&probe, &path, 30);
    let lines = log_text(&log);
    assert_eq!(
        lines.matches("could not name the busiest process").count(),
        1,
        "{lines}"
    );
}

#[test]
fn a_hung_process_list_does_not_stop_the_samples() {
    let probe = Trees::new(QUIET);
    let options = Options {
        census: sh("sleep 2"),
        ..Options::default()
    };
    let (path, _log) = start(&probe, options);
    let _client = ask(&path, watched("hung", ROOT));
    let answer = jobs_until(&path, |jobs| !jobs.stuck.is_empty());
    assert_eq!(answer.busiest, None);
}

#[test]
fn a_failed_tree_read_leaves_the_job_unflagged_and_says_so_once() {
    let probe = Trees::new(FAILS);
    let (path, log) = start(&probe, Options::default());
    let _client = ask(&path, watched("unread", ROOT));
    let answer = jobs_until(&path, |jobs| jobs.unreadable);
    assert!(answer.stuck.is_empty());
    assert_eq!(answer.running.len(), 1);
    after_reads(&probe, &path, 20);
    let lines = log_text(&log);
    assert_eq!(
        lines.matches("could not read the jobs' processes").count(),
        1,
        "{lines}"
    );
}

#[test]
fn a_tree_read_that_works_again_clears_the_failure() {
    let probe = Trees::new(FAILS);
    let (path, _log) = start(&probe, Options::default());
    let _client = ask(&path, watched("recovers", ROOT));
    jobs_until(&path, |jobs| jobs.unreadable);
    probe.set(BUSY);
    jobs_until(&path, |jobs| !jobs.unreadable);
}

#[test]
fn a_platform_with_no_reader_flags_nothing_and_logs_nothing() {
    let probe = Trees::new(UNSUPPORTED);
    let (path, log) = start(&probe, Options::default());
    let _client = ask(&path, watched("linux", ROOT));
    after_reads(&probe, &path, 20);
    let answer = jobs(&path);
    assert!(answer.stuck.is_empty());
    assert!(!answer.unreadable);
    assert_eq!(log_text(&log), "");
}

#[test]
fn the_client_is_told_when_its_job_becomes_stuck_and_when_it_is_active() {
    let probe = Trees::new(QUIET);
    let (path, _log) = start(&probe, Options::default());
    let mut client = ask(&path, watched("told", ROOT));
    match client.reply() {
        Reply::Stuck { again, busiest, .. } => {
            assert!(!again);
            assert!(matches!(busiest, Some(OutsideJobs::Busy { .. })));
        }
        other => panic!("the daemon sent {other:?}"),
    }
    probe.set(BUSY);
    assert!(matches!(client.reply(), Reply::Active { .. }));
    jobs_until(&path, |jobs| {
        jobs.stuck.is_empty() && jobs.busiest.is_none()
    });
}

#[test]
fn a_client_that_cannot_take_a_notice_keeps_its_job_listed() {
    let probe = Trees::new(QUIET);
    let (path, _log) = start(&probe, Options::default());
    let client = ask(&path, watched("deaf", ROOT));
    // Closing this side's reading direction makes the daemon's write fail.
    client
        .stream
        .shutdown(Shutdown::Read)
        .expect("the shutdown works");
    let answer = jobs_until(&path, |jobs| !jobs.stuck.is_empty());
    assert_eq!(answer.running.len(), 1);
}

#[test]
fn a_seated_job_is_watched_again() {
    let probe = Trees::new(QUIET);
    let (path, _log) = start(&probe, Options::default());
    let mut client = Client::connect(&path);
    client.send(&Request::Seat {
        job: watched("seated", ROOT),
        threads: Threads::new(4),
        ran: Ago::from_millis(60_000),
    });
    let answer = jobs_until(&path, |jobs| !jobs.stuck.is_empty());
    assert_eq!(answer.stuck[0].label.as_str(), "seated");
}

#[test]
fn a_slow_tree_read_holds_up_no_admission() {
    let mut probe = Trees::new(QUIET);
    probe.slow = Duration::from_millis(300);
    let (path, _log) = start(&probe, Options::default());
    let _first = ask(&path, watched("slow", ROOT));
    let target = probe.reads() + 1;
    let begun = Instant::now();
    while probe.reads() < target {
        let _ = jobs(&path);
        assert!(begun.elapsed() < REPLY_WITHIN);
    }
    let asked = Instant::now();
    let _second = ask(&path, job("second"));
    assert!(
        asked.elapsed() < Duration::from_millis(200),
        "{:?}",
        asked.elapsed()
    );
}

#[test]
fn on_stuck_reads_the_job_as_json_and_runs_once() {
    let directory = std::env::temp_dir().join(format!("db-hook-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("the directory is made");
    let file = directory.join("input");
    let probe = Trees::new(QUIET);
    let mut options = Options::default();
    options.stuck.on_stuck = sh(&format!(
        "cat >> '{}'; echo >> '{}'",
        file.display(),
        file.display()
    ));
    let (path, _log) = start(&probe, options);
    let _client = ask(&path, watched("hooked", ROOT));
    let begun = Instant::now();
    let text = loop {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        if text.ends_with('\n') {
            break text;
        }
        assert!(begun.elapsed() < REPLY_WITHIN);
        let _ = jobs(&path);
    };
    after_reads(&probe, &path, 20);
    let again = std::fs::read_to_string(&file).unwrap_or_default();
    std::fs::remove_dir_all(&directory).expect("the directory goes");
    assert_eq!(text, again);
    assert_eq!(text.lines().count(), 1);
    let value: serde_json::Value = serde_json::from_str(text.trim()).expect("JSON");
    assert_eq!(value["label"], "hooked");
    assert!(value["quiet_seconds"].is_u64());
    assert_eq!(value["busiest"]["name"], "syspolicyd");
}

#[test]
fn a_failing_on_stuck_is_one_log_line_and_detection_goes_on() {
    let probe = Trees::new(QUIET);
    let mut options = Options::default();
    options.stuck.on_stuck = sh("exit 9");
    let (path, log) = start(&probe, options);
    let _client = ask(&path, watched("hook-fails", ROOT));
    let begun = Instant::now();
    while !log_text(&log).contains("the on_stuck command failed") {
        let _ = jobs(&path);
        assert!(begun.elapsed() < REPLY_WITHIN);
    }
    assert_eq!(jobs(&path).stuck.len(), 1);
}

#[test]
fn a_job_with_no_on_stuck_logs_nothing_when_it_becomes_stuck() {
    let probe = Trees::new(QUIET);
    let (path, log) = start(&probe, Options::default());
    let _client = ask(&path, watched("no-hook", ROOT));
    jobs_until(&path, |jobs| !jobs.stuck.is_empty());
    assert!(!log_text(&log).contains("on_stuck"));
}

#[test]
fn an_on_stuck_command_that_cannot_start_is_named_in_the_log_when_a_job_becomes_stuck() {
    let probe = Trees::new(QUIET);
    let mut options = Options::default();
    options.stuck.on_stuck = vec!["/nonexistent/notify".to_owned()];
    let (path, log) = start(&probe, options);
    let _client = ask(&path, watched("hook-missing", ROOT));
    let begun = Instant::now();
    while !log_text(&log).contains("the on_stuck command /nonexistent/notify could not start") {
        let _ = jobs(&path);
        assert!(begun.elapsed() < REPLY_WITHIN, "{}", log_text(&log));
    }
}

fn timeline(path: &Path) -> crate::protocol::Timeline {
    let mut client = Client::connect(path);
    client.send(&Request::Timeline);
    match client.reply() {
        Reply::Timeline(timeline) => timeline,
        other => panic!("the daemon answered timeline with {other:?}"),
    }
}

#[test]
fn the_timeline_keeps_a_stuck_period_after_the_job_ends() {
    let probe = Trees::new(QUIET);
    let (path, _log) = start(&probe, Options::default());
    let mut client = ask(&path, watched("ended", ROOT));
    jobs_until(&path, |jobs| jobs.busiest.is_some());
    let live = timeline(&path);
    assert!(live.jobs[0].is_stuck());
    assert!(live.busiest.is_some());
    client.send(&Request::Done {
        exit_code: Some(0),
        signal: None,
        usage: None,
    });
    let begun = Instant::now();
    let row = loop {
        let row = timeline(&path).jobs.remove(0);
        if matches!(row.state, JobState::Finished { .. }) {
            break row;
        }
        assert!(begun.elapsed() < REPLY_WITHIN);
    };
    assert!(matches!(
        row.state,
        JobState::Finished {
            exit: Exit::Exited { code: 0 },
            ..
        }
    ));
    assert_eq!(row.stuck.len(), 1);
    assert!(row.stuck[0].until.is_some());
    assert!(!row.is_stuck());
}

#[test]
fn the_timeline_of_a_job_that_was_never_stuck_has_no_period() {
    let probe = Trees::new(BUSY);
    let (path, _log) = start(&probe, Options::default());
    let _client = ask(&path, watched("fine", ROOT));
    after_reads(&probe, &path, 20);
    let answer = timeline(&path);
    assert!(answer.jobs[0].stuck.is_empty());
    assert!(!answer.unreadable);
}

#[test]
fn a_job_that_ends_stops_being_read() {
    let probe = Trees::new(BUSY);
    let (path, _log) = start(&probe, Options::default());
    let client = ask(&path, watched("ends", ROOT));
    after_reads(&probe, &path, 3);
    drop(client);
    jobs_until(&path, |jobs| jobs.running.is_empty());
    // One read may already have copied the job before it ended.
    let settled = probe.reads() + 1;
    for _ in 0..200 {
        let _ = jobs(&path);
    }
    assert!(probe.reads() <= settled);
}

#[test]
fn two_watched_jobs_reported_with_one_pid_each_get_their_own_stuck_notice() {
    let probe = Trees::new(QUIET);
    let (path, _log) = start(&probe, Options::default());
    let mut first = ask(&path, watched("first", ROOT));
    let mut second = ask(&path, watched("second", ROOT));
    for client in [&mut first, &mut second] {
        let reply = client.reply();
        assert!(
            matches!(reply, Reply::Stuck { again: false, .. }),
            "{reply:?}"
        );
    }
    let answer = jobs_until(&path, |jobs| jobs.stuck.len() == 2);
    let running: Vec<_> = answer.running.iter().map(|job| job.id).collect();
    let stuck: Vec<_> = answer.stuck.iter().map(|job| job.id).collect();
    assert_eq!(stuck, running);
    assert_ne!(stuck[0], stuck[1]);
}

#[test]
fn a_waiting_job_with_the_same_pid_that_leaves_keeps_the_running_jobs_stuck_period() {
    let probe = Trees::new(QUIET);
    let (path, _log) = start(&probe, Options::default());
    let _runs = ask(&path, watched("runs", ROOT));
    let _others: Vec<Client> = ["b", "c", "d"]
        .into_iter()
        .map(|label| ask(&path, job(label)))
        .collect();
    // A period that began at least 200 ms ago cannot be a new track's,
    // which needs 30 ms of quiet before it opens a period.
    let old = |timeline: &crate::protocol::Timeline| {
        row(timeline, "runs")
            .stuck
            .first()
            .is_some_and(|period| period.since.as_millis() >= 200)
    };
    timeline_until(&path, old);
    let mut leaves = Client::connect(&path);
    leaves.send(&Request::Ask(Job {
        pid: ROOT,
        ..job("leaves")
    }));
    let reply = leaves.reply();
    assert!(matches!(reply, Reply::Queued { .. }), "{reply:?}");
    drop(leaves);
    let after = timeline_until(&path, |timeline| ended(timeline, "leaves"));
    assert!(old(&after), "{after:?}");
    assert!(row(&after, "leaves").stuck.is_empty(), "{after:?}");
}
