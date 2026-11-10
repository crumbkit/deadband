//! Tests of the stuck rule, its lines and its config keys.

use std::time::{Duration, Instant};

use super::{Change, Config, MAX_PERIODS, Track, active_line, quiet_text, still_line, stuck_line};
use crate::probe::ProcessUse;
use crate::protocol::OutsideJobs;

const AFTER: Duration = Duration::from_secs(300);

fn at(start: Instant, seconds: u64) -> Instant {
    start + Duration::from_secs(seconds)
}

fn process(pid: u32, cpu_ms: u64, disk_bytes: u64) -> ProcessUse {
    ProcessUse {
        pid,
        cpu: Duration::from_millis(cpu_ms),
        disk_bytes,
    }
}

#[test]
fn a_quiet_tree_becomes_stuck_after_the_limit() {
    let start = Instant::now();
    let mut track = Track::new(start);
    let tree = [process(1, 500, 100), process(2, 900, 100)];
    assert_eq!(track.observe(&tree, AFTER, at(start, 15)), None);
    assert_eq!(track.observe(&tree, AFTER, at(start, 299)), None);
    assert_eq!(
        track.observe(&tree, AFTER, at(start, 300)),
        Some(Change::Stuck {
            quiet: Duration::from_secs(300)
        })
    );
    assert_eq!(track.quiet(at(start, 360)), Some(Duration::from_secs(360)));
}

#[test]
fn a_tree_that_uses_a_second_of_cpu_is_not_stuck() {
    let start = Instant::now();
    let mut track = Track::new(start);
    track.observe(&[process(1, 0, 0)], AFTER, at(start, 15));
    // The total reaches one second at 290 s, so the timer starts again.
    track.observe(&[process(1, 1000, 0)], AFTER, at(start, 290));
    assert_eq!(
        track.observe(&[process(1, 1000, 0)], AFTER, at(start, 500)),
        None
    );
    assert_eq!(track.quiet(at(start, 500)), None);
}

#[test]
fn cpu_under_a_second_leaves_a_tree_quiet() {
    let start = Instant::now();
    let mut track = Track::new(start);
    track.observe(&[process(1, 0, 0)], AFTER, at(start, 15));
    assert_eq!(
        track.observe(&[process(1, 999, 0)], AFTER, at(start, 300)),
        Some(Change::Stuck {
            quiet: Duration::from_secs(300)
        })
    );
}

#[test]
fn a_mebibyte_of_disk_counts_as_activity() {
    let start = Instant::now();
    let mut track = Track::new(start);
    track.observe(&[process(1, 0, 0)], AFTER, at(start, 15));
    track.observe(&[process(1, 0, 1024 * 1024)], AFTER, at(start, 290));
    assert_eq!(
        track.observe(&[process(1, 0, 1024 * 1024)], AFTER, at(start, 500)),
        None
    );
}

#[test]
fn disk_under_a_mebibyte_leaves_a_tree_quiet() {
    let start = Instant::now();
    let mut track = Track::new(start);
    track.observe(&[process(1, 0, 0)], AFTER, at(start, 15));
    assert!(matches!(
        track.observe(&[process(1, 0, 1024 * 1024 - 1)], AFTER, at(start, 300)),
        Some(Change::Stuck { .. })
    ));
}

#[test]
fn an_unchanged_set_of_processes_stays_quiet() {
    let start = Instant::now();
    let mut track = Track::new(start);
    let tree = [process(1, 0, 0), process(2, 0, 0)];
    track.observe(&tree, AFTER, at(start, 15));
    assert!(matches!(
        track.observe(&tree, AFTER, at(start, 300)),
        Some(Change::Stuck { .. })
    ));
}

#[test]
fn a_changed_set_of_processes_counts_as_activity() {
    let start = Instant::now();
    let mut track = Track::new(start);
    track.observe(&[process(1, 0, 0)], AFTER, at(start, 15));
    track.observe(&[process(1, 0, 0), process(2, 0, 0)], AFTER, at(start, 290));
    assert_eq!(
        track.observe(&[process(1, 0, 0), process(2, 0, 0)], AFTER, at(start, 500)),
        None
    );
}

#[test]
fn a_child_that_exits_cannot_lower_the_total() {
    let start = Instant::now();
    let mut track = Track::new(start);
    track.observe(
        &[process(1, 0, 0), process(2, 700, 0)],
        AFTER,
        at(start, 15),
    );
    // The child's reading goes down, which adds nothing, and the set stays.
    track.observe(
        &[process(1, 600, 0), process(2, 0, 0)],
        AFTER,
        at(start, 30),
    );
    assert!(matches!(
        track.observe(
            &[process(1, 600, 0), process(2, 0, 0)],
            AFTER,
            at(start, 300)
        ),
        Some(Change::Stuck { .. })
    ));
}

#[test]
fn a_child_with_no_readable_usage_counts_as_quiet() {
    // The reader gives a child owned by another user a reading of zero.
    let start = Instant::now();
    let mut track = Track::new(start);
    let tree = [process(1, 0, 0), process(99, 0, 0)];
    track.observe(&tree, AFTER, at(start, 15));
    assert!(matches!(
        track.observe(&tree, AFTER, at(start, 300)),
        Some(Change::Stuck { .. })
    ));
}

