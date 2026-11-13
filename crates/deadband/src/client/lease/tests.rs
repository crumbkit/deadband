//! Tests of how a lease ends, and of a lease that nothing can watch.

use std::hint::spin_loop;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Barrier, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use super::{Lease, Notices, Seat, SeatState, UNWATCHED};
use crate::admit::Lane;
use crate::protocol::{Job, Request, read_line};
use crate::testing::socket_path;

/// How long a test waits for a line that must arrive.
const WITHIN: Duration = Duration::from_secs(10);

fn job() -> Job {
    Job {
        label: "build".to_owned(),
        pid: 9,
        command: "make".to_owned(),
        lane: Lane::Normal,
        watched: false,
        cwd: None,
        workspace: None,
        short: None,
    }
}

/// Returns the `Seat` request that a watcher sends to a new daemon.
fn seat_request() -> Request {
    Request::Seat {
        job: job(),
        jobs: 1,
        ran_ms: 0,
    }
}

/// Returns a lease of one thread that holds `stream`.
fn holding(stream: UnixStream) -> Lease {
    Lease {
        seat: Arc::new(Seat::new(stream)),
        jobs: 1,
    }
}

/// Returns a reader of the daemon's end of a connection, which gives up
/// after [`WITHIN`].
fn daemon_end(stream: UnixStream) -> BufReader<UnixStream> {
    stream
        .set_read_timeout(Some(WITHIN))
        .expect("the timeout sets");
    BufReader::new(stream)
}

/// Reads every request until the connection closes.
fn requests(reader: &mut BufReader<UnixStream>) -> Vec<Request> {
    let mut all = Vec::new();
    while let Some(request) = read_line::<Request>(reader).expect("the requests read in time") {
        all.push(request);
    }
    all
}

#[test]
fn ending_a_held_lease_writes_one_done_and_closes_the_connection() {
    let (client, daemon) = UnixStream::pair().expect("the pair connects");
    let mut daemon = daemon_end(daemon);
    holding(client).end(Some(2));
    assert_eq!(
        requests(&mut daemon),
        vec![Request::Done {
            exit_code: Some(2),
            signal: None,
            usage: None,
        }]
    );
}

#[test]
fn a_lost_lease_that_ends_while_a_new_daemon_seats_it_never_shuts_the_new_connection_untold() {
    // Each round ends a lost lease while another thread installs a new
    // connection. The installer starts a little later in each round, so the
    // rounds cover the whole time the end takes.
    for round in 0..2_000_u32 {
        let (old, _old_daemon) = UnixStream::pair().expect("the pair connects");
        let lease = holding(old);
        lease.seat.lose();
        let seat = Arc::clone(&lease.seat);
        let (new, new_daemon) = UnixStream::pair().expect("the pair connects");
        let mut new_daemon = daemon_end(new_daemon);
        let start = Arc::new(Barrier::new(2));
        let installer = {
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                for _ in 0..(round % 200) {
                    spin_loop();
                }
                seat.install(new, &seat_request())
            })
        };
        start.wait();
        lease.end(Some(0));
        let installed = installer.join().expect("the install ends");
        let told = requests(&mut new_daemon);
        if installed {
            assert_eq!(
                told,
                vec![
                    seat_request(),
                    Request::Done {
                        exit_code: Some(0),
                        signal: None,
                        usage: None,
                    }
                ],
                "round {round}: the end shut the new connection without telling it"
            );
        } else {
            assert!(told.is_empty(), "round {round}: {told:?}");
        }
    }
}

#[test]
fn a_seat_that_ended_while_lost_refuses_the_next_connection() {
    let (old, _old_daemon) = UnixStream::pair().expect("the pair connects");
    let lease = holding(old);
    lease.seat.lose();
    let seat = Arc::clone(&lease.seat);
    lease.end(None);
    let (late, late_daemon) = UnixStream::pair().expect("the pair connects");
    // macOS refuses a read timeout once the peer has closed, so it is set
    // before the install.
    let mut late_daemon = daemon_end(late_daemon);
    assert!(!seat.install(late, &seat_request()));
    assert!(requests(&mut late_daemon).is_empty());
}

#[test]
fn a_lease_that_cannot_be_watched_says_so_and_the_job_still_ends_with_the_daemon() {
    let (client, daemon) = UnixStream::pair().expect("the pair connects");
    let mut daemon = daemon_end(daemon);
    let (sender, lines) = mpsc::channel();
    let sender = Mutex::new(sender);
    let notices = Notices::new(move |line| {
        let _ = sender.lock().expect("locks").send(line.to_owned());
    });
    crate::spawn::refuse("deadband-lease");
    let lease = Lease::watched(client, 3, &socket_path(), &job(), Some(&notices));
    crate::spawn::allow();
    assert_eq!(lines.try_recv().as_deref(), Ok(UNWATCHED));
    assert!(
        lines.try_recv().is_err(),
        "one line says the lease is unwatched"
    );
    assert_eq!(lease.jobs(), 3);
    lease.end(Some(0));
    assert_eq!(
        requests(&mut daemon),
        vec![Request::Done {
            exit_code: Some(0),
            signal: None,
            usage: None,
        }]
    );
}

#[test]
fn a_seat_installed_before_the_job_ends_gets_the_seat_and_then_the_done() {
    let (old, _old_daemon) = UnixStream::pair().expect("the pair connects");
    let lease = holding(old);
    lease.seat.lose();
    let (new, new_daemon) = UnixStream::pair().expect("the pair connects");
    let mut new_daemon = daemon_end(new_daemon);

    assert!(lease.seat.install(new, &seat_request()));
    lease.end(Some(0));

    assert_eq!(
        requests(&mut new_daemon),
        vec![
            seat_request(),
            Request::Done {
                exit_code: Some(0),
                signal: None,
                usage: None,
            }
        ]
    );
}

#[test]
fn a_seat_write_that_fails_leaves_the_seat_lost() {
    let (old, _old_daemon) = UnixStream::pair().expect("the pair connects");
    let lease = holding(old);
    lease.seat.lose();
    let (new, new_daemon) = UnixStream::pair().expect("the pair connects");
    drop(new_daemon);

    assert!(!lease.seat.install(new, &seat_request()));

    assert!(matches!(*lease.seat.lock(), SeatState::Lost));
}
