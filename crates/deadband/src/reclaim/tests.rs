//! Tests of the reclaim config, the rule that says when to reclaim, and the
//! reading of a reclaimer's report.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process, test_kill_process};

use super::{
    Config, Kept, Reclaimed, Reclaimer, Report, Resource, ShownAge, Watch, parse_age, reclaim,
    size_bytes,
};
use crate::admit::GIB;
use crate::daemon::Log;
use crate::job::CommandLine;

const AFTER: Duration = Duration::from_secs(60);

fn one_reclaimer() -> Config {
    Config {
        reclaimers: vec![Reclaimer {
            name: "workspaces".to_owned(),
            command: CommandLine::of(&["tool", "reclaim"]),
        }],
        ..Config::default()
    }
}

#[test]
fn an_empty_config_runs_no_reclaimer_and_keeps_the_defaults() {
    let config = Config::parse("").expect("an empty file parses");
    assert_eq!(config, Config::default());
    assert!(config.reclaimers.is_empty());
    assert_eq!(config.after, Duration::from_secs(60));
    assert_eq!(config.timeout, Duration::from_secs(300));
    assert_eq!(config.memory, "4G");
    assert_eq!(config.disk, "20G");
}

#[test]
fn a_config_names_its_reclaimers_and_overrides_each_default() {
    let text = r#"
reclaim_after = "2m"
reclaim_timeout = "30s"
reclaim_memory = "8G"
reclaim_disk = "50G"
warm_dirs = ["target", "build/out"]
kind_tools = ["cargo", "make"]

[[reclaim]]
name = "workspaces"
command = ["tool", "reclaim", "--json"]
"#;
    let config = Config::parse(text).expect("the config parses");
    assert_eq!(
        config,
        Config {
            reclaimers: vec![Reclaimer {
                name: "workspaces".to_owned(),
                command: CommandLine::of(&["tool", "reclaim", "--json"]),
            }],
            after: Duration::from_secs(120),
            timeout: Duration::from_secs(30),
            stuck: crate::stuck::Config::default(),
            memory: "8G".to_owned(),
            disk: "50G".to_owned(),
            warm_dirs: vec!["target".to_owned(), "build/out".to_owned()],
            kind_tools: vec!["cargo".to_owned(), "make".to_owned()],
        }
    );
}

#[test]
fn a_config_with_an_unknown_key_is_refused_in_one_line_that_names_the_key() {
    let error = Config::parse("reclaim_aftr = \"60s\"\n").expect_err("an unknown key is refused");
    assert!(error.contains("reclaim_aftr"), "{error}");
    assert!(!error.contains('\n'), "{error}");
}

#[test]
fn a_config_that_is_not_toml_is_refused_in_one_line_that_names_the_line_and_column() {
    let error = Config::parse("reclaim_after = \"60s\"\nreclaim_memory = \n")
        .expect_err("a key without a value is refused");
    assert!(error.contains("line 2 column"), "{error}");
    assert!(!error.contains('\n'), "{error}");
    let error = Config::parse("reclaim_aftr = \"60s\"\n").expect_err("an unknown key is refused");
    assert!(error.contains("line 1 column"), "{error}");
}

#[test]
fn a_config_refuses_a_reclaimer_without_a_command_and_a_size_without_a_number() {
    let empty = "[[reclaim]]\nname = \"x\"\ncommand = []\n";
    let error = Config::parse(empty).expect_err("an empty command is refused");
    assert!(error.contains("x"), "{error}");
    let error = Config::parse("reclaim_memory = \"lots\"\n").expect_err("a bad size is refused");
    assert!(error.contains("lots"), "{error}");
    let error = Config::parse("reclaim_after = \"soon\"\n").expect_err("a bad age is refused");
    assert!(error.contains("soon"), "{error}");
}

#[test]
fn a_missing_config_file_runs_no_reclaimer() {
    let path = std::env::temp_dir().join("deadband-no-such-config.toml");
    assert_eq!(Config::load(&path), Ok(Config::default()));
}

#[test]
fn a_size_counts_in_powers_of_1024() {
    assert_eq!(size_bytes("512"), Some(512));
    assert_eq!(size_bytes("4K"), Some(4 * 1024));
    assert_eq!(size_bytes("4G"), Some(4 * 1024 * 1024 * 1024));
    assert_eq!(size_bytes("1T"), Some(1024 * 1024 * 1024 * 1024));
    assert_eq!(size_bytes("4GB"), None);
    assert_eq!(size_bytes("G"), None);
}