#[test]
fn a_stuck_job_is_reminded_every_ten_minutes_and_recovers() {
    let start = Instant::now();
    let mut track = Track::new(start);
    let tree = [process(1, 0, 0)];
    track.observe(&tree, AFTER, at(start, 15));
    assert!(track.observe(&tree, AFTER, at(start, 300)).is_some());
    assert_eq!(track.observe(&tree, AFTER, at(start, 899)), None);
    assert_eq!(
        track.observe(&tree, AFTER, at(start, 900)),
        Some(Change::StillStuck {
            quiet: Duration::from_secs(900)
        })
    );
    assert_eq!(
        track.observe(&[process(1, 2000, 0)], AFTER, at(start, 1000)),
        Some(Change::Active {
            quiet: Duration::from_secs(1000)
        })
    );
    assert_eq!(track.quiet(at(start, 1001)), None);
    assert_eq!(
        track.periods(),
        vec![super::Period {
            since: start,
            until: Some(at(start, 1000))
        }]
    );
}

#[test]
fn a_fifth_period_evicts_the_oldest() {
    let start = Instant::now();
    let mut track = Track::new(start);
    track.observe(&[process(1, 0, 0)], AFTER, at(start, 1));
    let mut cpu = 0;
    let mut now = 1;
    for _ in 0..=MAX_PERIODS {
        now += 400;
        track.observe(&[process(1, cpu, 0)], AFTER, at(start, now));
        cpu += 2000;
        now += 1;
        track.observe(&[process(1, cpu, 0)], AFTER, at(start, now));
    }
    let periods = track.periods();
    assert_eq!(periods.len(), MAX_PERIODS);
    assert!(periods.iter().all(|period| period.since > start));
}

#[test]
fn quiet_text_uses_the_largest_units() {
    assert_eq!(quiet_text(Duration::from_secs(45)), "45s");
    assert_eq!(quiet_text(Duration::from_secs(46 * 60 + 30)), "46m");
    assert_eq!(quiet_text(Duration::from_secs(65 * 60)), "1h 5m");
}

fn busy() -> OutsideJobs {
    OutsideJobs::Busy {
        name: "syspolicyd".to_owned(),
        cpu_percent: 470,
    }
}

#[test]
fn the_lines_match_the_signed_off_screens() {
    let five = Duration::from_secs(300);
    let fifteen = Duration::from_secs(900);
    assert_eq!(
        stuck_line(five, Some(&busy())),
        "the job has shown no CPU use, new process or disk use for 5m; deadband leaves it running; the busiest process outside the jobs is syspolicyd at 470% CPU"
    );
    assert_eq!(
        stuck_line(five, Some(&OutsideJobs::Quiet)),
        "the job has shown no CPU use, new process or disk use for 5m; deadband leaves it running; no process outside the jobs is using more than 50% CPU"
    );
    assert_eq!(
        stuck_line(five, None),
        "the job has shown no CPU use, new process or disk use for 5m; deadband leaves it running"
    );
    assert_eq!(
        still_line(fifteen, Some(&busy())),
        "the job has still shown no activity after 15m; the busiest process outside the jobs is syspolicyd at 470% CPU"
    );
    assert_eq!(
        still_line(fifteen, None),
        "the job has still shown no activity after 15m"
    );
    assert_eq!(
        active_line(Duration::from_secs(46 * 60)),
        "the job is active again after 46m with no activity"
    );
}

#[test]
fn the_keys_parse_with_their_defaults() {
    let config = Config::parse(None, &[], None).expect("no keys are valid");
    assert_eq!(config, Config::default());
    assert_eq!(config.after, Some(Duration::from_secs(300)));
}

#[test]
fn stuck_after_reads_an_age_and_off() {
    let config = Config::parse(Some("10m"), &[], None).expect("an age is valid");
    assert_eq!(config.after, Some(Duration::from_secs(600)));
    let off = Config::parse(Some("off"), &[], None).expect("off is valid");
    assert_eq!(off.after, None);
}

#[test]
fn stuck_after_under_a_minute_is_refused() {
    assert!(Config::parse(Some("59s"), &[], None).is_err());
    assert!(Config::parse(Some("soon"), &[], None).is_err());
}

#[test]
fn a_job_that_matches_stuck_ignore_is_ignored() {
    let config = Config::parse(None, &["^docker ".to_owned()], None).expect("valid");
    assert!(config.ignores("docker compose up --wait"));
}

#[test]
fn a_job_that_matches_no_stuck_ignore_is_not_ignored() {
    let config = Config::parse(None, &["^docker ".to_owned()], None).expect("valid");
    assert!(!config.ignores("cargo test"));
}

#[test]
fn an_invalid_stuck_ignore_is_refused() {
    assert!(Config::parse(None, &["(".to_owned()], None).is_err());
}

#[test]
fn an_empty_on_stuck_is_refused() {
    assert!(Config::parse(None, &[], Some(Vec::new())).is_err());
    let config = Config::parse(None, &[], Some(vec!["notify".to_owned()])).expect("valid");
    assert_eq!(config.on_stuck, vec!["notify".to_owned()]);
}
