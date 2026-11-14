//! Tests of the admission rule, the parallelism of each lane, and the queue.
//! Each test of a hold has a twin that admits the same job when the one
//! reading that held it changes.

use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use super::{GIB, Hold, JobId, Lane, Limits, Queue, RECENT, Reading, Running, Threads, admits};
use crate::probe::{Disk, Pressure};

/// Returns `n` as a positive number.
fn positive(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).expect("the test passes a positive number")
}

/// Limits of `max_jobs` jobs on a machine with `cores` cores, with no
/// priority file.
fn limits(max_jobs: usize, cores: usize) -> Limits {
    Limits {
        max_jobs: positive(max_jobs),
        cores: Some(positive(cores)),
        reserve_lanes: false,
    }
}

/// The same limits with an active priority file, which keeps a place open
/// for each lane.
fn reserving(max_jobs: usize, cores: usize) -> Limits {
    Limits {
        reserve_lanes: true,
        ..limits(max_jobs, cores)
    }
}

/// `all` running jobs, all in the asking job's lane, whose threads started
/// in the last minute add up to `recent_threads`.
fn running(all: usize, recent_threads: usize) -> Running {
    Running {
        all,
        in_lane: all,
        recent_threads,
    }
}

/// Starts the jobs of `queue` that may start at `now`, as
/// [`Queue::admit`] does, and returns each one's number with its count of
/// threads.
fn admitted<J>(
    queue: &mut Queue<J>,
    reading: Option<&Reading>,
    limits: &Limits,
    now: Instant,
) -> Vec<(JobId, usize)> {
    queue
        .admit(reading, limits, now)
        .into_iter()
        .map(|(id, threads)| (id, threads.get()))
        .collect()
}

/// A disk with `free` of `total` GiB free.
fn disk(free: u64, total: u64) -> Disk {
    Disk {
        free_bytes: free * GIB,
        total_bytes: total * GIB,
    }
}

/// A reading with `pressure`, a load of `load_1m` and 500 of 1000 GiB free.
fn reading(pressure: Pressure, load_1m: f64) -> Reading {
    Reading {
        pressure,
        load_1m,
        disk: Some(disk(500, 1000)),
    }
}

/// A calm machine: normal pressure, a load of 1 and a mostly empty disk.
fn calm() -> Reading {
    reading(Pressure::Normal, 1.0)
}

#[test]
fn a_job_starts_under_normal_pressure_when_fewer_than_max_jobs_run() {
    assert_eq!(
        admits(Some(&calm()), &running(1, 0), &limits(2, 16)),
        Ok(())
    );
}

#[test]
fn a_job_waits_under_normal_pressure_when_max_jobs_run() {
    assert_eq!(
        admits(Some(&calm()), &running(2, 0), &limits(2, 16)),
        Err(Hold::Full { max_jobs: 2 })
    );
}

#[test]
fn a_job_waits_under_warn_pressure_while_another_job_runs() {
    let warn = reading(Pressure::Warn, 1.0);
    assert_eq!(
        admits(Some(&warn), &running(1, 0), &limits(4, 16)),
        Err(Hold::Memory {
            pressure: Pressure::Warn
        })
    );
}

#[test]
fn a_job_waits_under_critical_pressure_while_another_job_runs() {
    let critical = reading(Pressure::Critical, 1.0);
    assert_eq!(
        admits(Some(&critical), &running(1, 0), &limits(4, 16)),
        Err(Hold::Memory {
            pressure: Pressure::Critical
        })
    );
}

#[test]
fn a_job_starts_under_critical_pressure_when_nothing_runs() {
    let critical = reading(Pressure::Critical, 1.0);
    assert_eq!(
        admits(Some(&critical), &running(0, 0), &limits(1, 16)),
        Ok(())
    );
}

#[test]
fn a_job_starts_when_the_probe_could_not_read_the_machine_and_a_place_is_free() {
    assert_eq!(admits(None, &running(3, 0), &limits(4, 16)), Ok(()));
}