#[test]
fn memory_pressure_asks_for_memory_only_once_it_has_lasted_reclaim_after() {
    let config = one_reclaimer();
    let mut watch = Watch::default();
    let start = Instant::now();
    assert_eq!(watch.observe(&config, true, false, start), None);
    assert_eq!(watch.observe(&config, true, false, start + AFTER / 2), None);
    assert_eq!(
        watch.observe(&config, true, false, start + AFTER),
        Some(Resource::Memory)
    );
}

#[test]
fn pressure_that_falls_back_to_normal_starts_the_wait_again() {
    let config = one_reclaimer();
    let mut watch = Watch::default();
    let start = Instant::now();
    assert_eq!(watch.observe(&config, true, false, start), None);
    assert_eq!(
        watch.observe(&config, false, false, start + AFTER / 2),
        None
    );
    assert_eq!(watch.observe(&config, true, false, start + AFTER), None);
    assert_eq!(
        watch.observe(&config, true, false, start + AFTER * 2),
        Some(Resource::Memory)
    );
}

#[test]
fn critically_low_disk_asks_for_disk_at_once_and_before_memory() {
    let config = one_reclaimer();
    let mut watch = Watch::default();
    let start = Instant::now();
    assert_eq!(watch.observe(&config, true, false, start), None);
    assert_eq!(
        watch.observe(&config, true, true, start + AFTER),
        Some(Resource::Disk)
    );
}

#[test]
fn no_reclaim_starts_while_one_runs_or_within_reclaim_after_of_its_end() {
    let config = one_reclaimer();
    let mut watch = Watch::default();
    let start = Instant::now();
    assert_eq!(
        watch.observe(&config, false, true, start),
        Some(Resource::Disk)
    );
    assert_eq!(watch.observe(&config, false, true, start + AFTER), None);
    let ended = start + AFTER * 2;
    watch.ended(1, ended);
    assert_eq!(watch.observe(&config, false, true, ended + AFTER / 2), None);
    assert_eq!(
        watch.observe(&config, false, true, ended + AFTER),
        Some(Resource::Disk)
    );
}

#[test]
fn a_skipped_reclaim_rests_exactly_reclaim_after_even_after_a_fruitless_one() {
    let config = one_reclaimer();
    let mut watch = Watch::default();
    let start = Instant::now();
    assert_eq!(
        watch.observe(&config, false, true, start),
        Some(Resource::Disk)
    );
    watch.ended(0, start);
    let second = start + AFTER * 2;
    assert_eq!(
        watch.observe(&config, false, true, second),
        Some(Resource::Disk)
    );
    watch.skipped(second);
    assert_eq!(
        watch.observe(&config, false, true, second + AFTER / 2),
        None
    );
    assert_eq!(
        watch.observe(&config, false, true, second + AFTER),
        Some(Resource::Disk)
    );
}

#[test]
fn a_reclaim_skipped_for_a_held_lock_rests_reclaim_after_and_is_not_counted_as_fruitless() {
    let config = one_reclaimer();
    let mut watch = Watch::default();
    let start = Instant::now();
    assert_eq!(
        watch.observe(&config, false, true, start),
        Some(Resource::Disk)
    );
    let skipped = start + AFTER;
    watch.skipped(skipped);
    assert!(!watch.quiet(Resource::Disk));
    assert_eq!(
        watch.observe(&config, false, true, skipped + AFTER / 2),
        None
    );
    assert_eq!(
        watch.observe(&config, false, true, skipped + AFTER),
        Some(Resource::Disk)
    );
}

#[test]
fn with_no_reclaimer_configured_nothing_is_asked() {
    let config = Config::default();
    let mut watch = Watch::default();
    let start = Instant::now();
    assert_eq!(watch.observe(&config, true, true, start), None);
    assert_eq!(watch.observe(&config, true, true, start + AFTER * 2), None);
}

#[test]
fn a_report_adds_up_the_freed_bytes_and_names_the_items_that_free_any() {
    let stdout = br#"{"schema":"x/reclaim/v1","dry_run":true,"data":[
        {"name":"a","state":"would_stop","freed":1024},
        {"name":"b","state":"kept","freed":0},
        {"name":"c","state":"would_stop","freed":2048}
    ],"failures":[]}"#;
    assert_eq!(
        Report::parse(stdout),
        Ok(Report {
            freed_bytes: 3072,
            items: vec!["a".to_owned(), "c".to_owned()],
            kept: vec![Kept {
                name: "b".to_owned(),
                why: String::new(),
            }],
        })
    );
}

