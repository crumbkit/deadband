//! Tests of the client against no daemon and against scripted daemons.

use std::io::BufReader;
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::lease::Seat;
use super::{Admission, Lease, Unasked, ask, jobs, timeline};
use crate::admit::{Hold, Lane};
use crate::protocol::{Ago, Job, Jobs, Reply, Request, Timeline, read_line, write_line};
use crate::testing::{TestSocket, socket_path};

/// How long the tests let a scripted daemon take to answer.
const ANSWER_WITHIN: Duration = Duration::from_secs(10);

/// How long the test of a silent daemon waits for its answer.
const SHORT_WAIT: Duration = Duration::from_millis(50);

fn job() -> Job {
    Job {
        label: "build".to_owned(),
        pid: 9,
        command: "make".to_owned(),
        lane: Lane::Normal,
    }
}

/// Starts a daemon on a new socket that reads one request, writes
/// `replies`, closes its side of the connection, and then sends each further request it reads to the returned
/// receiver until the client closes the connection.
fn scripted(replies: Vec<Reply>) -> (TestSocket, mpsc::Receiver<Request>) {
    script(replies, true)
}

/// Starts a daemon as [`scripted`] does that keeps its side of the
/// connection open, as a daemon does while a granted job runs.
fn granting(replies: Vec<Reply>) -> (TestSocket, mpsc::Receiver<Request>) {
    script(replies, false)
}

/// Starts the daemon of [`scripted`] and [`granting`]. `close_write` says
/// whether it closes its side of the connection after `replies`.
fn script(replies: Vec<Reply>, close_write: bool) -> (TestSocket, mpsc::Receiver<Request>) {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let (sender, received) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("the client connects");
        let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
        let first: Request = read_line(&mut reader).expect("reads").expect("a request");
        sender.send(first).expect("the test receives");
        for reply in &replies {
            write_line(&mut stream, reply).expect("the reply writes");
        }
        // The daemon has said all it will, so the client reads the end.
        if close_write {
            stream
                .shutdown(Shutdown::Write)
                .expect("the write side closes");
        }
        while let Ok(Some(request)) = read_line::<Request>(&mut reader) {
            sender.send(request).expect("the test receives");
        }
    });
    (path, received)
}

#[test]
fn ask_without_a_daemon_starts_the_job_and_says_the_daemon_is_not_running() {
    let path = socket_path();
    let Admission::Open(unasked) = ask(&path, &job(), ANSWER_WITHIN, |_, _| {}) else {
        panic!("a missing daemon granted the job");
    };
    assert!(matches!(unasked, Unasked::NotRunning { .. }));
    assert_eq!(
        unasked.to_string(),
        format!(
            "the daemon is not running (nothing listens on {})",
            path.display()
        )
    );
}

#[test]
fn ask_treats_a_socket_file_that_no_daemon_answers_on_as_no_daemon() {
    let path = socket_path();
    drop(UnixListener::bind(&path).expect("the stale socket binds"));
    let Admission::Open(unasked) = ask(&path, &job(), ANSWER_WITHIN, |_, _| {}) else {
        panic!("a stale socket granted the job");
    };
    assert!(matches!(unasked, Unasked::NotRunning { .. }), "{unasked:?}");
}

#[test]
fn ask_starts_the_job_when_the_daemon_does_not_answer_in_time() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let (held, hold) = mpsc::channel::<()>();
    thread::spawn(move || {
        let connection = listener.accept();
        let _ = hold.recv();
        drop(connection);
    });
    let Admission::Open(unasked) = ask(&path, &job(), SHORT_WAIT, |_, _| {}) else {
        panic!("a silent daemon granted the job");
    };
    drop(held);
    assert_eq!(
        unasked.to_string(),
        "the daemon did not answer within 50 ms"
    );
}

#[test]
fn ask_starts_the_job_when_the_daemon_closes_the_connection_while_it_waits() {
    let held = Hold::Full { max_jobs: 1 };
    let (path, received) = scripted(vec![Reply::Queued {
        ahead: 2,
        held: Some(held),
    }]);
    let mut queued = None;
    let admission = ask(&path, &job(), ANSWER_WITHIN, |n, why| {
        queued = Some((n, why));
    });
    assert_eq!(
        received.recv().expect("the ask arrives"),
        Request::Ask(job())
    );
    assert_eq!(queued, Some((2, Some(held))));
    assert!(
        matches!(admission, Admission::Open(Unasked::Closed)),
        "{admission:?}"
    );
}

