//! Tests of the replies that the daemon writes to a job's client: the daemon
//! decides them under the queue lock and writes them after it releases the
//! lock, so a client that does not read holds up no other job.

use std::io::{self, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::super::seat::ask;
use super::super::{SAMPLE_EVERY, Shared, WRITE_TIMEOUT, Waiter, lock};
use super::timeline::{row, state};
use super::{REPLY_WITHIN, job, queued};
use crate::admit::Lane;
use crate::protocol::{JobState, Reply, Request, read_line, write_line};

/// How long a test lets taking the queue lock take while another client
/// does not read. It is well under [`WRITE_TIMEOUT`], so a lock held across
/// the write to the client that does not read misses it.
const PROMPTLY: Duration = Duration::from_millis(300);

/// How long the next job's grant may take while another client does not
/// read: one sample of the daemon ([`SAMPLE_EVERY`], 1 s) with half a second
/// for a loaded machine.
const A_SAMPLE: Duration = SAMPLE_EVERY.saturating_add(Duration::from_millis(500));

/// Fills the buffer of the connection that `ours` writes to until a write
/// would wait, as a client that never reads leaves it, and gives `ours` the
/// write timeout that the daemon gives each connection.
fn fill(ours: &UnixStream) {
    ours.set_nonblocking(true)
        .expect("the stream turns nonblocking");
    let chunk = [b' '; 4096];
    loop {
        match (&mut &*ours).write(&chunk) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("filling the buffer failed: {error}"),
        }
    }
    ours.set_nonblocking(false)
        .expect("the stream turns blocking");
    ours.set_write_timeout(Some(WRITE_TIMEOUT))
        .expect("the timeout sets");
}

/// Queues a job labelled `label` on `shared` without granting it, whose
/// client never reads, and returns the client's end of the connection.
fn push_stuck(shared: &Shared, label: &str) -> UnixStream {
    let (ours, theirs) = UnixStream::pair().expect("the pair opens");
    fill(&ours);
    lock(shared).queue.push(
        Waiter {
            job: job(label),
            stream: ours.into(),
            queued: Instant::now(),
            arrival: None,
            ahead: 0,
        },
        Lane::Normal,
    );
    theirs
}

/// Starts a connection thread that asks for a job labelled `label` on
/// `shared`, as the daemon's accept loop does, and returns the client's end.
fn ask_on_thread(shared: &Shared, label: &str) -> (UnixStream, thread::JoinHandle<()>) {
    let (ours, theirs) = UnixStream::pair().expect("the pair opens");
    ours.set_write_timeout(Some(WRITE_TIMEOUT))
        .expect("the timeout sets");
    let shared = Arc::clone(shared);
    let job = job(label);
    let serving = thread::spawn(move || {
        let mut reader = BufReader::new(ours.try_clone().expect("the stream clones"));
        ask(&shared, ours, &mut reader, job);
    });
    (theirs, serving)
}

/// Starts a connection thread that asks for a job labelled `label` on
/// `shared` for a client that never reads, and returns the client's end.
/// The thread holds the connection open while it waits for the job's end.
fn ask_stuck_on_thread(shared: &Shared, label: &str) -> (UnixStream, thread::JoinHandle<()>) {
    let (ours, theirs) = UnixStream::pair().expect("the pair opens");
    fill(&ours);
    let shared = Arc::clone(shared);
    let job = job(label);
    let serving = thread::spawn(move || {
        let mut reader = BufReader::new(ours.try_clone().expect("the stream clones"));
        ask(&shared, ours, &mut reader, job);
    });
    (theirs, serving)
}

/// Waits until the job labelled `label` runs on `shared`. Each look takes
/// the queue lock, which is an observable step.
fn until_running(shared: &Shared, label: &str) {
    let begun = Instant::now();
    loop {
        let timeline = lock(shared).timeline(Instant::now());
        let running = timeline.jobs.iter().any(|row| {
            row.label.as_str() == label && matches!(row.state, JobState::Running { .. })
        });
        if running {
            return;
        }
        assert!(begun.elapsed() < REPLY_WITHIN, "{timeline:?}");
    }
}

/// Reads one reply from the client's `reader` within `within`.
fn reply_within(
    reader: &mut BufReader<&UnixStream>,
    within: Duration,
) -> io::Result<Option<Reply>> {
    reader.get_ref().set_read_timeout(Some(within))?;
    read_line(reader)
}