#[test]
fn a_report_names_each_kept_item_with_its_reason() {
    let stdout = br#"{"data":[
        {"name":"a","state":"would_stop","freed":1024,"reason":"idle"},
        {"name":"b","state":"kept","freed":0,"reason":"a job waits"},
        {"name":"c","state":"kept","freed":0},
        {"name":"d","freed":0,"reason":"no state"}
    ]}"#;
    let report = Report::parse(stdout).expect("a report");
    assert_eq!(
        report.kept,
        vec![
            Kept {
                name: "b".to_owned(),
                why: "a job waits".to_owned()
            },
            Kept {
                name: "c".to_owned(),
                why: String::new()
            },
        ]
    );
    assert_eq!(report.items, vec!["a".to_owned()]);
}

#[test]
fn a_report_that_is_not_json_is_refused_in_one_line() {
    let error = Report::parse(b"not json\nat all").expect_err("refused");
    assert!(!error.contains('\n'), "{error}");
}

#[test]
fn a_resource_names_itself_as_the_need_argument_does() {
    assert_eq!(Resource::Memory.need(&Config::default()), "memory=4G");
    assert_eq!(Resource::Disk.need(&Config::default()), "disk=20G");
}

#[test]
fn a_config_refuses_a_reclaim_after_under_10_seconds_and_a_zero_timeout() {
    let error = Config::parse("reclaim_after = \"5s\"\n").expect_err("5s is refused");
    assert!(error.contains("10s"), "{error}");
    assert!(Config::parse("reclaim_after = \"10s\"\n").is_ok());
    let error = Config::parse("reclaim_timeout = \"0s\"\n").expect_err("0s is refused");
    assert!(error.contains("reclaim_timeout"), "{error}");
}

#[test]
fn a_disk_need_the_reclaimer_cannot_meet_never_starves_memory_reclaim() {
    let config = one_reclaimer();
    let mut watch = Watch::default();
    let start = Instant::now();
    assert_eq!(
        watch.observe(&config, true, true, start),
        Some(Resource::Disk)
    );
    watch.ended(0, start);
    assert_eq!(
        watch.observe(&config, true, true, start + AFTER),
        Some(Resource::Memory)
    );
}

#[test]
fn a_reclaim_that_frees_nothing_doubles_the_wait_for_that_resource_up_to_30_minutes() {
    let config = one_reclaimer();
    let mut watch = Watch::default();
    let mut now = Instant::now();
    assert_eq!(
        watch.observe(&config, false, true, now),
        Some(Resource::Disk)
    );
    assert!(!watch.quiet(Resource::Disk));
    let mut expected = AFTER;
    for _ in 0..8 {
        watch.ended(0, now);
        assert!(watch.quiet(Resource::Disk));
        expected = (expected * 2).min(Duration::from_secs(30 * 60));
        let early = now + expected - Duration::from_secs(1);
        assert_eq!(watch.observe(&config, false, true, early), None);
        now += expected;
        assert_eq!(
            watch.observe(&config, false, true, now),
            Some(Resource::Disk)
        );
    }
    assert_eq!(expected, Duration::from_secs(30 * 60));
    watch.ended(GIB, now);
    assert!(!watch.quiet(Resource::Disk));
    assert_eq!(
        watch.observe(&config, false, true, now + AFTER),
        Some(Resource::Disk)
    );
}

#[test]
fn the_back_off_ends_when_the_resource_recovers() {
    let config = one_reclaimer();
    let mut watch = Watch::default();
    let now = Instant::now();
    assert_eq!(
        watch.observe(&config, false, true, now),
        Some(Resource::Disk)
    );
    watch.ended(0, now);
    assert_eq!(watch.observe(&config, false, false, now + AFTER / 2), None);
    assert!(!watch.quiet(Resource::Disk));
    assert_eq!(
        watch.observe(&config, false, true, now + AFTER),
        Some(Resource::Disk)
    );
}

