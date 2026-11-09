//! Tests of the client against no daemon and against scripted daemons.

use std::io::BufReader;
use std::net::Shutdown;
use std::os::unix::net::UnixListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{Admission, Unasked, ask, jobs, timeline};
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
        stream
            .shutdown(Shutdown::Write)
            .expect("the write side closes");
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
    let (path, received) = scripted(vec![queued, Reply::Granted { jobs: 5 }]);
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
    let (path, received) = scripted(vec![Reply::Granted { jobs: 1 }]);
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
    let (path, received) = scripted(vec![Reply::Granted { jobs: 1 }]);
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
