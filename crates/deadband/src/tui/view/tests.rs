//! Golden tests of the view. The disk, load, opened, restart and small
//! golden files were generated from the signed-off mockup's drawing code,
//! with its time axis narrowed from 84 columns to 80 so the status fits, and
//! its bars drawn in whole columns. They pin what the view draws. The off and
//! min files have no state in the mockup and were read after the view drew
//! them.

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{BG, draw};
use crate::admit::{GIB, Hold, Lane};
use crate::client::Unasked;
use crate::protocol::{Ago, Exit, JobRecord, JobState, Label, OutsideJobs, StuckPeriod, Timeline};
use crate::tui::model::Model;

const CORES: usize = 14;

fn ago(seconds: u64) -> Ago {
    Ago::from_millis(seconds * 1000)
}

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

fn record(label: &str, lane: Lane, queued: u64, state: JobState) -> JobRecord {
    JobRecord {
        label: Label::new(label),
        lane,
        queued: ago(queued),
        state,
        stuck: Vec::new(),
    }
}

fn past(label: &str, lane: Lane, queued: u64, started: u64, ended: u64, exit: Exit) -> JobRecord {
    record(
        label,
        lane,
        queued,
        JobState::Finished {
            started: ago(started),
            ended: ago(ended),
            exit,
        },
    )
}

/// The ended jobs of the mockup.
fn ended_jobs() -> Vec<JobRecord> {
    let ok = Exit::Exited { code: 0 };
    vec![
        past("starmap-docs", Lane::Normal, 430, 415, 300, Exit::Lost),
        record(
            "mxv-lint",
            Lane::Normal,
            280,
            JobState::Left { ended: ago(190) },
        ),
        past(
            "board-e2e",
            Lane::Normal,
            200,
            180,
            120,
            Exit::Signalled { signal: 9 },
        ),
        past("crumbkit-docs", Lane::Normal, 590, 575, 470, ok),
        past("board-cli-test", Lane::Normal, 560, 520, 330, ok),
        past("starmap-check", Lane::Fast, 500, 495, 400, ok),
        past(
            "deadband-suite",
            Lane::Normal,
            455,
            395,
            215,
            Exit::Exited { code: 1 },
        ),
        past("mxv-build", Lane::Fast, 380, 370, 250, ok),
        past("crumbkit-tool", Lane::Normal, 300, 210, 95, ok),
        past("board-lint", Lane::Normal, 240, 95, 40, ok),
    ]
}

fn waiting(label: &str, queued: u64) -> JobRecord {
    record(label, Lane::Normal, queued, JobState::Waiting)
}

/// The live jobs of the state held on disk.
fn disk_jobs() -> Vec<JobRecord> {
    vec![
        waiting("multiverse-review", 1262),
        waiting("crumbkit-tool", 1040),
        waiting("multiverse-review", 905),
        waiting("typed-references", 610),
        waiting("mxv-l-deny", 233),
        waiting("band-orders-red", 71),
    ]
}

/// The live jobs of the state held on load.
fn load_jobs() -> Vec<JobRecord> {
    let running = |label: &str, lane, started: u64| {
        record(
            label,
            lane,
            started + 45,
            JobState::Running {
                started: ago(started),
            },
        )
    };
    vec![
        running("board-cli-test", Lane::Normal, 254),
        running("mxv-build", Lane::Fast, 121),
        running("starmap-check", Lane::Normal, 48),
        waiting("crumbkit-tool", 163),
        waiting("mxv-l-deny", 92),
        waiting("band-orders-red", 18),
    ]
}

fn disk_hold() -> Hold {
    Hold::Disk {
        free_bytes: 26 * GIB,
        total_bytes: 926 * GIB,
    }
}

fn load_hold() -> Hold {
    Hold::Load {
        projected: 18.4,
        cores: CORES,
    }
}

/// The history of one state, deterministic where the mockup's is noisy.
struct History {
    load_now: f64,
    load_before: f64,
    held: Option<Hold>,
    held_for: u64,
    seen: u64,
}

impl History {
    fn load(&self, age: u64) -> f64 {
        if age < 300 {
            self.load_now
        } else {
            self.load_before
        }
    }

    fn hold(&self, age: u64) -> Option<Hold> {
        if age < self.held_for {
            self.held
        } else if age < self.held_for + 60 {
            Some(load_hold())
        } else if age % 97 < 20 {
            Some(Hold::Full { max_jobs: 4 })
        } else {
            None
        }
    }
}