#[test]
fn after_a_memory_reclaim_the_next_needs_a_fresh_reclaim_after_of_shortage() {
    let config = one_reclaimer();
    let mut watch = Watch::default();
    let start = Instant::now();
    assert_eq!(watch.observe(&config, true, false, start), None);
    let first = start + AFTER;
    assert_eq!(
        watch.observe(&config, true, false, first),
        Some(Resource::Memory)
    );
    let ended = first + AFTER * 2;
    watch.observe(&config, true, false, ended);
    watch.ended(GIB, ended);
    assert_eq!(watch.observe(&config, true, false, ended + AFTER / 2), None);
    assert_eq!(watch.observe(&config, true, false, ended + AFTER), None);
    assert_eq!(
        watch.observe(&config, true, false, ended + AFTER + AFTER / 2),
        Some(Resource::Memory)
    );
}

/// A directory of reclaimer scripts, removed when the test ends.
struct Scripts(PathBuf);

impl Scripts {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let directory = std::env::temp_dir().join(format!("db-worker-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("the directory is made");
        Self(directory)
    }

    /// Returns a reclaimer named `name` whose script appends its arguments
    /// to `calls`, prints `dry` on a dry run, and otherwise prints `real`
    /// and exits with `code`.
    fn reclaimer(&self, name: &str, dry: u64, real: u64, code: i32) -> Reclaimer {
        let body = format!(
            "echo \"{name} $*\" >> '{calls}'\n\
             case \"$*\" in *--dry-run*) echo '{{\"data\":[{{\"name\":\"w\",\"freed\":{dry}}}]}}'; exit 0;; esac\n\
             echo '{{\"data\":[{{\"name\":\"w\",\"freed\":{real}}}]}}'\nexit {code}\n",
            calls = self.0.join("calls").display()
        );
        self.script(name, &body)
    }

    /// Returns a reclaimer named `name` that runs `body` as a shell script.
    fn script(&self, name: &str, body: &str) -> Reclaimer {
        let script = self.0.join(format!("{name}.sh"));
        #[expect(
            clippy::disallowed_methods,
            reason = "the test writes the script a person would configure"
        )]
        std::fs::write(&script, body).expect("the script writes");
        Reclaimer {
            name: name.to_owned(),
            command: CommandLine::of(&["/bin/sh", &script.display().to_string()]),
        }
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.0.join("calls")).unwrap_or_default()
    }
}

impl Drop for Scripts {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs one memory reclaim with `reclaimers`, and returns the bytes it
/// freed, the records and the log.
fn run(reclaimers: Vec<Reclaimer>, quiet: bool) -> (u64, Vec<Reclaimed>, String) {
    let config = Config {
        reclaimers,
        ..Config::default()
    };
    let (freed, records, written) = run_with(&config, quiet);
    (freed, records, text(&written))
}

/// Runs one memory reclaim with `config`, and returns the bytes it freed,
/// the records and the log, which a reclaimer left running may still write.
fn run_with(config: &Config, quiet: bool) -> (u64, Vec<Reclaimed>, Arc<Mutex<Vec<u8>>>) {
    let written = Arc::new(Mutex::new(Vec::new()));
    let log: Log = written.clone();
    let mut records = Vec::new();
    let freed = reclaim(
        config,
        Resource::Memory,
        quiet,
        &log,
        &mut |each| records.push(each),
        &mut |_, _| {},
    );
    (freed, records, written)
}

/// Returns what `written` holds as text.
fn text(written: &Mutex<Vec<u8>>) -> String {
    String::from_utf8(written.lock().expect("locks").clone()).expect("UTF-8")
}

/// The reclaimers that a test leaves running, read from the files their
/// scripts write their process IDs to. Dropping it ends those not marked
/// as exited, so none outlives the test.
pub(crate) struct Left(Vec<Option<PathBuf>>);

impl Left {
    /// Watches the process IDs that scripts write to `files`.
    pub(crate) fn new(files: &[&Path]) -> Self {
        Self(files.iter().map(|file| Some(file.to_path_buf())).collect())
    }

    /// Returns the process ID in the file at `place`, once a script wrote it
    /// and while it is not marked as exited.
    pub(crate) fn pid(&self, place: usize) -> Option<Pid> {
        let text = std::fs::read_to_string(self.0.get(place)?.as_ref()?).ok()?;
        Pid::from_raw(text.trim().parse().ok()?)
    }

    /// Marks the process at `place` as exited and reaped, so the drop never
    /// signals its process ID, which another process may now hold.
    pub(crate) fn exited(&mut self, place: usize) {
        if let Some(file) = self.0.get_mut(place) {
            *file = None;
        }
    }

