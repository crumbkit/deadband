//! The daemon ends when its socket file is gone or replaced or when its
//! sampler panics, and it admits on a reading that has gone stale.

use std::io;
use std::num::NonZeroUsize;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::super::{Log, Settings, bind, serve};
use super::{CORES, Client, Dial, REPLY_WITHIN, SAMPLE_EVERY, granted, jobs, start};
use crate::admit::Hold;
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
    let bound = bind(path).expect("the test socket binds");
    let log: Log = Arc::new(Mutex::new(Vec::new()));
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
        let _ = ended.send(serve(&bound.listener, probe, settings, &log));
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