#[test]
fn ask_returns_a_lease_when_the_daemon_grants_the_job_after_it_waits() {
    let queued = Reply::Queued {
        ahead: 0,
        held: None,
    };
    let (path, received) = granting(vec![queued, Reply::Granted { jobs: 5 }]);
    let Admission::Granted(lease) = ask(&path, &job(), ANSWER_WITHIN, |_, _| {}) else {
        panic!("the daemon's grant was not taken");
    };
    assert_eq!(lease.jobs(), 5);
    lease.end(Some(4));
    assert_eq!(
        received.recv().expect("the ask arrives"),
        Request::Ask(job())
    );
    assert_eq!(
        received.recv().expect("the end arrives"),
        Request::Done {
            exit_code: Some(4),
            signal: None
        }
    );
}

#[test]
fn ask_starts_the_job_when_the_daemon_answers_with_a_reply_the_client_does_not_expect() {
    let jobs = Jobs {
        max_jobs: 1,
        running: Vec::new(),
        waiting: Vec::new(),
        held: None,
        reclaim: None,
    };
    let (path, _received) = scripted(vec![Reply::Jobs(jobs)]);
    let admission = ask(&path, &job(), ANSWER_WITHIN, |_, _| {});
    assert!(
        matches!(admission, Admission::Open(Unasked::Garbled { .. })),
        "{admission:?}"
    );
}

#[test]
fn jobs_returns_the_daemons_jobs() {
    let listed = Jobs {
        max_jobs: 2,
        running: vec![job()],
        waiting: Vec::new(),
        held: None,
        reclaim: None,
    };
    let (path, received) = scripted(vec![Reply::Jobs(listed.clone())]);
    assert_eq!(
        jobs(&path, ANSWER_WITHIN).expect("the daemon answers"),
        listed
    );
    assert_eq!(
        received.recv().expect("the request arrives"),
        Request::Status
    );
}

#[test]
fn jobs_without_a_daemon_says_the_daemon_is_not_running() {
    let error = jobs(&socket_path(), ANSWER_WITHIN).expect_err("no daemon answers");
    assert!(matches!(error, Unasked::NotRunning { .. }));
}

/// A timeline with no jobs.
fn empty_timeline() -> Timeline {
    Timeline {
        up: Ago::from_millis(5),
        jobs: Vec::new(),
        hidden: 0,
        held: None,
    }
}

#[test]
fn timeline_asks_the_daemon_and_returns_its_reply() {
    let (path, received) = scripted(vec![Reply::Timeline(empty_timeline())]);
    let reply = timeline(&path, ANSWER_WITHIN).expect("the daemon answers");
    assert_eq!(reply, empty_timeline());
    assert_eq!(
        received.recv().expect("the request arrives"),
        Request::Timeline
    );
}

#[test]
fn timeline_without_a_daemon_says_it_is_not_running() {
    let path = socket_path();
    assert!(matches!(
        timeline(&path, SHORT_WAIT),
        Err(Unasked::NotRunning { .. })
    ));
}

#[test]
fn timeline_from_a_daemon_that_closes_without_a_reply_says_closed() {
    let (path, _received) = scripted(Vec::new());
    assert!(matches!(
        timeline(&path, ANSWER_WITHIN),
        Err(Unasked::Closed)
    ));
}

#[test]
fn timeline_from_a_daemon_that_hangs_gives_up_after_the_wait() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let hold = thread::spawn(move || listener.accept());
    assert!(matches!(
        timeline(&path, SHORT_WAIT),
        Err(Unasked::Silent { .. })
    ));
    drop(hold);
}

#[test]
fn timeline_from_a_daemon_that_sends_garbage_says_garbled() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("the client connects");
        let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
        let _: Option<Request> = read_line(&mut reader).expect("the request reads");
        std::io::Write::write_all(&mut stream, b"not json\n").expect("the line writes");
        stream
    });
    assert!(matches!(
        timeline(&path, ANSWER_WITHIN),
        Err(Unasked::Garbled { .. })
    ));
}

#[test]
fn timeline_refuses_a_reply_of_the_wrong_kind() {
    let (path, _received) = scripted(vec![Reply::Granted { jobs: 1 }]);
    assert!(matches!(
        timeline(&path, ANSWER_WITHIN),
        Err(Unasked::Garbled { .. })
    ));
}

