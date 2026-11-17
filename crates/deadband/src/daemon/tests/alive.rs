//! The daemon ends when its socket file is gone or replaced or when its
//! sampler panics, and it admits on a reading that has gone stale. It
//! writes no log line with the queue lock held, and it empties a standard
//! error file that has grown past its limit.

use std::fs::File;
use std::io::{self, Read as _, Write};
use std::num::NonZeroUsize;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::super::alive::{Ending, LOG_LIMIT, age, standard_error_as, trim};
use super::super::{Log, Settings, Shared, bind, lock, serve};
use super::timeline::shared;
use super::{CORES, Client, Dial, REPLY_WITHIN, SAMPLE_EVERY, granted, jobs, start};
use crate::admit::Hold;
use crate::history::Recorder;
use crate::history::tests::Scratch;
use crate::probe::{Disk, Pressure, Probe, Sample, Unreadable};
use crate::testing::{TestSocket, socket_path};

/// A probe that reads once and panics at every later sample.
#[derive(Clone)]
struct PanicsAfterOne(Arc<AtomicUsize>);

impl Probe for PanicsAfterOne {
    fn sample(&self) -> Result<Sample, Unreadable> {
        assert!(
            self.0.fetch_add(1, Ordering::SeqCst) == 0,
            "the test probe panics at its second sample"
        );
        Ok(Sample {
            load_1m: 1.0,
            memory_pressure: Pressure::Normal,
            swap_used_bytes: 0,
            swap_total_bytes: 0,
        })
    }

    fn disk(&self, _path: &Path) -> io::Result<Disk> {
        Err(io::Error::other("the test probe reads no disk"))
    }
}

/// Starts a daemon on a new socket with `probe`, and returns the socket and
/// a channel that receives the error `serve` returns.
fn serving(probe: impl Probe + Clone + Send + 'static) -> (TestSocket, Receiver<io::Error>) {
    let path = socket_path();
    let ends = serving_at(&path, probe);
    (path, ends)
}

/// Starts a daemon as [`serving`] does on the socket at `path`.
fn serving_at(path: &Path, probe: impl Probe + Clone + Send + 'static) -> Receiver<io::Error> {
    serving_refusing(path, probe, None)
}

/// Starts a daemon as [`serving`] does on the socket at `path`, where the
/// operating system refuses each thread named `refused` that `serve` starts.
fn serving_refusing(
    path: &Path,
    probe: impl Probe + Clone + Send + 'static,
    refused: Option<&'static str>,
) -> Receiver<io::Error> {
    serving_logged(path, probe, refused, Arc::new(Mutex::new(Vec::new())), None)
}

/// Starts a daemon as [`serving_refusing`] does, which writes its log lines
/// to `log` and takes `standard_error` as its standard error.
fn serving_logged(
    path: &Path,
    probe: impl Probe + Clone + Send + 'static,
    refused: Option<&'static str>,
    log: Log,
    standard_error: Option<File>,
) -> Receiver<io::Error> {
    let bound = bind(path).expect("the test socket binds");
    let settings = Settings {
        sample_every: SAMPLE_EVERY,
        ..Settings::new(
            NonZeroUsize::new(2).expect("two is positive"),
            NonZeroUsize::new(CORES),
            None,
            None,
        )
    };
    let (ended, ends) = mpsc::channel();
    thread::spawn(move || {
        let bound = bound;
        if let Some(name) = refused {
            crate::spawn::refuse(name);
        }
        if let Some(file) = standard_error {
            standard_error_as(file);
        }
        let _ = ended.send(serve(&bound, probe, settings, &log));
    });
    ends
}

/// A probe that tells the test each time it is sampled.
#[derive(Clone)]
struct Tells(Dial, Sender<()>);

impl Probe for Tells {
    fn sample(&self) -> Result<Sample, Unreadable> {
        let _ = self.1.send(());
        self.0.sample()
    }