    /// Reports whether the process in the file at `place` still runs.
    pub(crate) fn alive(&self, place: usize) -> bool {
        self.pid(place)
            .is_some_and(|pid| test_kill_process(pid).is_ok())
    }
}

impl Drop for Left {
    fn drop(&mut self) {
        for place in 0..self.0.len() {
            if let Some(pid) = self.pid(place) {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the test ends the reclaimer that deadband left running"
                )]
                let _ = kill_process(pid, Signal::KILL);
            }
        }
    }
}

/// The reclaim timeout of the tests of a reclaimer that runs too long, and
/// the ceiling ten times it.
const SHORT: Duration = Duration::from_millis(20);

#[test]
fn a_reclaimer_past_its_ceiling_is_left_running_and_its_reclaim_ends_as_failed() {
    let scripts = Scripts::new();
    let pid = scripts.0.join("pid");
    let hung = scripts.script(
        "hung",
        &format!("echo $$ > '{}'\nexec sleep 30\n", pid.display()),
    );
    let config = Config {
        reclaimers: vec![hung, scripts.reclaimer("b", 5 * GIB, 5 * GIB, 0)],
        timeout: SHORT,
        ..Config::default()
    };
    let begun = Instant::now();
    let (freed, records, written) = run_with(&config, false);
    let left = Left::new(&[&pid]);
    assert!(begun.elapsed() >= SHORT * 10, "{:?}", begun.elapsed());
    assert!(left.alive(0), "deadband never ends a reclaimer");
    assert_eq!(freed, 0);
    assert_eq!(scripts.calls(), "", "no other reclaimer is asked");
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].failed.as_deref(),
        Some("ran past its ceiling of 200ms")
    );
    let pid = left.pid(0).expect("the script wrote its process ID");
    let log = text(&written);
    assert!(
        log.contains(&format!(
            "the reclaimer hung (process {}) ran past its ceiling of 200ms",
            pid.as_raw_nonzero()
        )),
        "{log}"
    );
}

#[test]
fn a_reclaimer_that_exits_after_its_timeout_but_before_its_ceiling_is_waited_for() {
    let scripts = Scripts::new();
    let config = Config {
        reclaimers: vec![scripts.script("slow", "sleep 0.3\n")],
        timeout: Duration::from_millis(100),
        ..Config::default()
    };
    let begun = Instant::now();
    let (_, records, written) = run_with(&config, false);
    assert!(begun.elapsed() >= Duration::from_millis(300));
    assert_eq!(
        records[0].failed.as_deref(),
        Some("ran past its timeout of 100ms")
    );
    assert!(!text(&written).contains("ran past its ceiling"));
}

#[test]
fn a_reclaimer_that_exits_after_the_daemon_left_it_changes_nothing_but_one_log_line() {
    let scripts = Scripts::new();
    let pid = scripts.0.join("pid");
    let late = scripts.script(
        "late",
        &format!(
            "echo $$ > '{}'\nsleep 0.5\necho '{{\"data\":[{{\"name\":\"w\",\"freed\":1}}]}}'\n",
            pid.display()
        ),
    );
    let config = Config {
        reclaimers: vec![late],
        timeout: SHORT,
        ..Config::default()
    };
    let (freed, records, written) = run_with(&config, false);
    let mut left = Left::new(&[&pid]);
    let pid = left.pid(0).expect("the script wrote its process ID");
    let exited = format!(
        "the reclaimer late (process {}) exited with code 0 after the daemon stopped waiting for it, so its output is discarded",
        pid.as_raw_nonzero()
    );
    let begun = Instant::now();
    while !text(&written).contains(&exited) {
        assert!(
            begun.elapsed() < Duration::from_secs(10),
            "{}",
            text(&written)
        );
        std::thread::yield_now();
    }
    left.exited(0);
    assert_eq!(freed, 0);
    assert_eq!(records.len(), 1);
    assert_eq!(text(&written).matches("exited with code").count(), 1);
}

#[test]
fn a_dry_run_that_plans_nothing_skips_the_real_run() {
    let scripts = Scripts::new();
    let (freed, records, log) = run(vec![scripts.reclaimer("a", 0, 5 * GIB, 0)], false);
    assert_eq!(freed, 0);
    assert_eq!(scripts.calls(), "a --need memory=4G --dry-run\n");
    assert_eq!(records.len(), 1);
    assert!(
        log.contains("would free 0 MiB for memory=4G: nothing"),
        "{log}"
    );
}

