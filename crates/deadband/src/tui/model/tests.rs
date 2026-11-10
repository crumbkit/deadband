//! Tests of the pure model, with a fake clock.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{Absence, Axis, Gate, MAX_SAMPLES, Model, Phase, Rows, Status, WINDOW};
use crate::admit::{Hold, Lane};
use crate::client::Unasked;
use crate::protocol::{Ago, Exit, JobRecord, JobState, Label, Short, StuckPeriod, Timeline};

/// An axis of 60 columns over 600 seconds, so a column is 10 seconds.
const AXIS: Axis = Axis::new(60);

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn ago(n: u64) -> Ago {
    Ago::from_millis(n * 1000)
}

fn record(label: &str, queued: u64, state: JobState) -> JobRecord {
    JobRecord {
        label: Label::new(label),
        workspace: None,
        short: None,
        lane: Lane::Normal,
        queued: ago(queued),
        state,
        stuck: Vec::new(),
    }
}

/// Returns a waiting job labelled `label` in `workspace` whose short
/// command is `short`.
fn shown(label: &str, workspace: Option<&str>, short: &str) -> JobRecord {
    JobRecord {
        workspace: workspace.map(Label::new),
        short: Some(Short::new(short)),
        ..record(label, 30, JobState::Waiting)
    }
}

#[test]
fn a_row_carries_the_workspace_label_and_short_command_of_its_job() {
    let row = only(vec![shown(
        "api-tests",
        Some("triage"),
        "cargo nextest run",
    )]);
    assert_eq!(row.workspace, "triage");
    assert_eq!(row.label, "api-tests");
    assert!(row.shows_label);
    assert_eq!(row.short, "cargo nextest run");
}

#[test]
fn a_row_leaves_out_a_label_that_equals_the_workspace_or_the_first_word_of_its_command() {
    let same = only(vec![shown(
        "reading-list",
        Some("reading-list"),
        "cargo build",
    )]);
    assert!(!same.shows_label);
    let default = only(vec![shown("tool", None, "tool")]);
    assert!(!default.shows_label);
    assert_eq!(default.short, "tool");
}

#[test]
fn a_row_from_an_older_daemon_shows_its_label_with_no_workspace_or_command() {
    let row = only(vec![record("build", 30, JobState::Waiting)]);
    assert_eq!(row.workspace, "");
    assert!(row.shows_label);
    assert_eq!(row.short, "");
}

fn timeline(up: u64, jobs: Vec<JobRecord>) -> Timeline {
    Timeline {
        up: ago(up),
        jobs,
        hidden: 0,
        held: None,
        busiest: None,
        unreadable: false,
    }
}

fn model() -> Model {
    Model::new(NonZeroUsize::new(14))
}

/// Returns the rows of a model that has just been given `jobs`.
fn rows_of(jobs: Vec<JobRecord>, capacity: usize) -> Rows {
    let mut model = model();
    let now = Instant::now();
    model.observe(now, Ok(timeline(1000, jobs)), Some(1.0));
    model.rows(now, AXIS, capacity)
}

fn only(jobs: Vec<JobRecord>) -> super::Row {
    rows_of(jobs, 10).rows.pop().expect("one row")
}

#[test]
fn age_zero_is_the_last_column_and_the_window_is_one_column_before_the_first() {
    assert_eq!(AXIS.column(secs(0)), 59);
    assert_eq!(AXIS.column(secs(9)), 59);
    assert_eq!(AXIS.column(secs(10)), 58);
    assert_eq!(AXIS.column(WINDOW), -1);
}

#[test]
fn the_axis_scales_with_its_width() {
    assert_eq!(Axis::new(84).column(secs(120)), 83 - 16);
    assert_eq!(Axis::new(44).column(secs(120)), 43 - 8);
}

#[test]
fn a_waiting_job_fills_from_its_queue_time_to_now() {
    let row = only(vec![record("a", 100, JobState::Waiting)]);
    assert_eq!(row.waited, 49..60);
    assert!(row.ran.is_empty());
    assert_eq!(row.mark, None);
    assert_eq!(row.status, Status::Waiting(secs(100)));
}

