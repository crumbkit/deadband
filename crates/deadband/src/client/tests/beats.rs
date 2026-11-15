//! Tests of the deadlines of an ask: the `Ask` line and the first answer
//! share one, and a queued client waits on a daemon that promised beats only
//! while its lines keep arriving.

use std::io::Write as _;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::super::{Admission, Unasked, Waits, ask, ask_reasking};
use super::{ANSWER_WITHIN, Peer, SHORT_WAIT, beating, job, next_connection, pause, reasking};
use crate::admit::Threads;
use crate::protocol::{Job, Reply, Request, write_line};
use crate::testing::socket_path;

/// How long a test client waits for a line from a daemon that promised
/// beats.
const SILENCE: Duration = Duration::from_millis(300);

/// How long a test client keeps asking after it loses a daemon that queued
/// it.
const REASK: Duration = Duration::from_millis(300);

/// Asks for `job` on `path` with `waits` on a thread of its own, and
/// returns the receiver of its admission.
fn asking(path: &Path, job: Job, waits: Waits) -> mpsc::Receiver<Admission> {
    let asked = path.to_path_buf();
    let (sender, admitted) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(ask_reasking(&asked, &job, waits, |_, _| {}, None));
    });
    admitted
}

/// Accepts the next connection on `listener`, checks that it asks for a
/// job that wants beats, and queues the job with `beats` as the promise.
fn queue(listener: &UnixListener, beats: bool) -> Peer {
    let mut peer = next_connection(listener, ANSWER_WITHIN).expect("the client connects");
    assert_eq!(peer.request(), Request::Ask(beating()));
    let queued = Reply::Queued {
        ahead: 1,
        held: None,
        beats,
    };
    write_line(&mut peer.stream, &queued).expect("the queue place writes");
    peer
}

/// Writes a grant of 3 threads to `peer`.
fn grant(peer: &mut Peer) {
    let granted = Reply::Granted {
        threads: Threads::new(3),
    };
    write_line(&mut peer.stream, &granted).expect("the grant writes");
}

#[test]
fn a_queued_client_whose_daemon_promised_beats_and_went_silent_starts_its_job() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let waits = Waits {
        silence: SILENCE,
        ..reasking(REASK)
    };
    let admitted = asking(&path, beating(), waits);
    let _peer = queue(&listener, true);
    let admission = admitted
        .recv_timeout(ANSWER_WITHIN)
        .expect("the silence ends the wait");
    let Admission::Open(unasked) = admission else {
        panic!("a silent daemon granted the job");
    };
    assert!(matches!(unasked, Unasked::Hung { .. }), "{unasked:?}");
    assert_eq!(
        unasked.to_string(),
        "the daemon queued the job and then sent nothing for 300 ms"
    );
}

#[test]
fn a_queued_client_whose_daemon_keeps_beating_waits_past_the_silence_for_its_grant() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let waits = Waits {
        silence: SILENCE,
        ..reasking(REASK)
    };
    let admitted = asking(&path, beating(), waits);
    let mut peer = queue(&listener, true);
    // Nine beats 100 ms apart keep the client waiting three times as long
    // as the silence it allows.
    for _ in 0..9 {
        pause(Duration::from_millis(100));
        write_line(&mut peer.stream, &Reply::Beat).expect("the beat writes");
    }
    grant(&mut peer);
    let admission = admitted.recv_timeout(ANSWER_WITHIN).expect("the ask ends");
    assert!(matches!(admission, Admission::Granted(_)), "{admission:?}");
}

#[test]
fn a_queued_client_waits_with_no_deadline_on_a_daemon_that_promised_no_beats() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let waits = Waits {
        silence: SILENCE,
        ..reasking(REASK)
    };
    let admitted = asking(&path, beating(), waits);
    let mut peer = queue(&listener, false);
    pause(SILENCE * 3);
    grant(&mut peer);
    let admission = admitted.recv_timeout(ANSWER_WITHIN).expect("the ask ends");
    assert!(matches!(admission, Admission::Granted(_)), "{admission:?}");
}

#[test]
fn a_daemon_that_queues_the_job_again_and_beats_gives_its_next_loss_a_window_of_its_own() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let admitted = asking(&path, beating(), reasking(REASK));
    drop(queue(&listener, true));
    let mut second = queue(&listener, true);
    write_line(&mut second.stream, &Reply::Beat).expect("the beat writes");
    // The second daemon serves the job for longer than the window that
    // the first loss opened, and the client keeps waiting on it.
    pause(REASK * 2);
    assert!(!second.closed_now(), "the client left the second daemon");
    drop(second);
    let mut third = next_connection(&listener, ANSWER_WITHIN).expect("the client asks again");
    assert_eq!(third.request(), Request::Ask(beating()));
    grant(&mut third);
    let admission = admitted.recv_timeout(ANSWER_WITHIN).expect("the ask ends");
    assert!(matches!(admission, Admission::Granted(_)), "{admission:?}");
}

#[test]
fn an_ask_that_the_daemon_never_reads_gives_up_when_the_first_answer_is_due() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let (held, hold) = mpsc::channel::<()>();
    thread::spawn(move || {
        let connection = listener.accept();
        let _ = hold.recv();
        drop(connection);
    });
    // A command line of 4 MiB fills any socket buffer that a daemon which
    // never reads leaves.
    let long = Job {
        command: "x".repeat(4 << 20),
        ..job()
    };
    let (sender, finished) = mpsc::channel();
    let asked = path.to_path_buf();
    thread::spawn(move || {
        let _ = sender.send(ask(&asked, &long, SHORT_WAIT, |_, _| {}));
    });
    let admission = finished
        .recv_timeout(ANSWER_WITHIN)
        .expect("the deadline ends the write");
    drop(held);
    assert!(
        matches!(admission, Admission::Open(Unasked::Silent { .. })),
        "{admission:?}"
    );
}

#[test]
fn an_ask_whose_daemon_drips_its_first_answer_gives_up_at_the_deadline() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("the client connects");
        while stream.write_all(b" ").is_ok() {
            pause(Duration::from_millis(5));
        }
    });
    let (sender, finished) = mpsc::channel();
    let asked = path.to_path_buf();
    thread::spawn(move || {
        let _ = sender.send(ask(&asked, &job(), SHORT_WAIT, |_, _| {}));
    });
    let admission = finished
        .recv_timeout(ANSWER_WITHIN)
        .expect("the deadline ends the wait");
    assert!(
        matches!(admission, Admission::Open(Unasked::Silent { .. })),
        "{admission:?}"
    );
}
