use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{Reason, check};

/// A directory of the test's own, removed when the test ends.
struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("db-program-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("the directory is made");
        Self(path)
    }

    fn file(&self, name: &str, mode: u32) -> String {
        let path = self.0.join(name);
        #[expect(
            clippy::disallowed_methods,
            reason = "the test writes the program file a person would install"
        )]
        std::fs::write(&path, "#!/bin/sh\n").expect("the file writes");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
            .expect("the mode is set");
        path.display().to_string()
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn command(program: &str) -> Vec<String> {
    vec![program.to_owned(), "reclaim".to_owned()]
}

#[test]
fn an_executable_file_is_a_usable_program() {
    let directory = Directory::new();
    let program = directory.file("mxv", 0o755);
    assert_eq!(check(&command(&program)), Ok(()));
}

#[test]
fn a_path_that_does_not_exist_is_reported_as_missing_with_its_path() {
    let directory = Directory::new();
    let program = directory.0.join("missing/mxv").display().to_string();
    let unusable = check(&command(&program)).expect_err("the program is missing");
    assert_eq!(unusable.reason, Reason::Missing);
    assert_eq!(unusable.path, PathBuf::from(&program));
}

#[test]
fn a_directory_is_reported_as_not_a_file() {
    let directory = Directory::new();
    let program = directory.0.display().to_string();
    let unusable = check(&command(&program)).expect_err("a directory is no program");
    assert_eq!(unusable.reason, Reason::NotAFile);
}

#[test]
fn a_file_without_an_execute_bit_is_reported_as_not_executable() {
    let directory = Directory::new();
    let program = directory.file("mxv", 0o644);
    let unusable = check(&command(&program)).expect_err("the file cannot run");
    assert_eq!(unusable.reason, Reason::NotExecutable);
}

#[test]
fn a_bare_program_name_is_reported_as_relative() {
    let unusable = check(&command("mxv")).expect_err("a bare name has no path");
    assert_eq!(unusable.reason, Reason::Relative);
    assert_eq!(unusable.path, PathBuf::from("mxv"));
}

#[test]
fn an_empty_command_names_no_program_to_check() {
    assert_eq!(check(&[]), Ok(()));
}

#[test]
fn each_reason_reads_as_a_clause_that_ends_a_sentence() {
    assert_eq!(Reason::Missing.to_string(), "which does not exist");
    assert_eq!(Reason::NotAFile.to_string(), "which is not a file");
    assert_eq!(Reason::NotExecutable.to_string(), "which is not executable");
    assert!(Reason::Relative.to_string().contains("PATH"));
}

#[test]
fn only_a_relative_program_may_still_start() {
    assert!(Reason::Missing.always_fails());
    assert!(Reason::NotAFile.always_fails());
    assert!(Reason::NotExecutable.always_fails());
    assert!(Reason::Unreadable("denied".to_owned()).always_fails());
    assert!(!Reason::Relative.always_fails());
}

#[test]
fn a_path_inside_a_directory_the_user_cannot_enter_is_reported_as_unreadable_with_the_error() {
    let directory = Directory::new();
    let locked = directory.0.join("locked");
    std::fs::create_dir(&locked).expect("the directory is made");
    let inside = PathBuf::from(directory.file("locked/mxv", 0o755));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
        .expect("the mode is set");
    let unusable = check(&command(&inside.display().to_string())).expect_err("the path is locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
        .expect("the mode is restored");
    let Reason::Unreadable(error) = unusable.reason else {
        panic!("{:?}", unusable.reason);
    };
    assert!(error.contains("Permission denied"), "{error}");
}