/// Reads `client` to its end, and reports whether the daemon closed it
/// within `within`.
fn closed_within(mut client: &UnixStream, within: Duration) -> bool {
    let begun = Instant::now();
    let mut buffer = [0; 4096];
    // macOS refuses the timeout once the daemon has shut the connection
    // down, and the read then returns at once.
    let _ = client.set_read_timeout(Some(within));
    while begun.elapsed() < within {
        match client.read(&mut buffer) {
            Ok(0) => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

/// Waits until the job labelled `label` has left the queue of `shared`
/// without running, and returns the longest that taking the queue lock
/// took meanwhile, or `None` when the job did not leave within
/// [`REPLY_WITHIN`]. Each look takes the queue lock, which is an observable
/// step.
fn left(shared: &Shared, label: &str) -> Option<Duration> {
    let begun = Instant::now();
    let mut longest = Duration::ZERO;
    while begun.elapsed() < REPLY_WITHIN {
        let asked = Instant::now();
        let state = lock(shared);
        longest = longest.max(asked.elapsed());
        let timeline = state.timeline(Instant::now());
        drop(state);
        let gone = timeline
            .jobs
            .iter()
            .any(|row| row.label.as_str() == label && matches!(row.state, JobState::Left { .. }));
        if gone {
            return Some(longest);
        }
    }
    None
}

/// Returns how long taking the queue lock of `shared` took.
fn lock_wait(shared: &Shared) -> Duration {
    let begun = Instant::now();
    drop(lock(shared));
    begun.elapsed()
}

#[test]
fn a_client_that_never_reads_its_grant_does_not_delay_the_next_grant() {
    let shared: Shared = Arc::new(Mutex::new(state(2)));
    let stuck = push_stuck(&shared, "stuck");
    let begun = Instant::now();
    let (next, serving) = ask_on_thread(&shared, "next");
    let longest = left(&shared, "stuck").expect("the stuck job leaves");
    assert!(
        longest < PROMPTLY,
        "the queue lock waited {longest:?} while the stuck client's write timed out"
    );
    let left_for_reply = A_SAMPLE
        .saturating_sub(begun.elapsed())
        .max(Duration::from_millis(1));
    let reply = reply_within(&mut BufReader::new(&next), left_for_reply);
    assert!(
        matches!(reply, Ok(Some(Reply::Granted { .. }))),
        "the next job waited more than a sample: {reply:?}"
    );
    assert!(begun.elapsed() < A_SAMPLE);
    assert!(closed_within(&stuck, REPLY_WITHIN));
    write_line(
        &mut &next,
        &Request::Done {
            exit_code: Some(0),
            signal: None,
            usage: None,
        },
    )
    .expect("the end writes");
    serving.join().expect("the connection thread ends");
}

#[test]
fn a_grant_that_cannot_be_written_frees_its_place_once_and_closes_the_connection() {
    let shared: Shared = Arc::new(Mutex::new(state(1)));
    let (stuck, stuck_serving) = ask_stuck_on_thread(&shared, "stuck");
    until_running(&shared, "stuck");
    let (next, serving) = ask_on_thread(&shared, "next");
    let mut reader = BufReader::new(&next);
    let reply = reply_within(&mut reader, REPLY_WITHIN).expect("the daemon replies in time");
    assert_eq!(reply, Some(queued(0, 1)));
    assert!(left(&shared, "stuck").is_some());
    assert!(closed_within(&stuck, REPLY_WITHIN));
    stuck_serving.join().expect("the stuck job's thread ends");
    let reply = reply_within(&mut reader, REPLY_WITHIN).expect("the daemon replies in time");
    assert!(matches!(reply, Some(Reply::Granted { .. })), "{reply:?}");
    drop(reader);
    let timeline = lock(&shared).timeline(Instant::now());
    let rows = timeline
        .jobs
        .iter()
        .filter(|row| row.label.as_str() == "stuck")
        .count();
    assert_eq!(rows, 1, "{timeline:?}");
    assert!(
        matches!(row(&timeline, "stuck").state, JobState::Left { .. }),
        "{timeline:?}"
    );
    drop(next);
    serving.join().expect("the connection thread ends");
    let timeline = lock(&shared).timeline(Instant::now());
    assert_eq!(timeline.jobs.len(), 2, "{timeline:?}");
}

#[test]
fn a_queued_reply_that_waits_on_its_client_leaves_the_queue_lock_free() {
    let shared: Shared = Arc::new(Mutex::new(state(1)));
    let (first, first_serving) = ask_on_thread(&shared, "first");
    assert!(matches!(
        reply_within(&mut BufReader::new(&first), REPLY_WITHIN),
        Ok(Some(Reply::Granted { .. }))
    ));
    let (theirs, serving) = ask_stuck_on_thread(&shared, "stuck");
    let begun = Instant::now();
    let mut longest = Duration::ZERO;
    while !serving.is_finished() && begun.elapsed() < REPLY_WITHIN {
        longest = longest.max(lock_wait(&shared));
    }
    assert!(longest < PROMPTLY, "the queue lock waited {longest:?}");
    assert!(closed_within(&theirs, REPLY_WITHIN));
    serving.join().expect("the connection thread ends");
    drop(first);
    first_serving.join().expect("the connection thread ends");
}

#[test]
fn a_client_that_reads_gets_its_queued_reply_and_then_its_grant() {
    let shared: Shared = Arc::new(Mutex::new(state(1)));
    let (first, first_serving) = ask_on_thread(&shared, "first");
    assert!(matches!(
        reply_within(&mut BufReader::new(&first), REPLY_WITHIN),
        Ok(Some(Reply::Granted { .. }))
    ));
    let (second, serving) = ask_on_thread(&shared, "second");
    let mut reader = BufReader::new(&second);
    let reply = reply_within(&mut reader, REPLY_WITHIN).expect("the daemon replies in time");
    assert_eq!(reply, Some(queued(0, 1)));
    drop(first);
    first_serving.join().expect("the connection thread ends");
    let reply = reply_within(&mut reader, REPLY_WITHIN).expect("the daemon replies in time");
    assert!(matches!(reply, Some(Reply::Granted { .. })), "{reply:?}");
    drop(reader);
    drop(second);
    serving.join().expect("the connection thread ends");
}