#[test]
fn a_job_waits_for_a_place_when_the_probe_could_not_read_the_machine() {
    assert_eq!(
        admits(None, &running(3, 0), &limits(3, 16)),
        Err(Hold::Full { max_jobs: 3 })
    );
}

#[test]
fn a_job_waits_while_the_load_and_the_jobs_started_in_the_last_minute_exceed_the_cores() {
    let busy = reading(Pressure::Normal, 10.0);
    assert_eq!(
        admits(Some(&busy), &running(1, 8), &limits(4, 16)),
        Err(Hold::Load {
            projected: 18.0,
            cores: 16
        })
    );
}

#[test]
fn a_job_starts_while_the_load_and_the_jobs_started_in_the_last_minute_stay_under_the_cores() {
    let busy = reading(Pressure::Normal, 10.0);
    assert_eq!(admits(Some(&busy), &running(1, 5), &limits(4, 16)), Ok(()));
}

#[test]
fn a_job_waits_while_the_projected_load_equals_the_cores() {
    let busy = reading(Pressure::Normal, 10.0);
    assert_eq!(
        admits(Some(&busy), &running(1, 6), &limits(4, 16)),
        Err(Hold::Load {
            projected: 16.0,
            cores: 16
        })
    );
}

#[test]
fn a_job_starts_on_a_high_load_when_nothing_runs() {
    let busy = reading(Pressure::Normal, 40.0);
    assert_eq!(admits(Some(&busy), &running(0, 0), &limits(4, 16)), Ok(()));
}

#[test]
fn the_load_holds_nothing_when_the_core_count_is_unknown() {
    let busy = reading(Pressure::Normal, 40.0);
    let unknown = Limits {
        max_jobs: positive(4),
        cores: None,
        reserve_lanes: false,
    };
    assert_eq!(admits(Some(&busy), &running(1, 0), &unknown), Ok(()));
}

#[test]
fn a_job_waits_while_free_disk_is_under_20_gib() {
    let low = Reading {
        disk: Some(disk(19, 1000)),
        ..calm()
    };
    assert_eq!(
        admits(Some(&low), &running(1, 0), &limits(4, 16)),
        Err(Hold::Disk {
            free_bytes: 19 * GIB,
            total_bytes: 1000 * GIB
        })
    );
}

#[test]
fn a_job_starts_while_free_disk_is_20_gib_and_over_3_percent() {
    let enough = Reading {
        disk: Some(disk(20, 600)),
        ..calm()
    };
    assert_eq!(
        admits(Some(&enough), &running(1, 0), &limits(4, 16)),
        Ok(())
    );
}

#[test]
fn a_job_waits_while_free_disk_is_under_3_percent() {
    let low = Reading {
        disk: Some(disk(29, 1000)),
        ..calm()
    };
    assert!(matches!(
        admits(Some(&low), &running(1, 0), &limits(4, 16)),
        Err(Hold::Disk { .. })
    ));
}

#[test]
fn a_job_starts_while_free_disk_is_3_percent() {
    let enough = Reading {
        disk: Some(disk(30, 1000)),
        ..calm()
    };
    assert_eq!(
        admits(Some(&enough), &running(1, 0), &limits(4, 16)),
        Ok(())
    );
}

#[test]
fn a_job_waits_on_a_critically_low_disk_even_when_nothing_runs() {
    let full = Reading {
        disk: Some(disk(1, 1000)),
        ..calm()
    };
    assert!(matches!(
        admits(Some(&full), &running(0, 0), &limits(4, 16)),
        Err(Hold::Disk { .. })
    ));
}

#[test]
fn a_job_starts_on_a_disk_with_room_when_nothing_runs_under_pressure() {
    let critical = reading(Pressure::Critical, 40.0);
    assert_eq!(
        admits(Some(&critical), &running(0, 0), &limits(4, 16)),
        Ok(())
    );
}

#[test]
fn an_unreadable_disk_holds_nothing() {
    let unknown = Reading {
        disk: None,
        ..calm()
    };
    assert_eq!(
        admits(Some(&unknown), &running(1, 0), &limits(4, 16)),
        Ok(())
    );
}