#[test]
fn each_dry_run_hands_its_plan_to_the_daemon_before_the_real_run() {
    let scripts = Scripts::new();
    let config = Config {
        reclaimers: vec![scripts.reclaimer("a", 5 * GIB, 5 * GIB, 0)],
        ..Config::default()
    };
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut seen = Vec::new();
    reclaim(
        &config,
        Resource::Memory,
        false,
        &log,
        &mut |_| {},
        &mut |reclaimer, plan| {
            seen.push((reclaimer.name.clone(), plan.freed_bytes, scripts.calls()));
        },
    );
    assert_eq!(
        seen,
        vec![(
            "a".to_owned(),
            5 * GIB,
            "a --need memory=4G --dry-run\n".to_owned()
        )]
    );
}

#[test]
fn a_quiet_reclaim_does_not_log_a_plan_of_nothing_again() {
    let scripts = Scripts::new();
    let (_, _, log) = run(vec![scripts.reclaimer("a", 0, 0, 0)], true);
    assert_eq!(log, "");
}

#[test]
fn the_reclaim_stops_once_the_bytes_freed_cover_the_need() {
    let scripts = Scripts::new();
    let reclaimers = vec![
        scripts.reclaimer("a", 5 * GIB, 5 * GIB, 0),
        scripts.reclaimer("b", 5 * GIB, 5 * GIB, 0),
    ];
    let (freed, _, _) = run(reclaimers, false);
    assert_eq!(freed, 5 * GIB);
    assert_eq!(
        scripts.calls(),
        "a --need memory=4G --dry-run\na --need memory=4G\n"
    );
}

#[test]
fn a_real_run_that_exits_with_an_error_still_counts_the_bytes_it_reported() {
    let scripts = Scripts::new();
    let (freed, records, _) = run(vec![scripts.reclaimer("a", 5 * GIB, 2 * GIB, 1)], false);
    assert_eq!(freed, 2 * GIB);
    let last = records.last().expect("a record");
    assert_eq!(last.freed_bytes, Some(2 * GIB));
    assert_eq!(last.failed.as_deref(), Some("it exited with code 1"));
}

#[test]
fn the_config_file_reads_the_stuck_keys() {
    let text =
        "stuck_after = \"10m\"\nstuck_ignore = [\"^docker \"]\non_stuck = [\"tell\", \"me\"]\n";
    let config = Config::parse(text).expect("the config parses");
    assert_eq!(config.stuck.after, Some(Duration::from_secs(600)));
    assert!(config.stuck.ignores("docker compose up"));
    assert_eq!(
        config.stuck.on_stuck,
        Some(CommandLine::of(&["tell", "me"]))
    );
}

#[test]
fn a_config_with_no_stuck_keys_detects_after_five_minutes() {
    let config = Config::parse("").expect("an empty file parses");
    assert_eq!(config.stuck.after, Some(Duration::from_secs(300)));
    assert_eq!(config.stuck.on_stuck, None);
}

#[test]
fn an_invalid_stuck_key_refuses_the_whole_config() {
    assert!(Config::parse("stuck_after = \"1s\"\n").is_err());
    assert!(Config::parse("stuck_ignore = [\"(\"]\n").is_err());
    assert!(Config::parse("on_stuck = []\n").is_err());
}

/// A reclaim that ended at `at`, seen at `now`.
fn reclaimed(at: std::time::SystemTime, now: std::time::SystemTime) -> Reclaimed {
    Reclaimed {
        at: crumbkit::Age::since(at, now),
        reclaimer: "workspaces".to_owned(),
        need: "memory=4G".to_owned(),
        freed_bytes: Some(2 * GIB),
        failed: None,
    }
}

#[test]
fn the_last_reclaim_says_how_long_ago_it_ended_in_text() {
    let at = std::time::UNIX_EPOCH + Duration::from_secs(1_791_374_467);
    let shown = reclaimed(at, at + Duration::from_secs(3 * 60 + 5)).to_string();
    assert_eq!(
        shown,
        "last reclaim: workspaces asked for memory=4G 3m ago, freed 2 GiB"
    );
}

