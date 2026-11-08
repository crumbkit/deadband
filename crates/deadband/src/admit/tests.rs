//! Tests of the admission rule and the queue.

use std::num::NonZeroUsize;

use super::{Queue, admits};
use crate::probe::Pressure;

/// Returns `n` as a maximum number of jobs.
fn max(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).expect("the test passes a positive maximum")
}

#[test]
fn a_job_starts_under_normal_pressure_when_fewer_than_max_jobs_run() {
    assert!(admits(Some(Pressure::Normal), 1, max(2)));
}

#[test]
fn a_job_waits_under_normal_pressure_when_max_jobs_run() {
    assert!(!admits(Some(Pressure::Normal), 2, max(2)));
}

#[test]
fn a_job_waits_under_warn_pressure_while_another_job_runs() {
    assert!(!admits(Some(Pressure::Warn), 1, max(4)));
}

#[test]
fn a_job_waits_under_critical_pressure_while_another_job_runs() {
    assert!(!admits(Some(Pressure::Critical), 1, max(4)));
}

#[test]
fn a_job_starts_under_critical_pressure_when_nothing_runs() {
    assert!(admits(Some(Pressure::Critical), 0, max(1)));
}

#[test]
fn a_job_starts_at_once_when_the_probe_could_not_read_the_machine() {
    assert!(admits(None, 3, max(1)));
}

#[test]
fn the_queue_starts_waiting_jobs_in_the_order_they_arrived() {
    let mut queue = Queue::default();
    let first = queue.push("first");
    let second = queue.push("second");
    let third = queue.push("third");
    assert_eq!(queue.admit(Some(Pressure::Normal), max(2)), [first, second]);
    assert_eq!(
        queue.running().copied().collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(queue.waiting().copied().collect::<Vec<_>>(), ["third"]);
    assert_eq!(queue.ahead_of(third), Some(0));
}

#[test]
fn the_queue_starts_the_next_job_when_a_running_job_is_removed() {
    let mut queue = Queue::default();
    let first = queue.push("first");
    let second = queue.push("second");
    assert_eq!(queue.admit(Some(Pressure::Normal), max(1)), [first]);
    assert!(queue.admit(Some(Pressure::Normal), max(1)).is_empty());
    assert_eq!(queue.remove(first), Some("first"));
    assert_eq!(queue.admit(Some(Pressure::Normal), max(1)), [second]);
    assert_eq!(queue.running_job(second), Some(&"second"));
}

#[test]
fn a_removed_waiting_job_never_starts_and_the_jobs_behind_it_move_up() {
    let mut queue = Queue::default();
    let first = queue.push("first");
    let second = queue.push("second");
    let third = queue.push("third");
    assert_eq!(queue.admit(Some(Pressure::Normal), max(1)), [first]);
    assert_eq!(queue.ahead_of(third), Some(1));
    assert_eq!(queue.remove(second), Some("second"));
    assert_eq!(queue.ahead_of(third), Some(0));
    assert_eq!(queue.remove(first), Some("first"));
    assert_eq!(queue.admit(Some(Pressure::Normal), max(1)), [third]);
}

#[test]
fn the_queue_starts_one_job_under_pressure_when_nothing_runs() {
    let mut queue = Queue::default();
    let first = queue.push("first");
    queue.push("second");
    assert_eq!(queue.admit(Some(Pressure::Warn), max(4)), [first]);
}