    fn disk(&self, path: &Path) -> io::Result<Disk> {
        self.0.disk(path)
    }
}

/// Returns the error that `serve` returned, or fails the test when it keeps
/// serving.
fn ended(ends: &Receiver<io::Error>) -> io::Error {
    ends.recv_timeout(REPLY_WITHIN)
        .expect("the daemon stops serving")
}

#[test]
fn a_daemon_stops_serving_when_its_socket_file_is_removed() {
    let (path, ends) = serving(Dial::new(Pressure::Normal));
    let _ = jobs(&path);
    std::fs::remove_file(&*path).expect("the socket file is removed");
    assert!(ended(&ends).to_string().contains("socket"));
}

#[test]
fn a_daemon_that_no_client_reaches_stops_serving_when_its_socket_file_is_removed() {
    let (tells, sampled) = mpsc::channel();
    let (path, ends) = serving(Tells(Dial::new(Pressure::Normal), tells));
    // The first sample comes from the serve call itself and the second from
    // the sampler thread, which holds the daemon's record of the file.
    for _ in 0..2 {
        sampled
            .recv_timeout(REPLY_WITHIN)
            .expect("the probe is sampled");
    }
    std::fs::remove_file(&*path).expect("the socket file is removed");
    assert!(ended(&ends).to_string().contains("socket"));
}

#[test]
fn a_daemon_whose_socket_file_is_untouched_keeps_serving_when_its_path_has_a_symlinked_directory() {
    let real = std::env::temp_dir().join(format!("db-real-{}", std::process::id()));
    let link = std::env::temp_dir().join(format!("db-link-{}", std::process::id()));
    std::fs::create_dir_all(&real).expect("the directory is made");
    std::os::unix::fs::symlink(&real, &link).expect("the link is made");
    let path = link.join("s.sock");
    let ends = serving_at(&path, Dial::new(Pressure::Normal));
    let _ = jobs(&path);
    assert!(
        ends.recv_timeout(Duration::from_millis(1500)).is_err(),
        "the daemon passed several checks"
    );
    let _ = jobs(&path);
    let _ = std::fs::remove_dir_all(&real);
    let _ = std::fs::remove_file(&link);
}

#[test]
fn a_daemon_whose_sampler_is_stalled_stops_serving_when_its_socket_file_is_removed() {
    let dial = Dial::new(Pressure::Normal);
    let (path, ends) = serving(dial.clone());
    let _ = jobs(&path);
    let _stalled = dial.stall();
    std::fs::remove_file(&*path).expect("the socket file is removed");
    assert!(ended(&ends).to_string().contains("socket"));
}

#[test]
fn a_daemon_stops_serving_when_another_socket_replaces_its_file() {
    let (path, ends) = serving(Dial::new(Pressure::Normal));
    let _ = jobs(&path);
    std::fs::remove_file(&*path).expect("the socket file is removed");
    let _other = UnixListener::bind(&*path).expect("another socket takes the path");
    assert!(ended(&ends).to_string().contains("socket"));
}

#[cfg(target_os = "macos")]
#[test]
fn a_daemon_stops_serving_when_its_lock_file_is_removed() {
    let (path, ends) = serving(Dial::new(Pressure::Normal));
    let _ = jobs(&path);
    std::fs::remove_file(crate::daemon::lock_path(&path)).expect("the lock file is removed");
    assert!(ended(&ends).to_string().contains("lock file"));
}

#[cfg(target_os = "macos")]
#[test]
fn a_daemon_stops_serving_when_another_file_replaces_its_lock_file() {
    let (path, ends) = serving(Dial::new(Pressure::Normal));
    let _ = jobs(&path);
    let lock = crate::daemon::lock_path(&path);
    std::fs::remove_file(&lock).expect("the lock file is removed");
    let _other = UnixListener::bind(&lock).expect("another file takes the path");
    assert!(ended(&ends).to_string().contains("lock file"));
}

