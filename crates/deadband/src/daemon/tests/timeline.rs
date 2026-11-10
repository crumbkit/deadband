//! Tests of the timeline, one for each row of the job's lifecycle.

use std::num::NonZeroUsize;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Instant;

use super::super::timeline::{MAX_ENDED, MAX_ROWS};
use super::super::{Settings, State, Waiter};
use super::{Client, Dial, REPLY_WITHIN, granted, job, queued, start};
use crate::admit::{Hold, JobId, Lane};
use crate::probe::Pressure;
use crate::protocol::{Exit, JobRecord, JobState, Label, Reply, Request, Timeline};

/// Asks the daemon at `path` for its timeline.
pub(super) fn timeline(path: &Path) -> Timeline {
    let mut client = Client::connect(path);
    client.send(&Request::Timeline);
    match client.reply() {
        Reply::Timeline(timeline) => timeline,
        other => panic!("the daemon answered timeline with {other:?}"),
    }
}

/// Asks the daemon until `done` accepts its timeline. The daemon records a
/// job on the thread of that job's connection, and each request is an
/// observable round trip.
pub(super) fn timeline_until(path: &Path, done: impl Fn(&Timeline) -> bool) -> Timeline {
    let begun = Instant::now();
    loop {
        let timeline = timeline(path);
        if done(&timeline) {
            return timeline;
        }
        assert!(begun.elapsed() < REPLY_WITHIN, "{timeline:?}");
    }
}

/// Returns the labels of `timeline`'s rows.
fn labels(timeline: &Timeline) -> Vec<&str> {
    timeline.jobs.iter().map(|row| row.label.as_str()).collect()
}

/// Returns the one row labelled `label`.
pub(super) fn row<'a>(timeline: &'a Timeline, label: &str) -> &'a JobRecord {
    timeline
        .jobs
        .iter()
        .find(|row| row.label.as_str() == label)
        .expect("the timeline has the row")
}

/// Reports whether `timeline` has a row of `label` that has ended.
pub(super) fn ended(timeline: &Timeline, label: &str) -> bool {
    timeline.jobs.iter().any(|row| {
        row.label.as_str() == label
            && matches!(row.state, JobState::Finished { .. } | JobState::Left { .. })
    })
}

/// Runs one granted job labelled `a` to `end` and returns its ended row.
fn ended_row(end: impl FnOnce(Client)) -> JobRecord {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut client = Client::ask(&path, "a");
    assert_eq!(client.reply(), granted(1));
    end(client);
    let timeline = timeline_until(&path, |timeline| ended(timeline, "a"));
    row(&timeline, "a").clone()
}

#[test]
fn a_daemon_with_no_jobs_answers_an_empty_timeline() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let timeline = timeline(&path);
    assert_eq!(timeline.jobs, Vec::new());
    assert_eq!(timeline.hidden, 0);
    assert_eq!(timeline.held, None);
}

#[test]
fn the_time_the_daemon_has_been_up_grows_between_replies() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let first = timeline(&path).up;
    timeline_until(&path, |later| later.up.duration() > first.duration());
}

#[test]
fn a_job_that_waits_shows_as_waiting_with_what_holds_it() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), granted(1));
    let mut second = Client::ask(&path, "b");
    assert_eq!(second.reply(), queued(0, 1));
    let timeline = timeline(&path);
    assert_eq!(row(&timeline, "b").state, JobState::Waiting);
    assert_eq!(timeline.held, Some(Hold::Full { max_jobs: 1 }));
}

#[test]
fn a_job_that_starts_shows_as_running() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut client = Client::ask(&path, "a");
    assert_eq!(client.reply(), granted(1));
    let timeline = timeline(&path);
    assert!(
        matches!(row(&timeline, "a").state, JobState::Running { .. }),
        "{timeline:?}"
    );
}

#[test]
fn a_job_whose_client_reports_a_code_is_finished_with_that_code() {
    let row = ended_row(|mut client| {
        client.send(&Request::Done {
            exit_code: Some(3),
            signal: None,
            usage: None,
        });
    });
    assert!(
        matches!(
            row.state,
            JobState::Finished {
                exit: Exit::Exited { code: 3 },
                ..
            }
        ),
        "{row:?}"
    );
}

#[test]
fn a_job_whose_client_reports_a_signal_is_finished_with_that_signal() {
    let row = ended_row(|mut client| {
        client.send(&Request::Done {
            exit_code: None,
            signal: Some(9),
            usage: None,
        });
    });
    assert!(
        matches!(
            row.state,
            JobState::Finished {
                exit: Exit::Signalled { signal: 9 },
                ..
            }
        ),
        "{row:?}"
    );
}

#[test]
fn a_job_whose_client_reports_neither_is_lost() {
    let row = ended_row(|mut client| {
        client.send(&Request::Done {
            exit_code: None,
            signal: None,
            usage: None,
        });
    });
    assert!(
        matches!(
            row.state,
            JobState::Finished {
                exit: Exit::Lost,
                ..
            }
        ),
        "{row:?}"
    );
}

#[test]
fn a_running_job_whose_client_is_killed_is_lost() {
    let row = ended_row(drop);
    assert!(
        matches!(
            row.state,
            JobState::Finished {
                exit: Exit::Lost,
                ..
            }
        ),
        "{row:?}"
    );
}