#[test]
fn with_a_priority_file_a_lane_waits_once_it_runs_all_but_one_place() {
    let full_lane = Running {
        all: 3,
        in_lane: 3,
        recent_threads: 0,
    };
    assert_eq!(
        admits(Some(&calm()), &full_lane, &reserving(4, 16)),
        Err(Hold::Lane { places: 3 })
    );
}

#[test]
fn with_a_priority_file_a_job_starts_in_the_lane_that_has_its_reserved_place() {
    let other_lane = Running {
        all: 3,
        in_lane: 0,
        recent_threads: 0,
    };
    assert_eq!(
        admits(Some(&calm()), &other_lane, &reserving(4, 16)),
        Ok(())
    );
}

#[test]
fn without_a_priority_file_one_lane_may_take_every_place() {
    let full_lane = Running {
        all: 3,
        in_lane: 3,
        recent_threads: 0,
    };
    assert_eq!(admits(Some(&calm()), &full_lane, &limits(4, 16)), Ok(()));
}

#[test]
fn with_one_place_a_priority_file_reserves_nothing() {
    assert_eq!(
        admits(Some(&calm()), &running(0, 0), &reserving(1, 16)),
        Ok(())
    );
}

#[test]
fn each_job_gets_the_cores_divided_by_max_jobs_and_a_fast_job_one_more() {
    let limits = limits(4, 14);
    assert_eq!(limits.parallelism(Lane::Normal), Threads::new(3));
    assert_eq!(limits.parallelism(Lane::Fast), Threads::new(4));
}

#[test]
fn each_job_gets_at_least_one_thread() {
    assert_eq!(limits(8, 4).parallelism(Lane::Normal), Threads::new(1));
    let unknown = Limits {
        max_jobs: positive(2),
        cores: None,
        reserve_lanes: false,
    };
    assert_eq!(unknown.parallelism(Lane::Normal), Threads::new(1));
    assert_eq!(unknown.parallelism(Lane::Fast), Threads::new(2));
}