#[cfg(target_os = "macos")]
#[test]
fn a_daemon_whose_lock_file_is_replaced_before_it_serves_stops_serving() {
    let path = socket_path();
    let bound = bind(&path).expect("the test socket binds");
    let lock = crate::daemon::lock_path(&path);
    std::fs::remove_file(&lock).expect("the lock file is removed");
    let _other = UnixListener::bind(&lock).expect("another file takes the path");
    let settings = Settings {
        sample_every: SAMPLE_EVERY,
        ..Settings::new(
            NonZeroUsize::new(2).expect("two is positive"),
            NonZeroUsize::new(CORES),
            None,
            None,
        )
    };
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let (ended_tx, ends) = mpsc::channel();
    thread::spawn(move || {
        let _ = ended_tx.send(serve(&bound, Dial::new(Pressure::Normal), settings, &log));
    });
    assert!(ended(&ends).to_string().contains("lock file"));
}

#[cfg(target_os = "macos")]
#[test]
fn a_daemon_whose_lock_file_is_untouched_keeps_serving() {
    let (path, ends) = serving(Dial::new(Pressure::Normal));
    let _ = jobs(&path);
    assert!(
        ends.recv_timeout(Duration::from_millis(1500)).is_err(),
        "the daemon passed several checks"
    );
    let _ = jobs(&path);
}

#[cfg(target_os = "macos")]
#[test]
fn a_daemon_that_cannot_start_a_connections_thread_stops_serving_and_closes_it() {
    use std::io::Read;

    let path = socket_path();
    let ends = serving_refusing(
        &path,
        Dial::new(Pressure::Normal),
        Some("deadband-connection"),
    );
    let mut stream = std::os::unix::net::UnixStream::connect(&*path).expect("the client connects");
    stream
        .set_read_timeout(Some(REPLY_WITHIN))
        .expect("the timeout is set");
    let error = ended(&ends).to_string();
    assert!(error.contains("thread"), "{error}");
    let mut rest = Vec::new();
    assert_eq!(
        stream.read_to_end(&mut rest).expect("the daemon closes it"),
        0
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_daemon_that_cannot_start_its_sampler_stops_serving() {
    let path = socket_path();
    let ends = serving_refusing(&path, Dial::new(Pressure::Normal), Some("deadband-sampler"));
    let error = ended(&ends).to_string();
    assert!(error.contains("sampler"), "{error}");
}

#[cfg(target_os = "macos")]
#[test]
fn a_daemon_that_cannot_start_its_watchdog_stops_serving() {
    let path = socket_path();
    let ends = serving_refusing(&path, Dial::new(Pressure::Normal), Some("deadband-age"));
    let error = ended(&ends).to_string();
    assert!(error.contains("watchdog"), "{error}");
}

#[test]
fn a_daemon_stops_serving_when_its_sampler_panics() {
    let (_path, ends) = serving(PanicsAfterOne(Arc::new(AtomicUsize::new(0))));
    assert!(ended(&ends).to_string().contains("sampler"));
}

#[test]
fn a_job_starts_when_the_sampler_stalls_and_its_last_reading_holds_the_job() {
    let dial = Dial::new(Pressure::Normal);
    dial.fill_disk(true);
    let (path, _) = start(2, &dial);
    let mut first = Client::ask(&path, "a");
    assert!(
        matches!(
            first.reply(),
            crate::protocol::Reply::Queued {
                held: Some(Hold::Disk { .. }),
                ..
            }
        ),
        "the low disk holds the job while the sampler reads"
    );
    let _stalled = dial.stall();
    assert_eq!(first.reply(), granted(2));
}

/// A log that sends each line to the test, with whether the queue lock of
/// `queue` was held when the line was written.
struct Lines {
    /// Where each line goes.
    sent: Sender<(String, bool)>,
    /// The queue whose lock the log checks, or `None` to check none.
    queue: Option<Shared>,
    /// The part of the line written so far.
    partial: Vec<u8>,
}

impl Lines {
    /// Returns a log that checks the lock of `queue`, and the end of the
    /// channel that its lines arrive on.
    fn log(queue: Option<Shared>) -> (Log, Receiver<(String, bool)>) {
        let (sent, lines) = mpsc::channel();
        let log: Log = Arc::new(Mutex::new(Self {
            sent,
            queue,
            partial: Vec::new(),
        }));
        (log, lines)
    }
}

impl Write for Lines {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.partial.extend_from_slice(buf);
        while let Some(end) = self.partial.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            let held = self
                .queue
                .as_ref()
                .is_some_and(|queue| queue.try_lock().is_err());
            let _ = self
                .sent
                .send((String::from_utf8_lossy(&line).into_owned(), held));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Waits for the first line of `lines` that contains `text`, and returns
/// whether the queue lock was held when it was written.
fn line_with(lines: &Receiver<(String, bool)>, text: &str) -> bool {
    let until = Instant::now() + REPLY_WITHIN;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        let (line, held) = lines
            .recv_timeout(left)
            .unwrap_or_else(|_| panic!("the log says {text:?}"));
        if line.contains(text) {
            return held;
        }
    }
}

/// Whether a [`Gated`] log holds its writes, and how many writes wait.
#[derive(Default)]
struct GateState {
    closed: bool,
    waiting: usize,
}

/// A gate that holds every write to a [`Gated`] log while it is closed, as
/// a stopped terminal or an unread pipe does.
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<GateState>, Condvar)>);

