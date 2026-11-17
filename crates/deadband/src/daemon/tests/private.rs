//! Tests of the directory that deadband owns for its fallback socket and of
//! the lock file that a daemon, the history writer and the services open.

use std::fs::{self, DirBuilder, Permissions};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

use rustix::process::getuid;

use crate::daemon::{
    DirectoryFault, Unbound, private_directory, private_directory_as, take_lock, take_lock_as,
};
use crate::history::tests::{Scratch, write_file};

/// Returns the uid of the user who runs the tests.
fn own() -> u32 {
    getuid().as_raw()
}

/// Returns the mode bits of `path`, without the file type.
fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .expect("the path exists")
        .permissions()
        .mode()
        & 0o7777
}

/// Makes `path` as a directory with exactly `mode`.
fn directory_with(path: &Path, mode: u32) {
    DirBuilder::new()
        .create(path)
        .expect("the directory is made");
    fs::set_permissions(path, Permissions::from_mode(mode)).expect("the mode is set");
}

#[test]
fn a_missing_fallback_directory_is_made_with_mode_0700() {
    let scratch = Scratch::new();
    let directory = scratch.path().join("deadband");
    private_directory(&directory).expect("the directory is made");
    assert_eq!(mode(&directory), 0o700);
}

#[test]
fn an_existing_directory_that_only_its_owner_can_enter_is_reused() {
    let scratch = Scratch::new();
    let directory = scratch.path().join("deadband");
    directory_with(&directory, 0o700);
    write_file(directory.join("kept"), "x").expect("the file writes");
    private_directory(&directory).expect("the directory is reused");
    assert!(directory.join("kept").exists());
}

#[test]
fn an_existing_directory_that_others_can_write_is_refused_and_left_as_it_is() {
    let scratch = Scratch::new();
    let directory = scratch.path().join("deadband");
    directory_with(&directory, 0o777);
    let refused = private_directory(&directory).expect_err("a wide mode is refused");
    assert!(
        matches!(
            refused,
            Unbound::Directory {
                fault: DirectoryFault::Mode { mode: 0o777 },
                ..
            }
        ),
        "{refused:?}"
    );
    assert_eq!(mode(&directory), 0o777);
}

#[test]
fn an_existing_directory_that_others_can_only_read_is_refused() {
    let scratch = Scratch::new();
    let directory = scratch.path().join("deadband");
    directory_with(&directory, 0o755);
    assert!(matches!(
        private_directory(&directory),
        Err(Unbound::Directory {
            fault: DirectoryFault::Mode { .. },
            ..
        })
    ));
}

#[test]
fn an_existing_directory_of_another_user_is_refused() {
    let scratch = Scratch::new();
    let directory = scratch.path().join("deadband");
    directory_with(&directory, 0o700);
    let refused = private_directory_as(&directory, own() + 1).expect_err("another owner");
    assert!(
        matches!(
            refused,
            Unbound::Directory { fault: DirectoryFault::Owner { owner }, .. } if owner == own()
        ),
        "{refused:?}"
    );
    private_directory_as(&directory, own()).expect("the twin: the owner is admitted");
}

#[test]
fn a_symlink_in_place_of_the_directory_is_refused() {
    let scratch = Scratch::new();
    let target = scratch.path().join("elsewhere");
    directory_with(&target, 0o700);
    let link = scratch.path().join("deadband");
    symlink(&target, &link).expect("the link is made");
    assert!(matches!(
        private_directory(&link),
        Err(Unbound::Directory {
            fault: DirectoryFault::NotDirectory,
            ..
        })
    ));
}

#[test]
fn the_refusal_names_the_directory_and_says_the_repair() {
    use crumbkit::{Failure, FieldValue, Recovery};

    let scratch = Scratch::new();
    let directory = scratch.path().join("deadband");
    directory_with(&directory, 0o777);
    let refused = private_directory(&directory).expect_err("a wide mode is refused");
    let fields = refused.fields();
    assert_eq!(fields.len(), 1, "{fields:?}");
    assert_eq!(fields[0].name(), "path");
    let shown = directory.to_str().expect("the path is UTF-8");
    assert_eq!(fields[0].value(), &FieldValue::from(shown));
    assert!(
        matches!(
            refused.recovery(),
            Recovery::Hint(ref text) if text.contains("DEADBAND_SOCKET_DIR")
                && text.contains("XDG_RUNTIME_DIR") && text.contains("root")
        ),
        "{:?}",
        refused.recovery()
    );
    assert!(refused.to_string().contains("777"), "{refused}");
}

#[test]
fn a_lock_file_that_is_a_symlink_is_refused_and_its_target_is_not_made() {
    let scratch = Scratch::new();
    let target = scratch.path().join("victim");
    let link = scratch.path().join("deadband.sock.lock");
    symlink(&target, &link).expect("the link is made");
    assert!(take_lock(&link).is_err());
    assert!(!target.exists(), "the lock made the link's target");
}

#[test]
fn a_regular_lock_file_is_locked_with_mode_0600() {
    let scratch = Scratch::new();
    let path = scratch.path().join("deadband.sock.lock");
    let lock = take_lock(&path).expect("the lock is free");
    assert_eq!(
        fs::metadata(&path).expect("the lock exists").mode() & 0o777,
        0o600
    );
    drop(lock);
}

#[test]
fn a_lock_file_of_another_user_is_refused() {
    let scratch = Scratch::new();
    let path = scratch.path().join("deadband.sock.lock");
    drop(take_lock(&path).expect("the lock is made"));
    let refused = take_lock_as(&path, own() + 1).expect_err("another owner is refused");
    assert!(
        matches!(
            refused,
            std::fs::TryLockError::Error(ref error)
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ),
        "{refused:?}"
    );
    drop(take_lock_as(&path, own()).expect("the twin: the owner locks it"));
}
