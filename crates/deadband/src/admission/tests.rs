//! Tests of admission against no daemon and against scripted daemons.

use std::io::BufReader;
use std::net::Shutdown;
use std::num::NonZeroUsize;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::{Admitted, Asked, Notices, admit};
use crate::admit::Lane;
use crate::environment::{Environment, HELD};
use crate::protocol::{Reply, Request, read_line, write_line};
use crate::testing::{TestSocket, socket_path};
use crate::throttle::{NORMAL_NICE, own_priority};

/// How long a test waits for a scripted daemon to hear from the client.
const WAIT: Duration = Duration::from_secs(10);

/// A configuration directory, removed when the test ends, whose `priority`
/// entry the test chooses.
struct Config(PathBuf);

impl Config {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("db-adm-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("the config directory is made");
        Self(path)
    }

    /// Writes the priority file with `words`.
    fn with_words(self, words: &str) -> Self {
        #[expect(
            clippy::disallowed_methods,
            reason = "the test writes the priority file a person would write"
        )]
        std::fs::write(self.priority(), words).expect("the priority file is written");
        self
    }

    /// Makes the priority file a directory, which cannot be read as text.
    fn unreadable(self) -> Self {
        std::fs::create_dir(self.priority()).expect("the directory is made");
        self
    }

    fn priority(&self) -> PathBuf {
        self.0.join("priority")
    }

    fn environment(&self) -> Environment {
        Environment {
            config: Some(self.0.clone()),
            ..Environment::default()
        }
    }
}

impl Drop for Config {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Starts a daemon on a new socket that reads one request, writes
/// `replies`, and then sends each further request it reads to the returned
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
        while let Ok(Some(request)) = read_line::<Request>(&mut reader) {
            sender.send(request).expect("the test receives");
        }
    });
    (path, received)
}

/// Starts a daemon on a new socket that accepts one client, reads its
/// request and then closes the connection without an answer.
fn closing() -> TestSocket {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    thread::spawn(move || {
        let (stream, _) = listener.accept().expect("the client connects");
        let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
        let _ = read_line::<Request>(&mut reader);
        stream.shutdown(Shutdown::Both).expect("the stream closes");
    });
    path
}

fn cores(count: usize) -> Option<NonZeroUsize> {
    NonZeroUsize::new(count)
}

/// Admits a job labelled `build` with `cores` cores, returning what
/// `admit` reported.
fn admitted(
    socket: &Path,
    environment: &Environment,
    lane: Option<Lane>,
) -> (Admitted, Vec<String>) {
    let asked = Asked {
        socket,
        environment,
        cores: cores(8),
        label: "build",
        command: "make all",
        lane,
        notices: None,
    };
    let mut lines = Vec::new();
    let result = admit(&asked, &mut |line: &str| lines.push(line.to_owned()));
    (result, lines)
}

/// The variables of a job with `jobs` threads that the daemon did not grant.
fn capped(jobs: usize) -> Vec<(&'static str, String)> {
    Environment::default().job_variables(jobs, false)
}

#[test]
fn admit_with_a_grant_holds_the_lease_and_gives_the_granted_threads() {
    let (socket, received) = scripted(vec![Reply::Granted { jobs: 3 }]);
    let environment = Environment::default();
    let (result, lines) = admitted(&socket, &environment, None);
    assert!(lines.is_empty(), "{lines:?}");
    assert_eq!(result.lane(), Some(Lane::Normal));
    assert_eq!(result.variables(), environment.job_variables(3, true));
    assert!(result.variables().iter().any(|(name, _)| *name == HELD));
    let Request::Ask(job) = received.recv_timeout(WAIT).expect("the daemon was asked") else {
        panic!("the first request was not an ask");
    };
    assert_eq!(job.label, "build");
    assert_eq!(job.command, "make all");
    assert_eq!(job.pid, std::process::id());
    assert_eq!(job.lane, Lane::Normal);
}

#[test]
fn end_tells_the_daemon_the_exit_code_of_a_granted_job() {
    let (socket, received) = scripted(vec![Reply::Granted { jobs: 2 }]);
    let (result, _) = admitted(&socket, &Environment::default(), None);
    let _ = received.recv_timeout(WAIT).expect("the ask arrives");
    result.end(Some(7));
    assert_eq!(
        received.recv_timeout(WAIT).expect("the daemon is told"),
        Request::Done {
            exit_code: Some(7),
            signal: None
        }
    );
}

#[test]
fn end_status_tells_the_daemon_the_signal_that_ended_a_granted_job() {
    use std::os::unix::process::ExitStatusExt;
    let (socket, received) = scripted(vec![Reply::Granted { jobs: 2 }]);
    let (result, _) = admitted(&socket, &Environment::default(), None);
    let _ = received.recv_timeout(WAIT).expect("the ask arrives");
    result.end_status(std::process::ExitStatus::from_raw(15));
    assert_eq!(
        received.recv_timeout(WAIT).expect("the daemon is told"),
        Request::Done {
            exit_code: None,
            signal: Some(15)
        }
    );
}