impl Gate {
    fn set(&self, closed: bool) {
        let (state, changed) = &*self.0;
        state.lock().unwrap_or_else(PoisonError::into_inner).closed = closed;
        changed.notify_all();
    }

    /// Reports whether a write waits at the closed gate within `within`.
    fn holds_a_write(&self, within: Duration) -> bool {
        let (state, changed) = &*self.0;
        let state = state.lock().unwrap_or_else(PoisonError::into_inner);
        let (state, _) = changed
            .wait_timeout_while(state, within, |state| state.waiting == 0)
            .unwrap_or_else(PoisonError::into_inner);
        state.waiting > 0
    }
}

/// A log whose writes wait while its [`Gate`] is closed.
struct Gated(Gate);

impl Write for Gated {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let (state, changed) = &*self.0.0;
        let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
        state.waiting += 1;
        changed.notify_all();
        while state.closed {
            state = changed.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
        state.waiting -= 1;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn status_and_a_grant_complete_while_the_stale_reading_line_cannot_be_written() {
    let dial = Dial::new(Pressure::Normal);
    let gate = Gate::default();
    let path = socket_path();
    let log: Log = Arc::new(Mutex::new(Gated(gate.clone())));
    let _ends = serving_logged(&path, dial.clone(), None, log, None);
    let _ = jobs(&path);
    gate.set(true);
    let stalled = dial.stall();
    assert!(
        gate.holds_a_write(REPLY_WITHIN),
        "the watchdog writes that the reading is stale"
    );
    assert!(jobs(&path).waiting.is_empty());
    assert_eq!(Client::ask(&path, "a").reply(), granted(2));
    gate.set(false);
    drop(stalled);
}

#[test]
fn the_line_that_the_history_thread_stopped_is_written_with_the_queue_lock_released() {
    let shared = shared(1);
    let (log, lines) = Lines::log(Some(Arc::clone(&shared)));
    let (recorder, writer_end) = Recorder::channel(1, Arc::clone(&log));
    drop(writer_end);
    {
        let mut state = lock(&shared);
        state.settings.sample_every = SAMPLE_EVERY;
        state.settings.machine_every = Duration::ZERO;
        state.recorder = Some(recorder);
        state.write_machine(Instant::now());
    }
    let path = socket_path();
    let listener = UnixListener::bind(&*path).expect("the test socket binds");
    let (reason, _ends) = mpsc::channel();
    let ending = Ending::new(&listener, None, reason);
    let (_stop, stopped) = mpsc::channel::<()>();
    let watchdog = Arc::clone(&shared);
    thread::spawn(move || age(&watchdog, &stopped, &log, ending));
    assert!(
        !line_with(&lines, "job history has stopped"),
        "the line was written with the queue lock held"
    );
}

/// Opens the log file at `path`, for appending when `append` and for
/// writing in place otherwise.
fn open_log(path: &Path, append: bool) -> File {
    #[expect(
        clippy::disallowed_methods,
        reason = "the test makes the log file that launchd would open for appending"
    )]
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(!append)
        .append(append)
        .open(path)
        .expect("the log file opens");
    file
}