#[test]
fn the_queue_starts_waiting_jobs_in_the_order_they_arrived() {
    let now = Instant::now();
    let mut queue = Queue::default();
    let first = queue.push("first", Lane::Normal);
    let second = queue.push("second", Lane::Normal);
    let third = queue.push("third", Lane::Normal);
    assert_eq!(
        admitted(&mut queue, Some(&calm()), &limits(2, 16), now),
        [(first, 8), (second, 8)]
    );
    assert_eq!(
        queue.running().copied().collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(queue.waiting().copied().collect::<Vec<_>>(), ["third"]);
    assert_eq!(queue.ahead_of(third), Some(0));
    assert_eq!(queue.held(), Some(Hold::Full { max_jobs: 2 }));
}

#[test]
fn a_waiting_fast_job_starts_before_the_normal_jobs_that_arrived_earlier() {
    let now = Instant::now();
    let mut queue = Queue::default();
    let running = queue.push("running", Lane::Normal);
    assert_eq!(
        admitted(&mut queue, Some(&calm()), &limits(1, 16), now),
        [(running, 16)]
    );
    let normal = queue.push("normal", Lane::Normal);
    let fast = queue.push("fast", Lane::Fast);
    assert_eq!(queue.ahead_of(fast), Some(0));
    assert_eq!(queue.ahead_of(normal), Some(1));
    assert_eq!(
        queue.waiting().copied().collect::<Vec<_>>(),
        ["fast", "normal"]
    );
    queue.remove(running);
    assert_eq!(
        admitted(&mut queue, Some(&calm()), &limits(1, 16), now),
        [(fast, 17)]
    );
}

#[test]
fn a_waiting_normal_job_starts_when_no_fast_job_waits() {
    let now = Instant::now();
    let mut queue = Queue::default();
    let running = queue.push("running", Lane::Fast);
    admitted(&mut queue, Some(&calm()), &limits(1, 16), now);
    let normal = queue.push("normal", Lane::Normal);
    queue.remove(running);
    assert_eq!(
        admitted(&mut queue, Some(&calm()), &limits(1, 16), now),
        [(normal, 16)]
    );
}

#[test]
fn the_queue_counts_each_job_started_in_the_last_minute_as_its_parallelism() {
    let start = Instant::now();
    let mut queue = Queue::default();
    let busy = reading(Pressure::Normal, 9.0);
    let first = queue.push("first", Lane::Normal);
    let second = queue.push("second", Lane::Normal);
    // 16 cores and 4 jobs give each job 4 threads. The first job starts
    // because nothing runs. A load of 9 and the first job's 4 project 13,
    // so the second starts, and with both the projection is 17.
    assert_eq!(
        admitted(&mut queue, Some(&busy), &limits(4, 16), start),
        [(first, 4), (second, 4)]
    );
    let third = queue.push("third", Lane::Normal);
    assert!(admitted(&mut queue, Some(&busy), &limits(4, 16), start).is_empty());
    assert_eq!(
        queue.held(),
        Some(Hold::Load {
            projected: 17.0,
            cores: 16
        })
    );
    let later = start + RECENT + Duration::from_secs(1);
    assert_eq!(
        admitted(&mut queue, Some(&busy), &limits(4, 16), later),
        [(third, 4)]
    );
}

#[test]
fn a_fast_job_on_a_machine_full_of_normal_jobs_starts_at_once() {
    let now = Instant::now();
    let mut queue = Queue::default();
    for each in ["a", "b", "c"] {
        queue.push(each, Lane::Normal);
    }
    assert_eq!(
        admitted(&mut queue, Some(&calm()), &reserving(4, 16), now).len(),
        3
    );
    let normal = queue.push("normal", Lane::Normal);
    assert!(
        queue
            .admit(Some(&calm()), &reserving(4, 16), now)
            .is_empty()
    );
    assert_eq!(queue.held(), Some(Hold::Lane { places: 3 }));
    let fast = queue.push("fast", Lane::Fast);
    assert_eq!(
        admitted(&mut queue, Some(&calm()), &reserving(4, 16), now),
        [(fast, 5)]
    );
    assert_eq!(queue.ahead_of(normal), Some(0));
}

#[test]
fn a_normal_job_starts_when_the_fast_lane_waits_on_its_own_cap() {
    let now = Instant::now();
    let mut queue = Queue::default();
    for each in ["a", "b", "c"] {
        queue.push(each, Lane::Fast);
    }
    assert_eq!(
        admitted(&mut queue, Some(&calm()), &reserving(4, 32), now).len(),
        3
    );
    let fast = queue.push("fast", Lane::Fast);
    let normal = queue.push("normal", Lane::Normal);
    assert_eq!(
        admitted(&mut queue, Some(&calm()), &reserving(4, 32), now),
        [(normal, 8)]
    );
    assert_eq!(queue.ahead_of(fast), Some(0));
}

#[test]
fn a_finished_job_no_longer_adds_its_threads_to_the_load() {
    let now = Instant::now();
    let mut queue = Queue::default();
    let busy = reading(Pressure::Normal, 9.0);
    let first = queue.push("first", Lane::Normal);
    let second = queue.push("second", Lane::Normal);
    let third = queue.push("third", Lane::Normal);
    assert_eq!(
        admitted(&mut queue, Some(&busy), &limits(4, 16), now),
        [(first, 4), (second, 4)]
    );
    assert!(admitted(&mut queue, Some(&busy), &limits(4, 16), now).is_empty());
    queue.remove(first);
    assert_eq!(
        admitted(&mut queue, Some(&busy), &limits(4, 16), now),
        [(third, 4)]
    );
}

#[test]
fn the_queue_starts_the_next_job_when_a_running_job_is_removed() {
    let now = Instant::now();
    let mut queue = Queue::default();
    let first = queue.push("first", Lane::Normal);
    let second = queue.push("second", Lane::Normal);
    assert_eq!(
        admitted(&mut queue, Some(&calm()), &limits(1, 16), now),
        [(first, 16)]
    );
    assert!(admitted(&mut queue, Some(&calm()), &limits(1, 16), now).is_empty());
    assert_eq!(queue.remove(first).map(|(job, _)| job), Some("first"));
    assert_eq!(
        admitted(&mut queue, Some(&calm()), &limits(1, 16), now),
        [(second, 16)]
    );
    assert_eq!(queue.running_job(second), Some(&"second"));
}

#[test]
fn a_removed_waiting_job_never_starts_and_the_jobs_behind_it_move_up() {
    let now = Instant::now();
    let mut queue = Queue::default();
    let first = queue.push("first", Lane::Normal);
    let second = queue.push("second", Lane::Normal);
    let third = queue.push("third", Lane::Normal);
    admitted(&mut queue, Some(&calm()), &limits(1, 16), now);
    assert_eq!(queue.ahead_of(third), Some(1));
    assert_eq!(queue.remove(second).map(|(job, _)| job), Some("second"));
    assert_eq!(queue.ahead_of(third), Some(0));
    assert_eq!(queue.remove(first).map(|(job, _)| job), Some("first"));
    assert_eq!(
        admitted(&mut queue, Some(&calm()), &limits(1, 16), now),
        [(third, 16)]
    );
}

#[test]
fn the_queue_starts_one_job_under_pressure_when_nothing_runs() {
    let now = Instant::now();
    let mut queue = Queue::default();
    let first = queue.push("first", Lane::Normal);
    queue.push("second", Lane::Normal);
    let warn = reading(Pressure::Warn, 1.0);
    assert_eq!(
        admitted(&mut queue, Some(&warn), &limits(4, 16), now),
        [(first, 4)]
    );
}

#[test]
fn each_hold_says_why_in_one_line() {
    assert_eq!(
        Hold::Full { max_jobs: 2 }.to_string(),
        "the daemon already runs 2 jobs, its most at once"
    );
    assert_eq!(
        Hold::Full { max_jobs: 1 }.to_string(),
        "the daemon already runs 1 job, its most at once"
    );
    assert_eq!(
        Hold::Lane { places: 1 }.to_string(),
        "the job's lane already runs 1 job, and the last place stays open for the other lane"
    );
    assert_eq!(
        Hold::Lane { places: 3 }.to_string(),
        "the job's lane already runs 3 jobs, and the last place stays open for the other lane"
    );
    assert_eq!(
        Hold::Memory {
            pressure: Pressure::Warn
        }
        .to_string(),
        "memory pressure is warn"
    );
    assert_eq!(
        Hold::Load {
            projected: 18.0,
            cores: 16
        }
        .to_string(),
        "the 1-minute load with the jobs started in the last minute is 18.0, over the 16 cores"
    );
    assert_eq!(
        Hold::Disk {
            free_bytes: 12 * GIB,
            total_bytes: 1000 * GIB
        }
        .to_string(),
        "free disk is 12 GiB, 1% of the disk, under 20 GiB or 3%"
    );
}

#[test]
fn removing_a_running_job_returns_when_it_started_and_its_threads() {
    let mut queue = Queue::default();
    let limits = limits(2, 16);
    let at = Instant::now();
    let id = queue.push("job", Lane::Normal);
    admitted(&mut queue, None, &limits, at);
    assert_eq!(queue.remove(id), Some(("job", Some((at, Threads::new(8))))));
}

#[test]
fn removing_a_waiting_job_returns_no_start() {
    let mut queue = Queue::default();
    let id = queue.push("job", Lane::Normal);
    assert_eq!(queue.remove(id), Some(("job", None)));
}

#[test]
fn removing_a_job_the_queue_does_not_hold_returns_nothing() {
    let mut queue: Queue<&str> = Queue::default();
    let id = queue.push("job", Lane::Normal);
    queue.remove(id);
    assert_eq!(queue.remove(id), None);
}

#[test]
fn a_seated_job_fills_a_place_without_waiting() {
    let mut queue = Queue::default();
    let limits = limits(2, 16);
    let at = Instant::now();
    queue.seat("seated", Lane::Normal, Threads::new(8), at, &limits);
    assert_eq!(queue.running().collect::<Vec<_>>(), vec![&"seated"]);
    assert_eq!(queue.waiting().count(), 0);
    let second = queue.push("second", Lane::Normal);
    queue.push("third", Lane::Normal);
    assert_eq!(admitted(&mut queue, None, &limits, at), [(second, 8)]);
    assert_eq!(queue.held(), Some(Hold::Full { max_jobs: 2 }));
}

#[test]
fn a_seat_past_max_jobs_holds_new_jobs_until_it_leaves() {
    let mut queue = Queue::default();
    let limits = limits(1, 16);
    let at = Instant::now();
    let first = queue.seat("first", Lane::Normal, Threads::new(16), at, &limits);
    let second = queue.seat("second", Lane::Normal, Threads::new(16), at, &limits);
    assert_eq!(queue.running().count(), 2);
    let waiting = queue.push("waiting", Lane::Normal);
    assert!(admitted(&mut queue, None, &limits, at).is_empty());
    assert_eq!(queue.held(), Some(Hold::Full { max_jobs: 1 }));
    queue.remove(first);
    assert!(admitted(&mut queue, None, &limits, at).is_empty());
    assert_eq!(queue.held(), Some(Hold::Full { max_jobs: 1 }));
    // The twin: the same job starts once the last seat has left.
    queue.remove(second);
    assert_eq!(admitted(&mut queue, None, &limits, at), [(waiting, 16)]);
}

#[test]
fn a_seat_younger_than_recent_adds_its_threads_to_the_load() {
    let mut queue = Queue::default();
    let limits = limits(8, 16);
    let busy = reading(Pressure::Normal, 14.0);
    let at = Instant::now();
    queue.seat("seated", Lane::Normal, Threads::new(2), at, &limits);
    queue.push("waiting", Lane::Normal);
    let young = at + RECENT - Duration::from_secs(1);
    assert!(admitted(&mut queue, Some(&busy), &limits, young).is_empty());
    assert_eq!(
        queue.held(),
        Some(Hold::Load {
            projected: 16.0,
            cores: 16
        })
    );
}

#[test]
fn a_seat_older_than_recent_adds_nothing_to_the_load() {
    let mut queue = Queue::default();
    let limits = limits(8, 16);
    let busy = reading(Pressure::Normal, 14.0);
    let at = Instant::now();
    queue.seat("seated", Lane::Normal, Threads::new(2), at, &limits);
    let waiting = queue.push("waiting", Lane::Normal);
    let old = at + RECENT + Duration::from_secs(1);
    assert_eq!(
        admitted(&mut queue, Some(&busy), &limits, old),
        [(waiting, 2)]
    );
}

#[test]
fn a_seat_counts_at_most_the_threads_of_one_grant() {
    let mut queue = Queue::default();
    let limits = limits(8, 16);
    let busy = reading(Pressure::Normal, 13.0);
    let at = Instant::now();
    queue.seat(
        "seated",
        Lane::Normal,
        Threads::new(usize::MAX),
        at,
        &limits,
    );
    let waiting = queue.push("waiting", Lane::Normal);
    // A load of 13 and one grant's 2 threads project 15 of 16 cores.
    assert_eq!(
        admitted(&mut queue, Some(&busy), &limits, at),
        [(waiting, 2)]
    );
}

#[test]
fn seated_jobs_list_in_the_order_they_started() {
    let mut queue = Queue::default();
    let limits = limits(4, 16);
    let at = Instant::now();
    queue.push("granted", Lane::Normal);
    admitted(&mut queue, None, &limits, at + Duration::from_secs(5));
    queue.seat(
        "late",
        Lane::Normal,
        Threads::new(1),
        at + Duration::from_secs(9),
        &limits,
    );
    queue.seat("early", Lane::Normal, Threads::new(1), at, &limits);
    assert_eq!(
        queue.running().copied().collect::<Vec<_>>(),
        ["early", "granted", "late"]
    );
}

#[test]
fn the_running_jobs_list_when_they_started() {
    let mut queue = Queue::default();
    let limits = limits(2, 16);
    let at = Instant::now();
    queue.push("job", Lane::Normal);
    admitted(&mut queue, None, &limits, at);
    assert_eq!(
        queue.running_since().collect::<Vec<_>>(),
        vec![(&"job", at)]
    );
}
