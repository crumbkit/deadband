//! Tests of the terminal guard, the refusals and one poll against a fake
//! socket.

use std::cell::RefCell;
use std::io;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use crumbkit::Failure;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use super::guard::{Guard, Modes};
use super::model::{Model, Phase};
use super::{TOP_NOT_A_TERMINAL, opens, quits, tick};
use crate::probe::{Disk, Pressure, Probe, Sample, Unreadable};
use crate::protocol::{Ago, Reply, Timeline, write_line};
use crate::testing::{TestSocket, socket_path};

#[derive(Clone, Default)]
struct Fake {
    log: Rc<RefCell<Vec<&'static str>>>,
    refuse: bool,
}

impl Modes for Fake {
    type Screen = ();

    fn enter(&mut self) -> io::Result<()> {
        self.log.borrow_mut().push("enter");
        if self.refuse {
            Err(io::ErrorKind::Other.into())
        } else {
            Ok(())
        }
    }

    fn leave(&mut self) {
        self.log.borrow_mut().push("leave");
    }
}

#[test]
fn the_guard_restores_the_terminal_on_a_normal_exit() {
    let fake = Fake::default();
    let log = fake.log.clone();
    let (guard, ()) = Guard::enter(fake).expect("the modes enter");
    assert_eq!(*log.borrow(), vec!["enter"]);
    drop(guard);
    assert_eq!(*log.borrow(), vec!["enter", "leave"]);
}

#[test]
fn the_guard_leaves_a_panic_to_the_hook_that_already_restored_the_terminal() {
    let fake = Fake::default();
    let log = fake.log.clone();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _entered = Guard::enter(fake).expect("the modes enter");
        panic!("the view panics");
    }));
    assert!(unwound.is_err());
    assert_eq!(*log.borrow(), vec!["enter"]);
}

#[test]
fn a_guard_that_fails_half_way_in_restores_what_it_entered() {
    let fake = Fake {
        refuse: true,
        ..Fake::default()
    };
    let log = fake.log.clone();
    assert!(Guard::enter(fake).is_err());
    assert_eq!(*log.borrow(), vec!["enter", "leave"]);
}

#[test]
fn the_view_opens_on_a_terminal_without_json() {
    assert!(opens(true, true, false).is_ok());
}

#[test]
fn the_view_refuses_a_pipe_on_either_stream_or_json() {
    for (stdin, stdout, json) in [
        (false, true, false),
        (true, false, false),
        (true, true, true),
    ] {
        let refused = opens(stdin, stdout, json).expect_err("the view refuses");
        assert_eq!(refused.code(), &TOP_NOT_A_TERMINAL);
    }
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

#[test]
fn q_quits_with_zero() {
    assert_eq!(quits(&key(KeyCode::Char('q'), KeyModifiers::NONE)), Some(0));
}

#[test]
fn control_c_quits_with_130() {
    assert_eq!(
        quits(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Some(130)
    );
}

#[test]
fn other_keys_and_resizes_do_not_quit() {
    assert_eq!(quits(&key(KeyCode::Char('c'), KeyModifiers::NONE)), None);
    assert_eq!(quits(&key(KeyCode::Char('x'), KeyModifiers::NONE)), None);
    assert_eq!(quits(&Event::Resize(80, 16)), None);
}

/// A probe that returns one load.
struct Fixed;

impl Probe for Fixed {
    fn sample(&self) -> Result<Sample, Unreadable> {
        Ok(Sample {
            load_1m: 3.0,
            memory_pressure: Pressure::Normal,
            swap_used_bytes: 0,
            swap_total_bytes: 0,
        })
    }

    fn disk(&self, _path: &Path) -> io::Result<Disk> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// A probe that cannot read the machine.
struct Broken;

impl Probe for Broken {
    fn sample(&self) -> Result<Sample, Unreadable> {
        Err(Unreadable::Load)
    }

    fn disk(&self, _path: &Path) -> io::Result<Disk> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// How long a tick waits for a daemon that the test expects to answer.
const ANSWER: Duration = Duration::from_secs(10);

/// Starts a fake daemon that answers each connection with `line` or, when
/// `line` is empty, closes it at once.
fn fake(lines: Vec<Vec<u8>>) -> TestSocket {
    let path = socket_path();
    let listener = UnixListener::bind(&path).expect("the test socket binds");
    thread::spawn(move || {
        for line in lines {
            let (mut stream, _) = listener.accept().expect("the client connects");
            if !line.is_empty() {
                std::io::Write::write_all(&mut stream, &line).expect("the line writes");
            }
            let mut bytes = [0_u8; 32];
            let _ = std::io::Read::read(&mut stream, &mut bytes);
        }
    });
    path
}

fn reply_line(up: u64) -> Vec<u8> {
    let mut line = Vec::new();
    let reply = Reply::Timeline(Timeline {
        up: Ago::from_millis(up),
        jobs: Vec::new(),
        hidden: 0,
        held: None,
    });
    write_line(&mut line, &reply).expect("the line writes");
    line
}

fn phase(model: &Model) -> Phase {
    model.phase(Instant::now(), 80, 16)
}

fn model() -> Model {
    Model::new(std::num::NonZeroUsize::new(4))
}

#[test]
fn a_tick_against_a_daemon_that_sends_garbage_keeps_the_last_model() {
    let path = fake(vec![reply_line(5), b"not json\n".to_vec()]);
    let mut model = model();
    tick(&mut model, &path, &Fixed, ANSWER);
    assert_eq!(phase(&model), Phase::Live);
    tick(&mut model, &path, &Fixed, ANSWER);
    assert!(
        matches!(phase(&model), Phase::Stale { .. }),
        "{:?}",
        phase(&model)
    );
}

#[test]
fn a_failed_probe_leaves_a_gap_and_the_poll_still_counts() {
    let path = fake(vec![reply_line(5)]);
    let mut model = model();
    tick(&mut model, &path, &Broken, ANSWER);
    assert_eq!(phase(&model), Phase::Live);
    assert_eq!(model.latest_load(), None);
}
