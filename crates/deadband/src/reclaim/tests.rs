//! Tests of the reclaim config, the rule that says when to reclaim, and the
//! reading of a reclaimer's report.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{Config, Reclaimed, Reclaimer, Report, Resource, Watch, reclaim, size_bytes};
use crate::admit::GIB;
use crate::daemon::Log;

const AFTER: Duration = Duration::from_secs(60);

fn one_reclaimer() -> Config {
    Config {
        reclaimers: vec![Reclaimer {
            name: "workspaces".to_owned(),
            command: vec!["tool".to_owned(), "reclaim".to_owned()],
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
                command: vec!["tool".to_owned(), "reclaim".to_owned(), "--json".to_owned()],
            }],
            after: Duration::from_secs(120),
            timeout: Duration::from_secs(30),
            memory: "8G".to_owned(),
            disk: "50G".to_owned(),
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
        })
    );
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
        let script = self.0.join(format!("{name}.sh"));
        let body = format!(
            "echo \"{name} $*\" >> '{calls}'\n\
             case \"$*\" in *--dry-run*) echo '{{\"data\":[{{\"name\":\"w\",\"freed\":{dry}}}]}}'; exit 0;; esac\n\
             echo '{{\"data\":[{{\"name\":\"w\",\"freed\":{real}}}]}}'\nexit {code}\n",
            calls = self.0.join("calls").display()
        );
        #[expect(
            clippy::disallowed_methods,
            reason = "the test writes the script a person would configure"
        )]
        std::fs::write(&script, body).expect("the script writes");
        Reclaimer {
            name: name.to_owned(),
            command: vec!["/bin/sh".to_owned(), script.display().to_string()],
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
    let written = Arc::new(Mutex::new(Vec::new()));
    let log: Log = written.clone();
    let config = Config {
        reclaimers,
        ..Config::default()
    };
    let mut records = Vec::new();
    let freed = reclaim(&config, Resource::Memory, quiet, &log, &mut |each| {
        records.push(each);
    });
    let log = String::from_utf8(written.lock().expect("locks").clone()).expect("UTF-8");
    (freed, records, log)
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
