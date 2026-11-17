//! Tests of the rotation check that the writer makes before it renames, and
//! of the line that says the writer has stopped.

use std::sync::{Arc, Mutex};

use super::super::tests::{Scratch, lines_of, machine, write_file, writer};
use super::super::{FILE, ROTATED_FILE};
use super::Recorder;
use crate::daemon::Log;

#[test]
fn a_rotation_skips_the_rename_when_another_file_replaced_the_history_file() {
    let scratch = Scratch::new();
    let (mut writer, _) = writer(scratch.path(), 1024 * 1024);
    writer.append(&machine(1));
    let path = scratch.path().join(FILE);
    #[expect(
        clippy::disallowed_methods,
        reason = "the test moves a fixture aside, and a failed move only fails that test"
    )]
    std::fs::rename(&path, scratch.path().join("moved-away")).expect("the file moves");
    write_file(&path, "{\"other\":1}\n").expect("the replacement writes");
    writer.limit = 0;

    writer.rotate_past_limit();

    assert!(!scratch.path().join(ROTATED_FILE).exists());
    assert_eq!(lines_of(&path), [serde_json::json!({"other": 1})]);
    writer.limit = 1024 * 1024;
    writer.append(&machine(2));
    let lines = lines_of(&path);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["at_ms"], 2);
}

#[test]
fn a_rotation_renames_the_file_the_writer_has_open() {
    let scratch = Scratch::new();
    let (mut writer, _) = writer(scratch.path(), 1024 * 1024);
    writer.append(&machine(1));
    writer.limit = 0;

    writer.rotate_past_limit();

    assert_eq!(lines_of(&scratch.path().join(ROTATED_FILE))[0]["at_ms"], 1);
    assert!(!scratch.path().join(FILE).exists());
}

/// Returns a recorder whose writer has stopped, or is running when
/// `running`, with the log it writes to.
fn recorder(running: bool) -> (Recorder, Arc<Mutex<Vec<u8>>>, impl Sized) {
    let written = Arc::new(Mutex::new(Vec::new()));
    let log: Log = written.clone();
    let (recorder, lines) = Recorder::channel(4, log);
    let kept = running.then_some(lines);
    (recorder, written, kept)
}

/// Returns the lines that `written` holds.
fn logged(written: &Mutex<Vec<u8>>) -> String {
    String::from_utf8(written.lock().expect("the log locks").clone()).expect("the log is text")
}

#[test]
fn a_line_that_finds_the_writer_stopped_is_logged_only_when_the_recorder_is_asked_to_say_so() {
    let (recorder, written, _kept) = recorder(false);
    recorder.send(machine(1));
    recorder.send(machine(2));
    assert_eq!(logged(&written), "");
    recorder.say_stopped();
    recorder.say_stopped();
    assert_eq!(logged(&written).matches("has stopped").count(), 1);
}

#[test]
fn a_running_writer_is_not_said_to_have_stopped() {
    let (recorder, written, _kept) = recorder(true);
    recorder.send(machine(1));
    recorder.say_stopped();
    assert_eq!(logged(&written), "");
}