/// Feeds `history` to a new model one second at a time, ends with `jobs`,
/// and returns the model with the time of the last poll.
fn model_of(history: &History, up: u64, jobs: Vec<JobRecord>) -> (Model, Instant) {
    model_with(history, up, jobs, None, false)
}

/// Returns the model that [`model_of`] does, whose last reply also says
/// `busiest` and `unreadable`.
fn model_with(
    history: &History,
    up: u64,
    jobs: Vec<JobRecord>,
    busiest: Option<OutsideJobs>,
    unreadable: bool,
) -> (Model, Instant) {
    let mut model = Model::new(NonZeroUsize::new(CORES));
    let start = Instant::now();
    let now = start + secs(history.seen);
    for age in (1..=history.seen).rev() {
        let reply = Timeline {
            up: ago(up + 100_000 - age),
            jobs: Vec::new(),
            hidden: 0,
            held: history.hold(age),
            busiest: None,
            unreadable: false,
        };
        model.observe(now - secs(age), Ok(reply), Some(history.load(age)));
    }
    let reply = Timeline {
        up: ago(if up < 1000 { up } else { up + 100_000 }),
        jobs,
        hidden: 0,
        held: history.hold(0),
        busiest,
        unreadable,
    };
    model.observe(now, Ok(reply), Some(history.load(0)));
    (model, now)
}

fn disk_history() -> History {
    History {
        load_now: 6.2,
        load_before: 9.0,
        held: Some(disk_hold()),
        held_for: 320,
        seen: 599,
    }
}

fn load_history(seen: u64) -> History {
    History {
        load_now: 15.9,
        load_before: 6.0,
        held: Some(load_hold()),
        held_for: 150,
        seen,
    }
}

