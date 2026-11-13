//! Tests of the check that the daemon on the socket belongs to the user who
//! asks, before the client sends it a command line.

use std::io::{self, BufReader};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::thread::{self, JoinHandle};

use rustix::process::getuid;

use super::{ANSWER_WITHIN, job, scripted};
use crate::client::{Admission, Unasked, ask_checked, jobs_checked, peer_uid};
use crate::protocol::{Jobs, Reply, Request, read_line};
use crate::testing::socket_path;

/// Returns the uid of the user who runs the tests.
fn own() -> u32 {
    getuid().as_raw()
}

/// The reader of a peer that belongs to another user.
fn foreign(_: &UnixStream) -> io::Result<u32> {
    Ok(own() + 1)
}

/// The reader of a peer that belongs to this user.
fn same(_: &UnixStream) -> io::Result<u32> {
    Ok(own())
}

/// The reader of a peer whose credentials the system will not give.
fn unreadable(_: &UnixStream) -> io::Result<u32> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// Listens on `path` for one connection, and returns the request that the
/// client sent on it, or `None` when the client closed it without one.
fn first_request(path: &Path) -> JoinHandle<Option<Request>> {
    let listener = UnixListener::bind(path).expect("the test socket binds");
    thread::spawn(move || {
        let (stream, _) = listener.accept().expect("the client connects");
        read_line::<Request>(&mut BufReader::new(stream)).expect("the read ends")
    })
}

#[test]
fn a_daemon_of_another_user_is_not_sent_the_command_line_and_the_job_starts() {
    let path = socket_path();
    let received = first_request(&path);
    let admission = ask_checked(&path, &job(), ANSWER_WITHIN, |_, _| {}, None, foreign);
    let Admission::Open(unasked) = admission else {
        panic!("a daemon of another user granted the job");
    };
    assert!(
        matches!(unasked, Unasked::Foreign { uid, .. } if uid == own() + 1),
        "{unasked:?}"
    );
    assert_eq!(received.join().expect("the daemon thread ends"), None);
}

#[test]
fn a_daemon_of_the_same_user_is_sent_the_command_line_and_grants_the_job() {
    let (path, received) = scripted(vec![Reply::Granted { jobs: 1 }]);
    let admission = ask_checked(&path, &job(), ANSWER_WITHIN, |_, _| {}, None, same);
    assert!(matches!(admission, Admission::Granted(_)), "{admission:?}");
    assert_eq!(
        received.recv().expect("the ask arrives"),
        Request::Ask(job())
    );
}

#[test]
fn the_refusal_says_in_one_line_whose_daemon_it_was() {
    let path = socket_path();
    let unasked = Unasked::Foreign {
        path: path.to_path_buf(),
        uid: 7,
    };
    let line = unasked.to_string();
    assert!(!line.contains('\n'), "{line}");
    assert!(line.contains(&path.display().to_string()), "{line}");
    assert!(line.contains("uid 7"), "{line}");
    assert!(line.contains("not sent"), "{line}");
}

#[test]
fn status_of_a_daemon_of_another_user_sends_nothing_and_says_why() {
    let path = socket_path();
    let received = first_request(&path);
    let answer = jobs_checked(&path, ANSWER_WITHIN, foreign);
    assert!(matches!(answer, Err(Unasked::Foreign { .. })), "{answer:?}");
    assert_eq!(received.join().expect("the daemon thread ends"), None);
}

#[test]
fn status_of_a_daemon_of_the_same_user_asks_and_reads_the_jobs() {
    let listed = Jobs {
        max_jobs: 2,
        running: Vec::new(),
        waiting: Vec::new(),
        held: None,
        reclaim: None,
        reclaiming: None,
        stuck: Vec::new(),
        busiest: None,
        unreadable: false,
    };
    let (path, received) = scripted(vec![Reply::Jobs(listed.clone())]);
    let answer = jobs_checked(&path, ANSWER_WITHIN, same).expect("the daemon answers");
    assert_eq!(answer, listed);
    assert_eq!(
        received.recv().expect("the request arrives"),
        Request::Status
    );
}

#[test]
fn a_peer_whose_credentials_cannot_be_read_starts_the_job() {
    let path = socket_path();
    let received = first_request(&path);
    let admission = ask_checked(&path, &job(), ANSWER_WITHIN, |_, _| {}, None, unreadable);
    assert!(
        matches!(admission, Admission::Open(Unasked::Unreachable { .. })),
        "{admission:?}"
    );
    assert_eq!(received.join().expect("the daemon thread ends"), None);
}

#[test]
fn the_system_reports_this_user_as_the_peer_of_a_socket_this_user_made() {
    let (client, _server) = UnixStream::pair().expect("the pair is made");
    assert_eq!(peer_uid(&client).expect("the uid reads"), own());
}