#[test]
fn the_last_reclaim_holds_the_time_it_ended_in_json_and_reads_it_back() {
    let at = std::time::UNIX_EPOCH + Duration::from_secs(1_791_374_467);
    let json = serde_json::to_value(reclaimed(at, at)).expect("the reclaim serializes");
    assert_eq!(json["at"], "2026-10-07T12:01:07.000Z");
    let read: Reclaimed = serde_json::from_value(json).expect("the reclaim reads back");
    assert_eq!(read.at.at(), at);
}

#[test]
fn a_running_reclaim_names_its_need_and_when_it_started_until_it_ends() {
    let config = one_reclaimer();
    let mut watch = Watch::default();
    let start = Instant::now();
    let wall = std::time::SystemTime::now();
    assert_eq!(watch.reclaiming(&config, wall), None);
    watch.observe(&config, true, false, start);
    assert_eq!(
        watch.observe(&config, true, false, start + AFTER),
        Some(Resource::Memory)
    );
    let shown = watch
        .reclaiming(&config, wall + Duration::from_secs(125))
        .expect("a reclaim runs");
    assert_eq!(shown.need, "memory=4G");
    assert_eq!(shown.to_string(), "reclaiming: memory=4G, started 2m ago");
    watch.ended(0, start + AFTER);
    assert_eq!(watch.reclaiming(&config, wall), None);
}

#[test]
fn a_reclaim_time_that_is_not_rfc_3339_is_refused() {
    let at = std::time::UNIX_EPOCH;
    let mut json = serde_json::to_value(reclaimed(at, at)).expect("the reclaim serializes");
    json["at"] = "yesterday".into();
    assert!(serde_json::from_value::<Reclaimed>(json).is_err());
}

#[test]
fn the_next_reclaim_starts_when_memory_has_been_short_for_the_wait() {
    let config = one_reclaimer();
    let mut watch = Watch::default();
    let start = Instant::now();
    assert_eq!(watch.next_in(&config, start), None);
    assert_eq!(watch.observe(&config, true, false, start), None);
    assert_eq!(
        watch.next_in(&config, start + AFTER / 3),
        Some(AFTER * 2 / 3)
    );
    assert_eq!(
        watch.next_in(&config, start + AFTER * 2),
        Some(Duration::ZERO)
    );
}

#[test]
fn no_reclaim_is_due_while_memory_is_not_short_or_a_reclaim_runs() {
    let config = one_reclaimer();
    let mut watch = Watch::default();
    let start = Instant::now();
    assert_eq!(watch.observe(&config, false, false, start), None);
    assert_eq!(watch.next_in(&config, start), None);
    assert_eq!(watch.observe(&config, true, false, start), None);
    assert_eq!(
        watch.observe(&config, true, false, start + AFTER),
        Some(Resource::Memory)
    );
    assert_eq!(watch.next_in(&config, start + AFTER), None);
}

#[test]
fn the_next_reclaim_waits_out_the_rest_after_reclaims_that_freed_nothing() {
    let config = one_reclaimer();
    let mut watch = Watch::default();
    let start = Instant::now();
    watch.observe(&config, true, false, start);
    watch.observe(&config, true, false, start + AFTER);
    watch.ended(0, start + AFTER);
    let later = start + AFTER + Duration::from_secs(10);
    watch.observe(&config, true, false, later);
    // The rest after one empty reclaim is twice the wait, and it began at
    // the reclaim's end, so 110 s remain after 10 s of it.
    assert_eq!(
        watch.next_in(&config, later),
        Some(AFTER * 2 - Duration::from_secs(10))
    );
}

#[test]
fn the_nearer_of_the_memory_and_disk_starts_is_the_next_reclaim() {
    let config = one_reclaimer();
    let mut watch = Watch::default();
    let start = Instant::now();
    assert_eq!(
        watch.observe(&config, false, true, start),
        Some(Resource::Disk)
    );
    watch.ended(0, start);
    let later = start + Duration::from_secs(5);
    watch.observe(&config, true, true, later);
    // Disk rests for twice the wait after one empty reclaim; memory has to
    // stay short for the wait, which ends sooner.
    assert_eq!(watch.next_in(&config, later), Some(AFTER));
}