#[test]
fn end_status_without_a_lease_tells_nobody_and_returns() {
    use std::os::unix::process::ExitStatusExt;
    let socket = socket_path();
    let (result, _) = admitted(&socket, &Environment::default(), None);
    result.end_status(std::process::ExitStatus::from_raw(0));
}

#[test]
fn end_without_a_lease_tells_nobody_and_returns() {
    let socket = socket_path();
    let (result, _) = admitted(&socket, &Environment::default(), None);
    result.end(Some(0));
    let environment = Environment {
        held: true,
        ..Environment::default()
    };
    let (result, _) = admitted(&socket, &environment, None);
    result.end(None);
}

#[test]
fn admit_that_queues_reports_the_wait_and_then_holds_the_grant() {
    let (socket, _received) = scripted(vec![
        Reply::Queued {
            ahead: 2,
            held: None,
        },
        Reply::Granted { jobs: 2 },
    ]);
    let environment = Environment::default();
    let (result, lines) = admitted(&socket, &environment, None);
    assert_eq!(
        lines,
        ["the job waits for headroom, and 2 jobs wait ahead of it"]
    );
    assert_eq!(result.variables(), environment.job_variables(2, true));
}

#[test]
fn admit_without_a_daemon_starts_the_job_with_capped_threads_and_says_why() {
    let socket = socket_path();
    let (result, lines) = admitted(&socket, &Environment::default(), None);
    assert_eq!(
        lines,
        [format!(
            "the daemon is not running (nothing listens on {}), so the job starts now",
            socket.display()
        )]
    );
    // Eight cores and a default of two jobs give four threads.
    assert_eq!(result.variables(), capped(4));
    assert!(result.variables().iter().all(|(name, _)| *name != HELD));
    assert_eq!(result.lane(), Some(Lane::Normal));
}

#[test]
fn admit_without_a_daemon_in_the_fast_lane_caps_the_threads_one_higher() {
    let socket = socket_path();
    let (result, _) = admitted(&socket, &Environment::default(), Some(Lane::Fast));
    assert_eq!(result.variables(), capped(5));
    assert_eq!(result.lane(), Some(Lane::Fast));
}

#[test]
fn admit_starts_the_job_when_the_daemon_closes_the_connection() {
    let socket = closing();
    let (result, lines) = admitted(&socket, &Environment::default(), None);
    assert_eq!(
        lines,
        ["the daemon closed the connection before it started the job, so the job starts now"]
    );
    assert_eq!(result.variables(), capped(4));
}

#[test]
fn admit_starts_the_job_when_the_daemon_answers_with_something_else() {
    let (socket, _received) = scripted(vec![Reply::Jobs(crate::protocol::Jobs {
        max_jobs: 1,
        running: Vec::new(),
        waiting: Vec::new(),
        held: None,
        reclaim: None,
        stuck: Vec::new(),
        busiest: None,
        unreadable: false,
    })]);
    let (result, lines) = admitted(&socket, &Environment::default(), None);
    let [line] = lines.as_slice() else {
        panic!("{lines:?}");
    };
    assert!(
        line.starts_with("the daemon's answer could not be read: "),
        "{line}"
    );
    assert!(line.ends_with(", so the job starts now"), "{line}");
    assert_eq!(result.variables(), capped(4));
}

#[test]
fn admit_starts_the_job_when_the_daemon_does_not_answer_in_time() {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    let (result, lines) = admitted(&path, &Environment::default(), None);
    assert_eq!(
        lines,
        ["the daemon did not answer within 2000 ms, so the job starts now"]
    );
    assert_eq!(result.variables(), capped(4));
    drop(listener);
}

#[test]
fn admit_starts_the_job_when_the_socket_cannot_be_reached() {
    // A path longer than a socket address holds cannot be connected to.
    let long = PathBuf::from(format!("/tmp/{}", "x".repeat(200)));
    let (result, lines) = admitted(&long, &Environment::default(), None);
    let [line] = lines.as_slice() else {
        panic!("{lines:?}");
    };
    assert!(line.ends_with(", so the job starts now"), "{line}");
    assert!(line.contains("could not be reached"), "{line}");
    assert_eq!(result.variables(), capped(4));
}

