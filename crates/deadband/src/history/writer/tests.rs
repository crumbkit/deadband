//! Tests of the rotation check that the writer makes before it renames.

use super::super::tests::{Scratch, lines_of, machine, write_file, writer};
use super::super::{FILE, ROTATED_FILE};

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