#[test]
fn a_lease_reports_an_exit_code_from_the_status_it_ends_with() {
    use std::os::unix::process::ExitStatusExt;
    let (path, received) = granting(vec![Reply::Granted { jobs: 1 }]);
    let Admission::Granted(lease) = ask(&path, &job(), ANSWER_WITHIN, |_, _| {}) else {
        panic!("the daemon granted the job");
    };
    received.recv().expect("the ask arrives");
    lease.end_status(std::process::ExitStatus::from_raw(3 << 8));
    assert_eq!(
        received.recv().expect("the end arrives"),
        Request::Done {
            exit_code: Some(3),
            signal: None
        }
    );
}

#[test]
fn a_lease_reports_the_signal_that_ended_the_job() {
    use std::os::unix::process::ExitStatusExt;
    let (path, received) = granting(vec![Reply::Granted { jobs: 1 }]);
    let Admission::Granted(lease) = ask(&path, &job(), ANSWER_WITHIN, |_, _| {}) else {
        panic!("the daemon granted the job");
    };
    received.recv().expect("the ask arrives");
    lease.end_status(std::process::ExitStatus::from_raw(9));
    assert_eq!(
        received.recv().expect("the end arrives"),
        Request::Done {
            exit_code: None,
            signal: Some(9)
        }
    );
}

#[test]
fn timeline_from_a_daemon_that_drips_bytes_stops_at_the_deadline() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("the client connects");
        while std::io::Write::write_all(&mut stream, b" ").is_ok() {
            #[expect(
                clippy::disallowed_methods,
                reason = "the fake daemon drips one byte at a time, with no event to wait on"
            )]
            thread::sleep(Duration::from_millis(5));
        }
    });
    let (sender, finished) = mpsc::channel();
    let asked = path.to_path_buf();
    thread::spawn(move || {
        let _ = sender.send(timeline(&asked, SHORT_WAIT));
    });
    let answer = finished
        .recv_timeout(ANSWER_WITHIN)
        .expect("the deadline ends the wait");
    assert!(matches!(answer, Err(Unasked::Silent { .. })), "{answer:?}");
}

/// How long the first answer may take in a test that watches for a read
/// timeout left on the connection. The test stays quiet for longer.
const QUIET_ANSWER: Duration = Duration::from_secs(1);

/// One connection that a scripted daemon accepted.
struct Peer {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Peer {
    /// Reads the next request.
    fn request(&mut self) -> Request {
        read_line(&mut self.reader)
            .expect("the request reads in time")
            .expect("a request arrives before the connection closes")
    }

    /// Reports whether the client has already closed the connection. Any
    /// data or a client that holds the connection open reports `false`.
    fn closed_now(&mut self) -> bool {
        // macOS refuses this call once the peer has closed. The read then
        // returns at once.
        let _ = self.stream.set_read_timeout(Some(Duration::from_millis(1)));
        matches!(read_line::<Request>(&mut self.reader), Ok(None))
    }
}

/// Accepts the next connection on `listener`, or returns `None` when none
/// arrives `within`.
fn next_connection(listener: &UnixListener, within: Duration) -> Option<Peer> {
    let listener = listener.try_clone().expect("the listener clones");
    let (sender, accepted) = mpsc::channel();
    thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let _ = sender.send(stream);
        }
    });
    let stream = accepted.recv_timeout(within).ok()?;
    stream
        .set_read_timeout(Some(ANSWER_WITHIN))
        .expect("the timeout sets");
    let reader = BufReader::new(stream.try_clone().expect("the stream clones"));
    Some(Peer { stream, reader })
}

/// Grants a job with `jobs` threads to a client that asks on `path`, and
/// returns its lease and the daemon's end of the connection.
fn lease_on(
    listener: &UnixListener,
    path: &Path,
    jobs: usize,
    answer_within: Duration,
) -> (Lease, Peer) {
    let asked = path.to_path_buf();
    let asking = thread::spawn(move || ask(&asked, &job(), answer_within, |_, _| {}));
    let mut peer = next_connection(listener, ANSWER_WITHIN).expect("the client connects");
    assert_eq!(peer.request(), Request::Ask(job()));
    write_line(&mut peer.stream, &Reply::Granted { jobs }).expect("the grant writes");
    let Admission::Granted(lease) = asking.join().expect("the ask ends") else {
        panic!("the daemon granted the job");
    };
    (lease, peer)
}