/// Returns the text that `model` draws on a terminal of the given size.
fn render(model: &Model, now: Instant, columns: u16, lines: u16) -> String {
    let mut buffer = Buffer::empty(Rect::new(0, 0, columns, lines));
    draw(&mut buffer, model, now);
    let mut text = String::new();
    for y in 0..lines {
        let line: String = (0..columns).map(|x| buffer[(x, y)].symbol()).collect();
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text
}

fn all(mut live: Vec<JobRecord>) -> Vec<JobRecord> {
    live.extend(ended_jobs());
    live
}

/// Asserts that `actual` equals `golden`, naming the first line that differs.
fn assert_golden(actual: &str, golden: &str) {
    for (number, (a, g)) in actual.lines().zip(golden.lines()).enumerate() {
        assert_eq!(a, g, "line {number} differs");
    }
    assert_eq!(actual.lines().count(), golden.lines().count());
}

#[test]
fn held_on_disk_matches_the_mockup() {
    let (model, now) = model_of(&disk_history(), 5000, all(disk_jobs()));
    assert_golden(
        &render(&model, now, 120, 28),
        include_str!("../../../tests/golden/top/disk.txt"),
    );
}

#[test]
fn held_on_load_matches_the_mockup() {
    let (model, now) = model_of(&load_history(599), 5000, all(load_jobs()));
    assert_golden(
        &render(&model, now, 120, 28),
        include_str!("../../../tests/golden/top/load.txt"),
    );
}

#[test]
fn just_opened_matches_the_mockup() {
    let (model, now) = model_of(&load_history(95), 5000, all(load_jobs()));
    assert_golden(
        &render(&model, now, 120, 28),
        include_str!("../../../tests/golden/top/opened.txt"),
    );
}

#[test]
fn a_daemon_restart_matches_the_mockup() {
    let (mut model, now) = model_of(&load_history(599), 5000, Vec::new());
    let reply = Timeline {
        up: ago(40),
        jobs: load_jobs(),
        hidden: 0,
        held: Some(load_hold()),
        busiest: None,
        unreadable: false,
    };
    model.observe(now, Ok(reply), Some(15.9));
    assert_golden(
        &render(&model, now, 120, 28),
        include_str!("../../../tests/golden/top/restart.txt"),
    );
}

#[test]
fn a_terminal_too_small_matches_the_mockup() {
    let model = Model::new(NonZeroUsize::new(CORES));
    let golden = include_str!("../../../tests/golden/top/small.txt");
    let drawn = render(&model, Instant::now(), 60, 12);
    let first_lines: Vec<_> = golden.lines().take(12).collect();
    assert_eq!(drawn.lines().collect::<Vec<_>>(), first_lines);
}

#[test]
fn no_daemon_says_jobs_start_at_once_and_keeps_the_load_graph() {
    let mut model = Model::new(NonZeroUsize::new(CORES));
    let now = Instant::now();
    model.observe(
        now,
        Err(Unasked::NotRunning {
            path: PathBuf::from("/no"),
        }),
        Some(7.0),
    );
    assert_golden(
        &render(&model, now, 120, 28),
        include_str!("../../../tests/golden/top/off.txt"),
    );
}

#[test]
fn the_smallest_terminal_shows_every_part_of_the_screen() {
    let (model, now) = model_of(&disk_history(), 5000, all(disk_jobs()));
    assert_golden(
        &render(&model, now, 80, 16),
        include_str!("../../../tests/golden/top/min.txt"),
    );
}

#[test]
fn an_older_daemon_is_named_on_the_screen() {
    let mut model = Model::new(NonZeroUsize::new(CORES));
    let now = Instant::now();
    model.observe(now, Err(Unasked::Closed), None);
    let text = render(&model, now, 120, 28);
    assert!(text.contains("may be older than this command"), "{text}");
    assert!(text.contains("Stop the daemon that runs now"), "{text}");
    assert!(text.contains("OLDER DAEMON"), "{text}");
    assert!(!text.contains("NO DAEMON"), "{text}");
}

#[test]
fn a_silent_daemon_says_how_long_since_an_answer() {
    let (mut model, now) = model_of(&load_history(10), 5000, load_jobs());
    model.observe(
        now + secs(7),
        Err(Unasked::Silent {
            waited: Duration::from_millis(500),
        }),
        Some(1.0),
    );
    let text = render(&model, now + secs(7), 120, 28);
    assert!(
        text.contains("no answer for 7s: the daemon did not answer within 500 ms"),
        "{text}"
    );
    assert!(text.contains("board-cli-test"), "{text}");
}

#[test]
fn a_reply_says_how_many_jobs_are_not_shown() {
    let mut model = Model::new(NonZeroUsize::new(CORES));
    let now = Instant::now();
    let reply = Timeline {
        up: ago(50),
        jobs: all(disk_jobs()),
        hidden: 2,
        held: None,
        busiest: None,
        unreadable: false,
    };
    model.observe(now, Ok(reply), None);
    let text = render(&model, now, 80, 16);
    assert!(text.contains("14 more jobs not shown"), "{text}");
}

#[test]
fn one_job_with_no_row_reads_in_the_singular() {
    let mut model = Model::new(NonZeroUsize::new(CORES));
    let now = Instant::now();
    let reply = Timeline {
        up: ago(50),
        jobs: Vec::new(),
        hidden: 1,
        held: None,
        busiest: None,
        unreadable: false,
    };
    model.observe(now, Ok(reply), None);
    let text = render(&model, now, 120, 28);
    assert!(text.contains("1 more job not shown"), "{text}");
}

/// Returns the buffer of a model at this size.
fn painted(columns: u16, lines: u16) -> Buffer {
    let (model, now) = model_of(&disk_history(), 5000, all(disk_jobs()));
    let mut buffer = Buffer::empty(Rect::new(0, 0, columns, lines));
    draw(&mut buffer, &model, now);
    buffer
}

#[test]
fn every_cell_below_the_header_has_the_mockups_background() {
    let buffer = painted(120, 28);
    for y in 1..28 {
        for x in 0..120 {
            assert_eq!(buffer[(x, y)].bg, BG, "cell {x}, {y}");
        }
    }
}

#[test]
fn the_too_small_message_has_the_mockups_background() {
    let buffer = painted(60, 12);
    assert_eq!(buffer[(1, 1)].bg, BG);
    assert_eq!(buffer[(59, 11)].bg, BG);
}

#[test]
fn a_label_cut_to_its_column_ends_in_an_ellipsis() {
    let mut model = Model::new(NonZeroUsize::new(CORES));
    let now = Instant::now();
    let reply = Timeline {
        up: ago(50),
        jobs: vec![waiting(&"a".repeat(30), 5)],
        hidden: 0,
        held: None,
        busiest: None,
        unreadable: false,
    };
    model.observe(now, Ok(reply), None);
    let text = render(&model, now, 120, 28);
    assert!(text.contains(&format!(" {}… ", "a".repeat(19))), "{text}");
}

/// Returns the text of a model whose one job has the longest state.
fn longest_state(columns: u16, lines: u16) -> String {
    let mut model = Model::new(NonZeroUsize::new(CORES));
    let now = Instant::now();
    let reply = Timeline {
        up: ago(50),
        jobs: vec![past(
            "a",
            Lane::Normal,
            999 * 3600 + 20,
            999 * 3600 + 10,
            10,
            Exit::Signalled { signal: 64 },
        )],
        hidden: 0,
        held: None,
        busiest: None,
        unreadable: false,
    };
    model.observe(now, Ok(reply), None);
    render(&model, now, columns, lines)
}

#[test]
fn the_longest_state_shows_whole_at_120_columns() {
    let text = longest_state(120, 28);
    assert!(text.contains("signal 64 999h"), "{text}");
}

#[test]
fn the_longest_state_shows_whole_at_the_minimum_width() {
    let text = longest_state(80, 16);
    assert!(text.contains("signal 64 999h"), "{text}");
}

/// Returns a running job that has shown no activity for `quiet` seconds.
fn stuck(label: &str, queued: u64, started: u64, quiet: u64) -> JobRecord {
    JobRecord {
        stuck: vec![StuckPeriod {
            since: ago(quiet),
            until: None,
        }],
        ..record(
            label,
            Lane::Normal,
            queued,
            JobState::Running {
                started: ago(started),
            },
        )
    }
}

fn running(label: &str, queued: u64, started: u64) -> JobRecord {
    record(
        label,
        Lane::Normal,
        queued,
        JobState::Running {
            started: ago(started),
        },
    )
}

fn full_history() -> History {
    History {
        load_now: 3.2,
        load_before: 3.2,
        held: Some(Hold::Full { max_jobs: 4 }),
        held_for: 600,
        seen: 599,
    }
}

fn syspolicyd() -> Option<OutsideJobs> {
    Some(OutsideJobs::Busy {
        name: "syspolicyd".to_owned(),
        cpu_percent: 470,
    })
}

/// The jobs of the mockup's first state: two stuck, syspolicyd busy.
fn incident_jobs() -> Vec<JobRecord> {
    vec![
        running("starmap-check", 330, 300),
        waiting("crumbkit-tool", 260),
        waiting("mxv-l-deny", 150),
        waiting("band-orders-red", 60),
        stuck("board-cli-test", 2840, 2790, 2760),
        stuck("mirror-git-dir", 900, 850, 420),
    ]
}

fn incident(columns: u16, lines: u16) -> String {
    let (model, now) = model_with(&full_history(), 5000, incident_jobs(), syspolicyd(), false);
    render(&model, now, columns, lines)
}

#[test]
fn a_stuck_job_is_listed_first_with_its_quiet_time() {
    let screen = incident(120, 28);
    let lines: Vec<_> = screen.lines().collect();
    assert!(lines[3].starts_with(" board-cli-test "), "{screen}");
    assert!(lines[3].ends_with("stuck 46m"), "{screen}");
    assert!(lines[4].starts_with(" mirror-git-dir "), "{screen}");
    assert!(lines[4].ends_with("stuck 7m"), "{screen}");
}

#[test]
fn a_stuck_job_shows_the_no_activity_bar_from_its_last_activity() {
    let screen = incident(120, 28);
    let lines: Vec<_> = screen.lines().collect();
    // A quiet time over the window fills the axis.
    assert_eq!(lines[3].matches('▓').count(), 80, "{screen}");
    // 420 seconds of a 600 second window on 80 columns is 56 columns.
    let bars = lines[4].matches('▓').count();
    assert!((56..=57).contains(&bars), "{bars}: {screen}");
    assert!(lines[4].contains("█▓"), "{screen}");
    assert_eq!(lines[5].matches('▓').count(), 0, "{screen}");
}

#[test]
fn the_header_counts_the_stuck_jobs_left_of_the_hold_label() {
    let screen = incident(120, 28);
    let first = screen.lines().next().expect("a line");
    let stuck = first.find("2 STUCK").expect("the label shows");
    let hold = first.find("HELD · FULL").expect("the hold shows");
    assert!(stuck < hold, "{first}");
}

#[test]
fn a_line_above_the_rule_names_the_count_and_the_busiest_process() {
    let screen = incident(120, 28);
    let lines: Vec<_> = screen.lines().collect();
    assert!(
        lines[18].starts_with(
            " 2 jobs show no activity · busiest process outside the jobs: syspolicyd at 470% CPU"
        ),
        "{screen}"
    );
    assert!(lines[19].starts_with("───"), "{screen}");
}

#[test]
fn the_legend_gains_no_activity_while_a_row_shows_it() {
    let screen = incident(120, 28);
    assert!(
        screen.contains(
            "▀ open   ▀ full   ▀ load   ▀ memory   ▀ disk   ░ waiting   █ running   ▓ no activity"
        ),
        "{screen}"
    );
}

#[test]
fn the_smallest_terminal_keeps_stuck_jobs_and_the_whole_legend() {
    let screen = incident(80, 16);
    let lines: Vec<_> = screen.lines().collect();
    assert!(lines[3].starts_with(" board-cli-test "), "{screen}");
    assert!(lines[4].starts_with(" mirror-git-dir "), "{screen}");
    assert!(lines[5].starts_with(" starmap-check "), "{screen}");
    assert!(lines[6].starts_with(" 2 jobs show no activity"), "{screen}");
    assert!(screen.contains("▓ no activity"), "{screen}");
    assert!(lines[0].contains("2 STUCK"), "{screen}");
}

#[test]
fn a_screen_with_no_stuck_job_shows_none_of_it() {
    let jobs = vec![
        running("starmap-check", 330, 300),
        waiting("crumbkit-tool", 260),
    ];
    let (model, now) = model_with(&full_history(), 5000, jobs, None, false);
    let screen = render(&model, now, 120, 28);
    assert!(!screen.contains("STUCK"), "{screen}");
    assert!(!screen.contains("no activity"), "{screen}");
    assert!(!screen.contains('▓'), "{screen}");
}

#[test]
fn a_recovered_job_keeps_its_period_and_runs_again() {
    let mut job = running("board-cli-test", 3050, 3000);
    job.stuck = vec![StuckPeriod {
        since: ago(240),
        until: Some(ago(90)),
    }];
    let (model, now) = model_with(&full_history(), 5000, vec![job], None, false);
    let screen = render(&model, now, 120, 28);
    let lines: Vec<_> = screen.lines().collect();
    assert!(lines[3].ends_with("run 50m"), "{screen}");
    assert_eq!(lines[3].matches('▓').count(), 20, "{screen}");
    assert!(screen.contains("▓ no activity"), "{screen}");
    assert!(!screen.contains("STUCK"), "{screen}");
    assert!(!screen.contains("show no activity"), "{screen}");
}

#[test]
fn an_ended_job_keeps_its_period_while_it_is_in_the_window() {
    let mut job = past(
        "mirror-git-dir",
        Lane::Normal,
        520,
        500,
        110,
        Exit::Exited { code: 0 },
    );
    job.stuck = vec![StuckPeriod {
        since: ago(430),
        until: Some(ago(210)),
    }];
    let (model, now) = model_with(&full_history(), 5000, vec![job], None, false);
    let screen = render(&model, now, 120, 28);
    let lines: Vec<_> = screen.lines().collect();
    assert_eq!(lines[3].matches('▓').count(), 29, "{screen}");
    assert!(lines[3].contains("ok 6m"), "{screen}");
}

#[test]
fn a_read_that_failed_says_activity_is_unknown() {
    let jobs = vec![running("board-cli-test", 400, 380)];
    let (model, now) = model_with(&full_history(), 5000, jobs, None, true);
    let screen = render(&model, now, 120, 28);
    let lines: Vec<_> = screen.lines().collect();
    assert!(
        lines[18].starts_with(" activity unknown: deadband could not read the jobs' processes"),
        "{screen}"
    );
}

#[test]
fn a_stuck_row_is_drawn_in_red() {
    let (model, now) = model_with(&full_history(), 5000, incident_jobs(), syspolicyd(), false);
    let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 28));
    draw(&mut buffer, &model, now);
    assert_eq!(buffer[(30, 3)].symbol(), "▓");
    assert_eq!(buffer[(30, 3)].fg, super::RED);
    assert_eq!(buffer[(106, 3)].fg, super::RED);
}

/// The screens below were read after the view drew them, and compared with a
/// text dump of the signed-off mockup: the rows, the header labels, the line
/// above the rule and the legend match. The pill keeps the two columns at the
/// right that every other screen has, and the graph shows this test's history.
#[test]
fn two_stuck_jobs_match_the_signed_off_screen() {
    assert_golden(
        &incident(120, 28),
        include_str!("../../../tests/golden/top/stuck.txt"),
    );
}

#[test]
fn two_stuck_jobs_on_the_smallest_terminal_match_the_signed_off_screen() {
    assert_golden(
        &incident(80, 16),
        include_str!("../../../tests/golden/top/stuck-min.txt"),
    );
}
