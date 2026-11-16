//! Tests of the reclaim history that the timeline carries.

use std::time::{Duration, Instant};

use super::super::reclaims::{MAX_RUNS, REMEMBER, Reclaims, command_label};
use super::timeline::timeline_until;
use super::{Dial, start_with};
use crate::history::tests::{Scratch, write_file};
use crate::job::CommandLine;
use crate::probe::Pressure;
use crate::protocol::{ReclaimRun, ReclaimRunState, ReclaimerRow};
use crate::reclaim::{Config, Kept, Reclaimed, Reclaimer, Report};

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

fn config() -> Config {
    let reclaimer = |name: &str, words: &[&str]| Reclaimer {
        name: name.to_owned(),
        command: CommandLine::of(words),
    };
    Config {
        reclaimers: vec![
            reclaimer("workspaces", &["/home/me/bin/mxv", "reclaim", "--json"]),
            reclaimer("caches", &["/usr/bin/tidy", "reap", "--json"]),
        ],
        ..Config::default()
    }
}

/// Returns the outcome of a reclaimer's run that freed `freed` or failed.
fn outcome(freed: Option<u64>, failed: Option<&str>) -> Reclaimed {
    Reclaimed {
        at: crumbkit::Age::since(std::time::SystemTime::now(), std::time::SystemTime::now()),
        reclaimer: "any".to_owned(),
        need: "memory=4G".to_owned(),
        freed_bytes: freed,
        failed: failed.map(str::to_owned),
    }
}

fn runs<'a>(rows: &'a [ReclaimerRow], name: &str) -> &'a [ReclaimRun] {
    &rows
        .iter()
        .find(|row| row.name.as_str() == name)
        .expect("the row")
        .runs
}

#[test]
fn a_reclaimer_is_labelled_by_its_program_and_its_plain_subcommand_words() {
    let label = |words: &[&str]| command_label(&CommandLine::of(words));
    assert_eq!(
        label(&["/home/me/bin/mxv", "reclaim", "--json"]),
        "mxv reclaim"
    );
    assert_eq!(label(&["tool"]), "tool");
    assert_eq!(label(&["tool", "a", "b", "c"]), "tool a b");
    assert_eq!(label(&["tool", "--json", "reclaim"]), "tool");
    assert_eq!(label(&["tool", "/etc/x"]), "tool");
}

#[test]
fn nothing_is_shown_before_a_reclaim_begins() {
    let history = Reclaims::default();
    assert_eq!(history.rows(&config(), Instant::now()), Vec::new());
    assert_eq!(history.plan(), None);
}

#[test]
fn a_reclaim_that_runs_shows_its_first_reclaimer_as_running_since_the_start() {
    let start = Instant::now();
    let mut history = Reclaims::default();
    history.begin("memory=4G".to_owned(), start);
    let rows = history.rows(&config(), start + secs(38));
    let running = runs(&rows, "workspaces");
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].state, ReclaimRunState::Running);
    assert_eq!(running[0].started.duration(), secs(38));
    assert_eq!(running[0].need.as_str(), "memory=4G");
    assert_eq!(runs(&rows, "caches"), &[]);
    assert_eq!(rows[0].command.as_str(), "mxv reclaim");
}

#[test]
fn each_outcome_ends_the_run_of_the_reclaimer_in_turn_and_starts_the_next() {
    let start = Instant::now();
    let mut history = Reclaims::default();
    history.begin("memory=4G".to_owned(), start);
    history.ended(
        &outcome(None, Some("it exited with code 1")),
        start + secs(5),
    );
    let rows = history.rows(&config(), start + secs(8));
    let first = runs(&rows, "workspaces");
    assert!(matches!(
        &first[0].state,
        ReclaimRunState::Ended { failed: Some(why), .. } if why.as_str() == "it exited with code 1"
    ));
    assert_eq!(first[0].started.duration(), secs(8));
    let second = runs(&rows, "caches");
    assert_eq!(second[0].state, ReclaimRunState::Running);
    assert_eq!(second[0].started.duration(), secs(3));
    history.ended(&outcome(Some(1 << 30), None), start + secs(9));
    history.finish();
    let rows = history.rows(&config(), start + secs(10));
    assert!(matches!(
        runs(&rows, "caches")[0].state,
        ReclaimRunState::Ended {
            freed_bytes: Some(freed),
            ..
        } if freed == 1 << 30
    ));
}

#[test]
fn runs_older_than_ten_minutes_leave_the_rows() {
    let start = Instant::now();
    let mut history = Reclaims::default();
    history.begin("memory=4G".to_owned(), start);
    history.ended(&outcome(Some(1), None), start + secs(1));
    history.finish();
    assert_eq!(
        runs(
            &history.rows(&config(), start + REMEMBER - secs(1)),
            "workspaces"
        )
        .len(),
        1
    );
    assert_eq!(
        history.rows(&config(), start + REMEMBER + secs(1)),
        Vec::new()
    );
}