#[test]
fn a_running_job_whose_client_sends_an_unreadable_line_is_lost() {
    let row = ended_row(|mut client| {
        std::io::Write::write_all(&mut client.stream, b"not json\n").expect("the line writes");
    });
    assert!(
        matches!(
            row.state,
            JobState::Finished {
                exit: Exit::Lost,
                ..
            }
        ),
        "{row:?}"
    );
}

#[test]
fn a_waiting_job_whose_client_is_killed_has_left() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    let mut first = Client::ask(&path, "a");
    assert_eq!(first.reply(), granted(1));
    let mut second = Client::ask(&path, "b");
    assert_eq!(second.reply(), queued(0, 1));
    drop(second);
    let timeline = timeline_until(&path, |timeline| ended(timeline, "b"));
    assert!(
        matches!(row(&timeline, "b").state, JobState::Left { .. }),
        "{timeline:?}"
    );
}

#[test]
fn the_reply_lists_running_then_waiting_then_ended_jobs_newest_first() {
    let (path, _) = start(1, &Dial::new(Pressure::Normal));
    for label in ["old", "new"] {
        let mut client = Client::ask(&path, label);
        assert_eq!(client.reply(), granted(1));
        client.send(&Request::Done {
            exit_code: Some(0),
            signal: None,
            usage: None,
        });
        timeline_until(&path, |timeline| ended(timeline, label));
    }
    let mut running = Client::ask(&path, "running");
    assert_eq!(running.reply(), granted(1));
    let mut waiting = Client::ask(&path, "waiting");
    assert_eq!(waiting.reply(), queued(0, 1));
    assert_eq!(
        labels(&timeline(&path)),
        vec!["running", "waiting", "new", "old"]
    );
}

/// A daemon's state with room for `max_jobs` jobs.
fn state(max_jobs: usize) -> State {
    let settings = Settings::new(
        NonZeroUsize::new(max_jobs).expect("positive"),
        NonZeroUsize::new(16),
        None,
        None,
    );
    State::new(settings, None, false)
}

/// Queues a job labelled `label` whose client has gone when `gone` holds,
/// and returns its number with the client's end of the connection.
fn push(state: &mut State, label: &str, gone: bool) -> (JobId, Option<UnixStream>) {
    let (ours, theirs) = UnixStream::pair().expect("the pair opens");
    let id = state.queue.push(
        Waiter {
            job: job(label),
            stream: ours,
            label: Label::new(label),
            queued: Instant::now(),
            arrival: None,
            ahead: 0,
        },
        Lane::Normal,
    );
    (id, (!gone).then_some(theirs))
}

#[test]
fn a_job_that_cannot_take_its_grant_has_left() {
    let mut state = state(1);
    push(&mut state, "a", true);
    state.grant();
    let timeline = state.timeline(Instant::now());
    assert!(
        matches!(row(&timeline, "a").state, JobState::Left { .. }),
        "{timeline:?}"
    );
}

#[test]
fn a_job_that_takes_its_grant_runs() {
    let mut state = state(1);
    let (_id, _client) = push(&mut state, "a", false);
    state.grant();
    let timeline = state.timeline(Instant::now());
    assert!(
        matches!(row(&timeline, "a").state, JobState::Running { .. }),
        "{timeline:?}"
    );
}

#[test]
fn a_job_is_recorded_once_when_its_connection_ends_after_a_failed_grant() {
    let mut state = state(1);
    let (id, _) = push(&mut state, "a", true);
    state.grant();
    state.record(id, Some(Exit::Lost), None);
    assert_eq!(state.timeline(Instant::now()).jobs.len(), 1);
}

/// Ends one job labelled `label` on `state`.
fn end_one(state: &mut State, label: &str) {
    let (id, client) = push(state, label, false);
    state.grant();
    state.record(id, Some(Exit::Exited { code: 0 }), None);
    drop(client);
}

#[test]
fn the_sixty_fifth_ended_job_evicts_the_oldest() {
    let mut state = state(1);
    for n in 0..=MAX_ENDED {
        end_one(&mut state, &n.to_string());
    }
    let timeline = state.timeline(Instant::now());
    assert_eq!(timeline.jobs.len(), MAX_ENDED);
    assert_eq!(labels(&timeline).first(), Some(&"64"));
    assert_eq!(labels(&timeline).last(), Some(&"1"));
}

#[test]
fn sixty_four_ended_jobs_are_all_kept() {
    let mut state = state(1);
    for n in 0..MAX_ENDED {
        end_one(&mut state, &n.to_string());
    }
    let timeline = state.timeline(Instant::now());
    assert_eq!(timeline.jobs.len(), MAX_ENDED);
    assert_eq!(labels(&timeline).last(), Some(&"0"));
}

#[test]
fn the_reply_stops_at_its_row_limit_and_counts_the_rest() {
    let mut state = state(1);
    let clients: Vec<_> = (0..=MAX_ROWS)
        .map(|n| push(&mut state, &n.to_string(), false))
        .collect();
    let timeline = state.timeline(Instant::now());
    assert_eq!(timeline.jobs.len(), MAX_ROWS);
    assert_eq!(timeline.hidden, 1);
    drop(clients);
}

#[test]
fn a_reply_at_its_row_limit_hides_nothing() {
    let mut state = state(1);
    let clients: Vec<_> = (0..MAX_ROWS)
        .map(|n| push(&mut state, &n.to_string(), false))
        .collect();
    let timeline = state.timeline(Instant::now());
    assert_eq!(timeline.jobs.len(), MAX_ROWS);
    assert_eq!(timeline.hidden, 0);
    drop(clients);
}