/// Asserts that `admit` asked nothing, reported nothing and returned no
/// lease, lane or variables, with a daemon on `socket` that would have
/// seen the client.
fn assert_asks_nothing(environment: &Environment) {
    let socket = socket_path();
    let listener = UnixListener::bind(&socket).expect("the test socket binds");
    listener.set_nonblocking(true).expect("the listener is set");
    let (result, lines) = admitted(&socket, environment, Some(Lane::Fast));
    assert!(lines.is_empty(), "{lines:?}");
    assert_eq!(result.lane(), None);
    assert!(result.variables().is_empty());
    let contacted = listener.accept();
    assert!(
        matches!(&contacted, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "the daemon was contacted: {contacted:?}"
    );
    result.end(Some(0));
}

#[test]
fn admit_inside_a_granted_job_asks_nothing() {
    assert_asks_nothing(&Environment {
        held: true,
        ..Environment::default()
    });
}

#[test]
fn admit_with_bypass_asks_nothing() {
    assert_asks_nothing(&Environment {
        bypass: true,
        ..Environment::default()
    });
}

#[test]
fn admit_takes_the_fast_lane_when_the_priority_file_matches_the_label() {
    let config = Config::new().with_words("build\n");
    let socket = socket_path();
    let (result, lines) = admitted(&socket, &config.environment(), None);
    assert_eq!(result.lane(), Some(Lane::Fast));
    assert_eq!(lines.len(), 1, "{lines:?}");
}

#[test]
fn admit_stays_in_the_normal_lane_when_the_priority_file_does_not_match() {
    let config = Config::new().with_words("release\n");
    let socket = socket_path();
    let (result, _) = admitted(&socket, &config.environment(), None);
    assert_eq!(result.lane(), Some(Lane::Normal));
}

#[test]
fn admit_stays_in_the_normal_lane_when_there_is_no_priority_file() {
    let config = Config::new();
    let socket = socket_path();
    let (result, lines) = admitted(&socket, &config.environment(), None);
    assert_eq!(result.lane(), Some(Lane::Normal));
    assert_eq!(lines.len(), 1, "{lines:?}");
}

#[test]
fn admit_with_an_unreadable_priority_file_says_why_and_waits_in_the_normal_lane() {
    let config = Config::new().unreadable();
    let error = std::fs::read_to_string(config.priority()).expect_err("a directory is unreadable");
    let socket = socket_path();
    let (result, lines) = admitted(&socket, &config.environment(), None);
    assert_eq!(result.lane(), Some(Lane::Normal));
    assert_eq!(
        lines.first().map(String::as_str),
        Some(
            format!(
                "deadband could not read {}: {error}, so the job waits in the normal lane",
                config.priority().display()
            )
            .as_str()
        )
    );
}

#[test]
fn admit_with_an_explicit_lane_ignores_the_priority_file() {
    let matching = Config::new().with_words("build\n");
    let socket = socket_path();
    let (result, _) = admitted(&socket, &matching.environment(), Some(Lane::Normal));
    assert_eq!(result.lane(), Some(Lane::Normal));

    let unreadable = Config::new().unreadable();
    let (result, lines) = admitted(&socket, &unreadable.environment(), Some(Lane::Fast));
    assert_eq!(result.lane(), Some(Lane::Fast));
    assert_eq!(
        lines.len(),
        1,
        "only the missing daemon is reported: {lines:?}"
    );
}

/// Returns what `lower_priority` reported for a job in `lane`.
fn lowered(lane: Option<Lane>, environment: &Environment) -> Vec<String> {
    let socket = socket_path();
    let (result, _) = admitted(&socket, environment, lane);
    let mut lines = Vec::new();
    result.lower_priority(&mut |line: &str| lines.push(line.to_owned()));
    lines
}

#[test]
fn lower_priority_in_the_normal_lane_lowers_the_calling_process_to_nice_5() {
    let lines = lowered(Some(Lane::Normal), &Environment::default());
    assert!(lines.is_empty(), "{lines:?}");
    assert!(own_priority().expect("the priority reads") >= NORMAL_NICE);
}

#[test]
fn lower_priority_in_the_fast_lane_leaves_the_priority_alone() {
    let before = own_priority().expect("the priority reads");
    let lines = lowered(Some(Lane::Fast), &Environment::default());
    assert!(lines.is_empty(), "{lines:?}");
    assert_eq!(own_priority().expect("the priority reads"), before);
}

#[test]
fn lower_priority_without_a_lane_leaves_the_priority_alone() {
    let before = own_priority().expect("the priority reads");
    let environment = Environment {
        held: true,
        ..Environment::default()
    };
    let lines = lowered(None, &environment);
    assert!(lines.is_empty(), "{lines:?}");
    assert_eq!(own_priority().expect("the priority reads"), before);
}

/// Admits a job on a scripted daemon that grants it, and returns the job
/// the daemon received.
fn asked_job(notices: Option<&Notices>) -> Request {
    let (socket, received) = scripted(vec![Reply::Granted { jobs: 2 }]);
    let environment = Environment::default();
    let asked = Asked {
        socket: &socket,
        environment: &environment,
        cores: cores(8),
        label: "build",
        command: "make all",
        lane: None,
        notices,
    };
    let admitted = admit(&asked, &mut |_: &str| {});
    drop(admitted);
    received
        .recv_timeout(Duration::from_secs(10))
        .expect("a request")
}

#[test]
fn a_caller_that_passes_notices_asks_for_a_watched_job() {
    let notices = Notices::new(|_| {});
    let request = asked_job(Some(&notices));
    assert!(matches!(request, Request::Ask(job) if job.watched));
}

#[test]
fn a_caller_that_passes_no_notices_asks_for_an_unwatched_job() {
    let request = asked_job(None);
    assert!(matches!(request, Request::Ask(job) if !job.watched));
}