#[test]
fn a_running_job_waits_up_to_its_start_and_runs_to_now() {
    let row = only(vec![record(
        "a",
        100,
        JobState::Running { started: ago(50) },
    )]);
    assert_eq!(row.waited, 49..54);
    assert_eq!(row.ran, 54..60);
    assert_eq!(row.mark, None);
    assert_eq!(row.status, Status::Running(secs(50)));
}

fn finished(exit: Exit) -> JobRecord {
    record(
        "a",
        100,
        JobState::Finished {
            started: ago(50),
            ended: ago(20),
            exit,
        },
    )
}

#[test]
fn a_finished_job_stops_at_its_end_and_the_mark_follows_it() {
    let row = only(vec![finished(Exit::Exited { code: 0 })]);
    assert_eq!(row.waited, 49..54);
    assert_eq!(row.ran, 54..58);
    assert_eq!(row.mark, Some(58));
    assert_eq!(
        row.status,
        Status::Finished {
            exit: Exit::Exited { code: 0 },
            ran: secs(30)
        }
    );
}

#[test]
fn a_job_that_left_waited_until_it_left() {
    let row = only(vec![record("a", 100, JobState::Left { ended: ago(20) })]);
    assert_eq!(row.waited, 49..58);
    assert!(row.ran.is_empty());
    assert_eq!(row.mark, Some(58));
    assert_eq!(row.status, Status::Left { waited: secs(80) });
}

#[test]
fn a_job_older_than_the_window_is_clipped_at_the_left_edge() {
    let row = only(vec![record("a", 2000, JobState::Waiting)]);
    assert_eq!(row.waited, 0..60);
}

#[test]
fn a_job_queued_inside_the_window_is_not_clipped() {
    let row = only(vec![record("a", 580, JobState::Waiting)]);
    assert_eq!(row.waited, 1..60);
}

#[test]
fn a_job_that_ended_before_the_window_is_not_a_row() {
    let rows = rows_of(
        vec![record(
            "a",
            900,
            JobState::Finished {
                started: ago(800),
                ended: ago(600),
                exit: Exit::Lost,
            },
        )],
        10,
    );
    assert!(rows.rows.is_empty());
    assert_eq!(rows.unseen, 0);
}

#[test]
fn a_job_that_ended_just_inside_the_window_is_a_row() {
    let rows = rows_of(
        vec![record(
            "a",
            900,
            JobState::Finished {
                started: ago(800),
                ended: ago(599),
                exit: Exit::Lost,
            },
        )],
        10,
    );
    assert_eq!(rows.rows.len(), 1);
}

#[test]
fn the_rows_run_from_the_oldest_queued_to_the_newest() {
    let rows = rows_of(
        vec![
            record("running", 30, JobState::Running { started: ago(20) }),
            record("waiting", 10, JobState::Waiting),
            finished_at("ended", 100),
        ],
        10,
    );
    let labels: Vec<_> = rows.rows.iter().map(|row| row.label.as_str()).collect();
    assert_eq!(labels, vec!["ended", "running", "waiting"]);
}

fn finished_at(label: &str, queued: u64) -> JobRecord {
    record(
        label,
        queued,
        JobState::Finished {
            started: ago(queued - 1),
            ended: ago(queued - 2),
            exit: Exit::Exited { code: 0 },
        },
    )
}

#[test]
fn rows_beyond_the_screen_keep_the_live_jobs_and_count_the_rest() {
    let rows = rows_of(
        vec![
            record("running", 30, JobState::Running { started: ago(20) }),
            record("waiting", 10, JobState::Waiting),
            finished_at("newest", 100),
            finished_at("oldest", 200),
        ],
        3,
    );
    let labels: Vec<_> = rows.rows.iter().map(|row| row.label.as_str()).collect();
    assert_eq!(labels, vec!["newest", "running", "waiting"]);
    assert_eq!(rows.unseen, 1);
}

#[test]
fn the_rows_that_the_daemon_left_out_count_as_unseen() {
    let mut model = model();
    let now = Instant::now();
    let mut reply = timeline(5, Vec::new());
    reply.hidden = 7;
    model.observe(now, Ok(reply), None);
    assert_eq!(model.rows(now, AXIS, 3).unseen, 7);
}

