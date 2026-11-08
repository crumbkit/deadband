//! Tests of the client against no daemon and against scripted daemons.

use std::io::BufReader;
use std::net::Shutdown;
use std::os::unix::net::UnixListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{Admission, Unasked, ask, jobs};
use crate::protocol::{Job, Jobs, Reply, Request, read_line, write_line};
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
    let Admission::Open(unasked) = ask(&path, &job(), ANSWER_WITHIN, |_| {}) else {
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
    let Admission::Open(unasked) = ask(&path, &job(), ANSWER_WITHIN, |_| {}) else {
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
    let Admission::Open(unasked) = ask(&path, &job(), SHORT_WAIT, |_| {}) else {
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
    let (path, received) = scripted(vec![Reply::Queued { ahead: 2 }]);
    let mut ahead = None;
    let admission = ask(&path, &job(), ANSWER_WITHIN, |n| ahead = Some(n));
    assert_eq!(
        received.recv().expect("the ask arrives"),
        Request::Ask(job())
    );
    assert_eq!(ahead, Some(2));
    assert!(
        matches!(admission, Admission::Open(Unasked::Closed)),
        "{admission:?}"
    );
}

#[test]
fn ask_returns_a_lease_when_the_daemon_grants_the_job_after_it_waits() {
    let (path, received) = scripted(vec![Reply::Queued { ahead: 0 }, Reply::Granted]);
    let Admission::Granted(lease) = ask(&path, &job(), ANSWER_WITHIN, |_| {}) else {
        panic!("the daemon's grant was not taken");
    };
    lease.end(Some(4));
    assert_eq!(
        received.recv().expect("the ask arrives"),
        Request::Ask(job())
    );
    assert_eq!(
        received.recv().expect("the end arrives"),
        Request::Done { exit_code: Some(4) }
    );
}

#[test]
fn ask_starts_the_job_when_the_daemon_answers_with_a_reply_the_client_does_not_expect() {
    let jobs = Jobs {
        max_jobs: 1,
        running: Vec::new(),
        waiting: Vec::new(),
    };
    let (path, _received) = scripted(vec![Reply::Jobs(jobs)]);
    let admission = ask(&path, &job(), ANSWER_WITHIN, |_| {});
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