/// Lets `length` pass. The channel's sender stays alive, so the receive
/// always takes the whole length.
fn pause(length: Duration) {
    let (_sender, never) = mpsc::channel::<()>();
    let _ = never.recv_timeout(length);
}

#[test]
fn a_lease_seats_again_with_its_job_and_threads_after_the_connection_closes() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let (_lease, peer) = lease_on(&listener, &path, 5, ANSWER_WITHIN);
    drop(peer);
    let mut next = next_connection(&listener, ANSWER_WITHIN).expect("the lease connects again");
    let Request::Seat {
        job: seated,
        jobs,
        ran_ms,
    } = next.request()
    else {
        panic!("the lease did not send a seat");
    };
    assert_eq!(seated, job());
    assert_eq!(jobs, 5);
    // The lease waits 100 ms after the connection closes, and the job has
    // run at least that long.
    assert!(ran_ms >= 100, "{ran_ms}");
}

#[test]
fn the_end_of_a_seated_job_reaches_the_new_daemon() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let (lease, peer) = lease_on(&listener, &path, 5, ANSWER_WITHIN);
    drop(peer);
    let mut next = next_connection(&listener, ANSWER_WITHIN).expect("the lease connects again");
    assert!(matches!(next.request(), Request::Seat { .. }));
    lease.end(Some(4));
    assert_eq!(
        next.request(),
        Request::Done {
            exit_code: Some(4),
            signal: None
        }
    );
}

#[test]
fn a_lease_opens_no_second_connection_while_the_first_stays_open() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let (_lease, _peer) = lease_on(&listener, &path, 5, QUIET_ANSWER);
    // The wait outlasts the read timeout of the first answer, which the
    // lease must not take for the daemon going away.
    assert!(next_connection(&listener, QUIET_ANSWER * 2).is_none());
}

#[test]
fn a_lease_closes_the_old_connection_before_the_new_one_carries_its_seat() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let (_lease, mut first) = lease_on(&listener, &path, 5, ANSWER_WITHIN);
    first
        .stream
        .shutdown(Shutdown::Write)
        .expect("the write side closes");
    let mut second = next_connection(&listener, ANSWER_WITHIN).expect("the lease connects again");
    assert!(matches!(second.request(), Request::Seat { .. }));
    assert!(first.closed_now());
}

#[test]
fn a_lease_tries_again_when_a_daemon_closes_the_seat() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let (_lease, peer) = lease_on(&listener, &path, 5, ANSWER_WITHIN);
    drop(peer);
    let mut second = next_connection(&listener, ANSWER_WITHIN).expect("the lease connects again");
    assert!(matches!(second.request(), Request::Seat { .. }));
    drop(second);
    let mut third = next_connection(&listener, ANSWER_WITHIN).expect("the lease tries again");
    assert!(matches!(third.request(), Request::Seat { .. }));
}

#[test]
fn a_lease_ends_at_once_with_no_daemon_listening() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let (lease, peer) = lease_on(&listener, &path, 5, ANSWER_WITHIN);
    drop(peer);
    drop(listener);
    std::fs::remove_file(&*path).expect("the socket file goes");
    // Several connections fail in this time, one per 100 ms.
    pause(Duration::from_millis(250));
    let begun = Instant::now();
    lease.end(Some(0));
    assert!(begun.elapsed() < Duration::from_secs(1));
}

#[test]
fn a_dropped_lease_closes_its_connection() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let (lease, mut peer) = lease_on(&listener, &path, 5, ANSWER_WITHIN);
    drop(lease);
    assert_eq!(
        read_line::<Request>(&mut peer.reader).expect("the end reads in time"),
        None
    );
}

#[test]
fn a_lease_that_ended_connects_no_more() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let (lease, peer) = lease_on(&listener, &path, 5, ANSWER_WITHIN);
    lease.end(Some(0));
    drop(peer);
    assert!(next_connection(&listener, Duration::from_millis(300)).is_none());
}

#[test]
fn a_connection_that_opens_after_the_job_ended_is_closed() {
    let (held, _other) = UnixStream::pair().expect("the pair opens");
    let (late, theirs) = UnixStream::pair().expect("the pair opens");
    theirs
        .set_read_timeout(Some(ANSWER_WITHIN))
        .expect("the timeout sets");
    let seat = Seat::new(held);
    seat.end();
    assert!(!seat.install(late));
    assert_eq!(
        read_line::<Request>(&mut BufReader::new(theirs)).expect("the end reads in time"),
        None
    );
}