#[test]
fn a_hidden_count_near_the_limit_saturates() {
    let mut model = model();
    let now = Instant::now();
    let mut reply = timeline(5, vec![record("a", 10, JobState::Waiting); 2]);
    reply.hidden = usize::MAX;
    model.observe(now, Ok(reply), None);
    assert_eq!(model.rows(now, AXIS, 1).unseen, usize::MAX);
}

#[test]
fn ages_keep_growing_between_replies() {
    let mut model = model();
    let now = Instant::now();
    model.observe(
        now,
        Ok(timeline(5, vec![record("a", 100, JobState::Waiting)])),
        None,
    );
    let rows = model.rows(now + secs(5), AXIS, 3);
    assert_eq!(rows.rows[0].status, Status::Waiting(secs(105)));
}

#[test]
fn a_fast_job_is_marked_fast() {
    let mut job = record("a", 10, JobState::Waiting);
    job.lane = Lane::Fast;
    assert!(only(vec![job]).fast);
}

#[test]
fn a_normal_job_is_not_marked_fast() {
    assert!(!only(vec![record("a", 10, JobState::Waiting)]).fast);
}

#[test]
fn each_hold_maps_to_its_place_on_the_strip() {
    let cases = [
        (None, Gate::Open),
        (Some(Hold::Full { max_jobs: 1 }), Gate::Full),
        (Some(Hold::Lane { places: 1 }), Gate::Full),
        (
            Some(Hold::Disk {
                free_bytes: 1,
                total_bytes: 2,
            }),
            Gate::Disk,
        ),
        (
            Some(Hold::Memory {
                pressure: crate::probe::Pressure::Warn,
            }),
            Gate::Memory,
        ),
        (
            Some(Hold::Load {
                projected: 1.0,
                cores: 1,
            }),
            Gate::Load,
        ),
    ];
    for (hold, gate) in cases {
        assert_eq!(Gate::from(hold), gate, "{hold:?}");
    }
}

#[test]
fn the_samples_read_back_by_age_and_a_gap_stays_a_gap() {
    let mut model = model();
    let start = Instant::now();
    model.observe(start, Ok(timeline(1, Vec::new())), Some(2.0));
    model.observe(start + secs(1), Ok(timeline(2, Vec::new())), None);
    model.observe(start + secs(2), Ok(timeline(3, Vec::new())), Some(4.0));
    let now = start + secs(2);
    assert_eq!(model.load_at(now, secs(0)), Some(4.0));
    assert_eq!(model.load_at(now, secs(1)), None);
    assert_eq!(model.load_at(now, secs(2)), Some(2.0));
    assert_eq!(model.gate_at(now, secs(2)), Some(Gate::Open));
    assert_eq!(model.latest_load(), Some(4.0));
}

#[test]
fn the_view_has_no_sample_before_it_opened() {
    let mut model = model();
    let now = Instant::now();
    model.observe(now, Ok(timeline(1, Vec::new())), Some(2.0));
    assert_eq!(model.load_at(now, secs(30)), None);
    assert_eq!(model.gate_at(now, secs(30)), None);
}

#[test]
fn a_poll_with_no_answer_leaves_no_hold_on_the_strip_but_keeps_the_load() {
    let mut model = model();
    let now = Instant::now();
    model.observe(now, Err(not_running()), Some(2.0));
    assert_eq!(model.load_at(now, secs(0)), Some(2.0));
    assert_eq!(model.gate_at(now, secs(0)), None);
}

#[test]
fn the_view_keeps_the_last_six_hundred_samples() {
    let mut model = model();
    let start = Instant::now();
    for n in 0..=u32::try_from(MAX_SAMPLES).expect("fits") {
        model.observe(
            start + secs(u64::from(n)),
            Ok(timeline(1, Vec::new())),
            Some(f64::from(n)),
        );
    }
    assert_eq!(model.sample_count(), MAX_SAMPLES);
}

fn not_running() -> Unasked {
    Unasked::NotRunning {
        path: PathBuf::from("/no"),
    }
}

fn silent() -> Unasked {
    Unasked::Silent {
        waited: Duration::from_millis(500),
    }
}