#[test]
fn the_history_keeps_the_newest_64_runs_and_drops_expired_ones_as_it_records() {
    let start = Instant::now();
    let mut history = Reclaims::default();
    for round in 0..(MAX_RUNS + 6) {
        history.begin("memory=4G".to_owned(), start);
        history.ended(&outcome(Some(1), None), start + secs(round as u64));
        history.finish();
    }
    let rows = history.rows(&config(), start + secs(100));
    assert_eq!(runs(&rows, "workspaces").len(), MAX_RUNS);
    history.begin("memory=4G".to_owned(), start + REMEMBER * 3);
    history.ended(&outcome(Some(1), None), start + REMEMBER * 3);
    let rows = history.rows(&config(), start + REMEMBER * 3);
    assert_eq!(runs(&rows, "workspaces").len(), 1);
}

#[test]
fn a_dry_run_plan_shows_while_the_reclaim_runs_and_ends_with_its_reclaimer() {
    let start = Instant::now();
    let mut history = Reclaims::default();
    let report = Report {
        freed_bytes: 5 << 30,
        items: vec!["triage".to_owned(), "reading-list".to_owned()],
        kept: vec![
            Kept {
                name: "board-sync".to_owned(),
                why: "a job waits".to_owned(),
            },
            Kept {
                name: String::new(),
                why: "no name".to_owned(),
            },
        ],
    };
    let reclaimer = config().reclaimers.remove(0);
    history.planned(&reclaimer, &report);
    assert_eq!(history.plan(), None, "no plan while no reclaim runs");
    history.begin("memory=4G".to_owned(), start);
    history.planned(&reclaimer, &report);
    let plan = history.plan().expect("a plan");
    assert_eq!(plan.command.as_str(), "mxv reclaim");
    let stop: Vec<_> = plan.stop.iter().map(|name| name.as_str()).collect();
    assert_eq!(stop, ["triage", "reading-list"]);
    assert_eq!(plan.bytes, 5 << 30);
    assert_eq!(plan.keep.len(), 1);
    assert_eq!(plan.keep[0].name.as_str(), "board-sync");
    assert_eq!(plan.keep[0].why.as_str(), "a job waits");
    assert_eq!(plan.more, 0);
    history.ended(&outcome(Some(1), None), start + secs(3));
    assert_eq!(history.plan(), None);
}

#[test]
fn a_plan_names_at_most_eight_items_on_each_side_and_counts_the_rest() {
    let mut history = Reclaims::default();
    history.begin("memory=4G".to_owned(), Instant::now());
    let report = Report {
        freed_bytes: 1,
        items: (0..11).map(|each| format!("stop-{each}")).collect(),
        kept: (0..9)
            .map(|each| Kept {
                name: format!("keep-{each}"),
                why: String::new(),
            })
            .collect(),
    };
    history.planned(&config().reclaimers[0], &report);
    let plan = history.plan().expect("a plan");
    assert_eq!((plan.stop.len(), plan.keep.len(), plan.more), (8, 8, 4));
}

#[test]
fn a_reclaim_that_never_ran_leaves_no_run() {
    let start = Instant::now();
    let mut history = Reclaims::default();
    history.begin("memory=4G".to_owned(), start);
    history.finish();
    assert_eq!(history.rows(&config(), start), Vec::new());
}

#[test]
fn a_daemon_that_reclaims_shows_the_reclaimer_and_its_ended_run_on_the_timeline() {
    let scratch = Scratch::new();
    let script = scratch.path().join("tool.sh");
    write_file(
        &script,
        "echo '{\"data\":[{\"name\":\"w\",\"freed\":1048576}]}'\n",
    )
    .expect("the script writes");
    let (path, _) = start_with(1, &Dial::new(Pressure::Warn), |settings| {
        settings.reclaim = Config {
            reclaimers: vec![Reclaimer {
                name: "workspaces".to_owned(),
                command: CommandLine::of(&["/bin/sh", &script.display().to_string()]),
            }],
            after: Duration::from_millis(30),
            ..Config::default()
        };
    });
    let shown = timeline_until(&path, |timeline| {
        timeline.reclaimers.first().is_some_and(|row| {
            row.runs
                .iter()
                .any(|run| matches!(run.state, ReclaimRunState::Ended { .. }))
        })
    });
    let row = &shown.reclaimers[0];
    assert_eq!(row.name.as_str(), "workspaces");
    assert_eq!(row.command.as_str(), "sh");
    assert_eq!(row.runs[0].need.as_str(), "memory=4G");
    assert!(matches!(
        row.runs[0].state,
        ReclaimRunState::Ended {
            freed_bytes: Some(1_048_576),
            failed: None,
            ..
        }
    ));
}