#[test]
fn every_age_the_docs_and_configs_use_reads_as_the_same_duration() {
    let ages = [
        ("60s", 60),
        ("90s", 90),
        ("10s", 10),
        ("30s", 30),
        ("2m", 2 * 60),
        ("5m", 5 * 60),
        ("10m", 10 * 60),
        ("1h", 60 * 60),
        ("2h", 2 * 60 * 60),
        ("1h 30m", 90 * 60),
        ("1h30m", 90 * 60),
        ("1 hour 30 minutes", 90 * 60),
        ("2 hours", 2 * 60 * 60),
        ("1hr", 60 * 60),
        ("5min", 5 * 60),
        ("5 minutes", 5 * 60),
        ("30sec", 30),
        ("30 seconds", 30),
        ("1d", 24 * 60 * 60),
        ("2 days", 2 * 24 * 60 * 60),
        ("1day 2h", 26 * 60 * 60),
        ("1w", 7 * 24 * 60 * 60),
        (" 5m ", 5 * 60),
        ("0", 0),
    ];
    for (text, seconds) in ages {
        assert_eq!(
            parse_age(text),
            Ok(Duration::from_secs(seconds)),
            "{text:?} reads as {seconds}s"
        );
    }
    assert_eq!(parse_age("100ms"), Ok(Duration::from_millis(100)));
}

#[test]
fn a_word_that_is_not_an_age_is_refused_naming_the_value() {
    for text in ["soon", "5", "-5m", "5m ago", ""] {
        assert!(parse_age(text).is_err(), "{text:?} is refused");
    }
    let error = Config::parse("reclaim_timeout = \"soon\"\n").expect_err("soon is refused");
    assert!(
        error.starts_with("\"soon\" is not an age such as \"60s\": "),
        "{error}"
    );
    assert!(!error.contains('\n'), "{error}");
}

#[test]
fn the_config_reads_each_age_key_in_long_and_short_units() {
    let config = Config::parse("reclaim_after = \"1 minute\"\nreclaim_timeout = \"1h 30m\"\n")
        .expect("the ages parse");
    assert_eq!(config.after, Duration::from_secs(60));
    assert_eq!(config.timeout, Duration::from_secs(90 * 60));
    let config = Config::parse("reclaim_timeout = \"2 days\"\n").expect("days parse");
    assert_eq!(config.timeout, Duration::from_secs(2 * 24 * 60 * 60));
}

#[test]
fn an_age_prints_in_its_largest_units_first() {
    let shown = [
        (Duration::from_secs(60), "1m"),
        (Duration::from_secs(50 * 60), "50m"),
        (Duration::from_secs(90 * 60), "1h 30m"),
        (Duration::from_secs(5 * 60), "5m"),
        (Duration::from_secs(10), "10s"),
        (Duration::from_secs(61), "1m 1s"),
        (Duration::from_secs(8 * 60 * 60 + 20 * 60), "8h 20m"),
        (Duration::from_secs(24 * 60 * 60), "1day"),
        (Duration::from_secs(25 * 60 * 60), "1day 1h"),
        (Duration::from_secs(2 * 24 * 60 * 60 + 60), "2days 1m"),
        (Duration::from_millis(200), "200ms"),
        (Duration::from_millis(1500), "1s 500ms"),
        (Duration::ZERO, "0s"),
    ];
    for (age, text) in shown {
        assert_eq!(ShownAge(age).to_string(), text, "{age:?}");
    }
}

#[test]
fn a_reclaim_after_under_10_seconds_is_refused_with_its_age_as_it_prints() {
    let error = Config::parse("reclaim_after = \"9500ms\"\n").expect_err("9.5s is refused");
    assert_eq!(
        error,
        "reclaim_after is 9s 500ms, under the shortest it accepts, 10s"
    );
}

#[test]
fn a_reclaim_time_reads_in_each_rfc_3339_form_the_daemon_has_written() {
    let at = std::time::UNIX_EPOCH + Duration::from_secs(1_791_374_467);
    let forms = [
        ("2026-10-07T12:01:07.000Z", at),
        ("2026-10-07T12:01:07Z", at),
        ("2026-10-07T12:01:07+00:00", at),
        ("2026-10-07 12:01:07Z", at),
        (
            "2026-10-07T12:01:07.123456789Z",
            at + Duration::from_nanos(123_456_789),
        ),
    ];
    let mut json = serde_json::to_value(reclaimed(at, at)).expect("the reclaim serializes");
    for (text, expected) in forms {
        json["at"] = text.into();
        let read: Reclaimed = serde_json::from_value(json.clone()).expect(text);
        assert_eq!(read.at.at(), expected, "{text}");
    }
}
