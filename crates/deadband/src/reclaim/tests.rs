//! Tests of the reclaim config, the rule that says when to reclaim, and the
//! reading of a reclaimer's report.

use std::time::{Duration, Instant};

use super::{Config, Reclaimer, Report, Resource, Watch, size_bytes};

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
    watch.ended(ended);
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