#[test]
fn before_any_answer_the_view_is_connecting() {
    assert_eq!(
        model().phase(Instant::now(), 80, 16),
        Phase::Connecting(None)
    );
}

#[test]
fn a_reply_makes_the_view_live() {
    let mut model = model();
    let now = Instant::now();
    model.observe(now, Ok(timeline(1, Vec::new())), None);
    assert_eq!(model.phase(now, 80, 16), Phase::Live);
}

#[test]
fn a_terminal_one_column_too_narrow_is_too_small() {
    let mut model = model();
    let now = Instant::now();
    model.observe(now, Ok(timeline(1, Vec::new())), None);
    assert_eq!(
        model.phase(now, 79, 16),
        Phase::TooSmall {
            columns: 79,
            lines: 16
        }
    );
}

#[test]
fn a_terminal_one_line_too_short_is_too_small() {
    assert_eq!(
        model().phase(Instant::now(), 80, 15),
        Phase::TooSmall {
            columns: 80,
            lines: 15
        }
    );
}

#[test]
fn no_daemon_drops_the_rows_and_says_it_is_down() {
    let mut model = model();
    let now = Instant::now();
    model.observe(
        now,
        Ok(timeline(1, vec![record("a", 10, JobState::Waiting)])),
        None,
    );
    model.observe(now + secs(1), Err(not_running()), None);
    assert_eq!(
        model.phase(now + secs(1), 80, 16),
        Phase::DaemonDown(Absence::NotRunning)
    );
    assert!(model.rows(now + secs(1), AXIS, 3).rows.is_empty());
    assert_eq!(model.held(), None);
}

#[test]
fn a_daemon_that_closes_with_no_reply_may_be_older() {
    let mut model = model();
    let now = Instant::now();
    model.observe(now, Err(Unasked::Closed), None);
    assert_eq!(
        model.phase(now, 80, 16),
        Phase::DaemonDown(Absence::MaybeOlder)
    );
}

#[test]
fn a_silent_daemon_keeps_the_last_rows_and_says_how_long() {
    let mut model = model();
    let now = Instant::now();
    model.observe(
        now,
        Ok(timeline(1, vec![record("a", 10, JobState::Waiting)])),
        None,
    );
    model.observe(now + secs(3), Err(silent()), None);
    assert_eq!(
        model.phase(now + secs(3), 80, 16),
        Phase::Stale {
            silent: secs(3),
            why: "the daemon did not answer within 500 ms".to_owned()
        }
    );
    assert_eq!(model.rows(now + secs(3), AXIS, 3).rows.len(), 1);
}

#[test]
fn a_garbled_reply_keeps_the_last_rows_and_gives_the_reason() {
    let mut model = model();
    let now = Instant::now();
    model.observe(
        now,
        Ok(timeline(1, vec![record("a", 10, JobState::Waiting)])),
        None,
    );
    model.observe(
        now + secs(1),
        Err(Unasked::Garbled {
            error: std::io::ErrorKind::InvalidData.into(),
        }),
        None,
    );
    let Phase::Stale { why, .. } = model.phase(now + secs(1), 80, 16) else {
        panic!("the view is stale");
    };
    assert!(
        why.starts_with("the daemon's answer could not be read"),
        "{why}"
    );
    assert_eq!(model.rows(now + secs(1), AXIS, 3).rows.len(), 1);
}

#[test]
fn a_silent_daemon_before_any_reply_is_connecting_with_the_reason() {
    let mut model = model();
    let now = Instant::now();
    model.observe(now, Err(silent()), None);
    assert_eq!(
        model.phase(now, 80, 16),
        Phase::Connecting(Some("the daemon did not answer within 500 ms".to_owned()))
    );
}

#[test]
fn a_shrinking_up_time_shows_a_restart_and_replaces_the_rows() {
    let mut model = model();
    let now = Instant::now();
    model.observe(
        now,
        Ok(timeline(500, vec![record("old", 10, JobState::Waiting)])),
        None,
    );
    model.observe(now + secs(1), Ok(timeline(40, Vec::new())), None);
    assert_eq!(model.restarted(), Some(secs(40)));
    assert!(model.rows(now + secs(1), AXIS, 3).rows.is_empty());
}

