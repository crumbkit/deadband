//! Tests of the check that the daemon on the socket belongs to the user who
//! asks, before the client sends it a command line.

use std::fs::{DirBuilder, Permissions};
use std::io::{self, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::thread::{self, JoinHandle};

use rustix::process::getuid;

use super::{ANSWER_WITHIN, job, scripted};
use crate::client::{
    Admission, Unasked, ask_checked, check_directory, connect_checked, jobs_checked, peer_uid,
};
use crate::daemon::DirectoryFault;
use crate::history::tests::Scratch;
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

/// Makes a directory under `scratch` with exactly `mode`.
fn directory_with(scratch: &Scratch, name: &str, mode: u32) -> std::path::PathBuf {
    let directory = scratch.path().join(name);
    DirBuilder::new()
        .create(&directory)
        .expect("the directory is made");
    std::fs::set_permissions(&directory, Permissions::from_mode(mode)).expect("the mode is set");
    directory
}

#[test]
fn a_socket_in_a_directory_of_another_user_is_not_asked() {
    let scratch = Scratch::new();
    let directory = directory_with(&scratch, "deadband", 0o700);
    let refused = check_directory(&directory.join("deadband.sock"), own() + 1)
        .expect_err("another user's directory is refused");
    assert!(
        matches!(
            refused,
            Unasked::Directory {
                fault: DirectoryFault::Owner { .. },
                ..
            }
        ),
        "{refused:?}"
    );
    check_directory(&directory.join("deadband.sock"), own())
        .expect("the twin: the caller's own directory is asked");
}

#[test]
fn a_socket_in_a_directory_that_anyone_can_write_to_is_not_asked() {
    let scratch = Scratch::new();
    let open = directory_with(&scratch, "open", 0o777);
    let refused = check_directory(&open.join("deadband.sock"), own())
        .expect_err("a world-writable directory is refused");
    assert!(
        matches!(
            refused,
            Unasked::Directory {
                fault: DirectoryFault::Mode { .. },
                ..
            }
        ),
        "{refused:?}"
    );
    let line = refused.to_string();
    assert!(line.contains("not asked") && !line.contains('\n'), "{line}");
    let sticky = directory_with(&scratch, "sticky", 0o1777);
    check_directory(&sticky.join("deadband.sock"), own())
        .expect("the twin: a sticky directory such as /tmp is asked");
    let readable = directory_with(&scratch, "readable", 0o755);
    check_directory(&readable.join("deadband.sock"), own())
        .expect("the twin: a directory only its owner writes is asked");
}

#[test]
fn a_job_starts_at_once_when_the_socket_is_in_a_directory_anyone_can_write_to() {
    let scratch = Scratch::new();
    let open = directory_with(&scratch, "open", 0o777);
    let path = open.join("deadband.sock");
    let _listener = UnixListener::bind(&path).expect("the socket binds");
    let admission = ask_checked(&path, &job(), ANSWER_WITHIN, |_, _| {}, None, same);
    assert!(
        matches!(admission, Admission::Open(Unasked::Directory { .. })),
        "{admission:?}"
    );
}

#[test]
fn a_listener_with_a_full_queue_makes_the_client_fail_open_instead_of_waiting() {
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketType, bind, connect, listen, socket};

    let path = socket_path();
    let address = SocketAddrUnix::new(&*path).expect("the address is valid");
    let listener =
        socket(AddressFamily::UNIX, SocketType::STREAM, None).expect("the socket is made");
    bind(&listener, &address).expect("the socket binds");
    listen(&listener, 0).expect("the socket listens");
    // The listener never accepts, so connections fill its queue until one
    // does not complete.
    let mut queued = Vec::new();
    loop {
        let filler =
            socket(AddressFamily::UNIX, SocketType::STREAM, None).expect("the socket is made");
        rustix::fs::fcntl_setfl(&filler, rustix::fs::OFlags::NONBLOCK)
            .expect("the socket is nonblocking");
        let full = connect(&filler, &address).is_err();
        queued.push(filler);
        if full {
            break;
        }
    }
    let answer = connect_checked(&path, same);
    assert!(answer.is_err(), "a full queue let the client connect");
}