/// Returns a file in `scratch` opened for appending, as launchd opens the
/// daemon's log, that is `len` bytes long, and a second handle on it that
/// writes in place.
fn appended(scratch: &Scratch, len: u64) -> (File, File) {
    let path = scratch.path().join("deadband.log");
    let file = open_log(&path, true);
    file.set_len(len).expect("the log file grows");
    (file, open_log(&path, false))
}

#[test]
fn a_standard_error_file_over_the_limit_is_emptied_at_start_and_at_the_alive_check() {
    let scratch = Scratch::new();
    let (file, other) = appended(&scratch, LOG_LIMIT + 1);
    let (log, lines) = Lines::log(None);
    let path = socket_path();
    let _ends = serving_logged(&path, Dial::new(Pressure::Normal), None, log, Some(file));
    let _ = line_with(&lines, "emptied");
    assert_eq!(other.metadata().expect("the file reads").len(), 0);
    other.set_len(LOG_LIMIT + 1).expect("the log file grows");
    let _ = line_with(&lines, "emptied");
    assert_eq!(other.metadata().expect("the file reads").len(), 0);
}

#[test]
fn a_standard_error_file_at_the_limit_is_left_alone() {
    let scratch = Scratch::new();
    let (file, other) = appended(&scratch, LOG_LIMIT);
    let (log, lines) = Lines::log(None);
    let path = socket_path();
    let _ends = serving_logged(&path, Dial::new(Pressure::Normal), None, log, Some(file));
    let _ = jobs(&path);
    // Two alive checks pass in this time, at 60 samples of 10 ms each.
    let quiet = Instant::now() + Duration::from_millis(1500);
    while let Ok((line, _)) = lines.recv_timeout(quiet.saturating_duration_since(Instant::now())) {
        assert!(!line.contains("emptied"), "{line}");
    }
    assert_eq!(other.metadata().expect("the file reads").len(), LOG_LIMIT);
}

#[test]
fn a_pipe_as_standard_error_is_never_emptied() {
    let (mut reader, mut writer) = io::pipe().expect("the pipe opens");
    writer.write_all(b"kept\n").expect("the pipe takes a line");
    let (log, lines) = Lines::log(None);
    trim(&File::from(std::os::fd::OwnedFd::from(writer)), 0, &log);
    let mut kept = [0_u8; 5];
    reader
        .read_exact(&mut kept)
        .expect("the line is still there");
    assert_eq!(&kept, b"kept\n");
    assert!(lines.try_recv().is_err(), "the daemon logs nothing");
}

#[test]
fn a_regular_file_as_standard_error_is_emptied_past_its_limit() {
    let scratch = Scratch::new();
    let (file, other) = appended(&scratch, 1);
    let (log, lines) = Lines::log(None);
    trim(&file, 0, &log);
    assert_eq!(other.metadata().expect("the file reads").len(), 0);
    let _ = line_with(&lines, "emptied");
}

#[test]
fn a_file_not_opened_for_appending_is_never_emptied() {
    let scratch = Scratch::new();
    let (_appending, other) = appended(&scratch, 1);
    let (log, lines) = Lines::log(None);
    trim(&other, 0, &log);
    assert_eq!(other.metadata().expect("the file reads").len(), 1);
    assert!(lines.try_recv().is_err(), "the daemon logs nothing");
}