#[test]
fn a_growing_up_time_shows_no_restart() {
    let mut model = model();
    let now = Instant::now();
    model.observe(now, Ok(timeline(500, Vec::new())), None);
    model.observe(now + secs(1), Ok(timeline(501, Vec::new())), None);
    assert_eq!(model.restarted(), None);
}

#[test]
fn a_restart_seen_across_a_time_with_no_daemon_still_counts() {
    let mut model = model();
    let now = Instant::now();
    model.observe(now, Ok(timeline(500, Vec::new())), None);
    model.observe(now + secs(1), Err(not_running()), None);
    model.observe(now + secs(2), Ok(timeline(1, Vec::new())), None);
    assert_eq!(model.restarted(), Some(secs(1)));
}

#[test]
fn the_restart_notice_goes_once_the_window_holds_only_the_new_daemon() {
    let mut model = model();
    let now = Instant::now();
    model.observe(now, Ok(timeline(700, Vec::new())), None);
    model.observe(now + secs(1), Ok(timeline(1, Vec::new())), None);
    model.observe(now + secs(2), Ok(timeline(600, Vec::new())), None);
    assert_eq!(model.restarted(), None);
}

fn quiet_record(label: &str, queued: u64, quiet: u64) -> JobRecord {
    JobRecord {
        stuck: vec![StuckPeriod {
            since: ago(quiet),
            until: None,
        }],
        ..record(
            label,
            queued,
            JobState::Running {
                started: ago(queued),
            },
        )
    }
}

#[test]
fn stuck_jobs_come_before_every_other_row_however_small_the_screen() {
    let now = Instant::now();
    let mut model = model();
    let jobs = vec![
        record("old", 500, JobState::Running { started: ago(490) }),
        record("older", 550, JobState::Waiting),
        quiet_record("stuck", 100, 60),
    ];
    model.observe(now, Ok(timeline(50, jobs)), None);
    let rows = model.rows(now, Axis::new(80), 1);
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].label, "stuck");
    assert_eq!(rows.unseen, 2);
}

#[test]
fn rows_with_no_stuck_job_keep_the_oldest_queued_first() {
    let now = Instant::now();
    let mut model = model();
    let jobs = vec![
        record("young", 100, JobState::Running { started: ago(90) }),
        record("older", 550, JobState::Waiting),
    ];
    model.observe(now, Ok(timeline(50, jobs)), None);
    let rows = model.rows(now, Axis::new(80), 2);
    assert_eq!(rows.rows[0].label, "older");
    assert_eq!(rows.rows[1].label, "young");
}

#[test]
fn a_recovered_job_does_not_count_as_stuck() {
    let now = Instant::now();
    let mut model = model();
    let mut job = record("back", 300, JobState::Running { started: ago(300) });
    job.stuck = vec![StuckPeriod {
        since: ago(200),
        until: Some(ago(100)),
    }];
    model.observe(now, Ok(timeline(50, vec![job])), None);
    assert_eq!(model.stuck_count(), 0);
    let rows = model.rows(now, Axis::new(60), 3);
    assert_eq!(rows.rows[0].stuck, vec![39..49]);
    assert_eq!(
        rows.rows[0].status,
        Status::Running(Duration::from_secs(300))
    );
}

#[test]
fn a_stuck_job_shows_its_quiet_time() {
    let now = Instant::now();
    let mut model = model();
    model.observe(
        now,
        Ok(timeline(50, vec![quiet_record("q", 300, 120)])),
        None,
    );
    assert_eq!(model.stuck_count(), 1);
    let rows = model.rows(now, Axis::new(60), 3);
    assert_eq!(rows.rows[0].status, Status::Stuck(Duration::from_secs(120)));
}

#[test]
fn a_period_outside_the_window_draws_no_span() {
    let now = Instant::now();
    let mut model = model();
    let mut job = record("long", 3000, JobState::Running { started: ago(3000) });
    job.stuck = vec![StuckPeriod {
        since: ago(2000),
        until: Some(ago(1000)),
    }];
    model.observe(now, Ok(timeline(50, vec![job])), None);
    let rows = model.rows(now, Axis::new(60), 3);
    assert!(rows.rows[0].stuck.is_empty());
}
