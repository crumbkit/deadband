//! Tests of `deadband service` and of the daemon's state that `deadband
//! status` names, run against a fake launchd and a temporary home.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{Fixed, Test};
use crate::cli::{EXIT_SERVICE, LaunchAgent};
use crate::testing::{FakeLaunchd, TestHome};

/// Returns a test machine whose launch agent lives in `home`.
fn with_agent(home: &Path, launchd: &Arc<FakeLaunchd>) -> Test<Fixed> {
    let mut test = Test::new(Fixed);
    test.machine.service = Some(LaunchAgent {
        home: Some(home.to_path_buf()),
        binary: Some(PathBuf::from("/Users/me/.local/bin/deadband")),
        uid: 501,
        launchd: Arc::clone(launchd) as _,
    });
    test
}

fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents/io.github.crumbkit.deadband.plist")
}

fn write_plist(home: &Path) {
    std::fs::create_dir_all(home.join("Library/LaunchAgents")).expect("writable");
    #[expect(clippy::disallowed_methods, reason = "the test writes a fixture")]
    std::fs::write(plist_path(home), "<plist version=\"1.0\"></plist>\n").expect("writable");
}

#[test]
fn status_names_service_install_when_the_plist_is_missing() {
    let home = TestHome::new();
    let test = with_agent(&home, &Arc::new(FakeLaunchd::new(false)));

    let (code, out, err) = test.run(&["status"]);

    assert_eq!(code, 0, "{err}");
    assert!(
        out.ends_with("daemon: not installed; run deadband service install\n"),
        "{out}"
    );
}

#[test]
fn status_names_service_install_when_launchd_has_not_loaded_the_plist() {
    let home = TestHome::new();
    write_plist(&home);
    let test = with_agent(&home, &Arc::new(FakeLaunchd::new(false)));

    let (code, out, err) = test.run(&["status"]);

    assert_eq!(code, 0, "{err}");
    assert!(
        out.ends_with("daemon: installed but not loaded; run deadband service install\n"),
        "{out}"
    );
}

#[test]
fn status_says_not_running_when_launchd_has_loaded_the_agent_and_no_daemon_answers() {
    let home = TestHome::new();
    write_plist(&home);
    let test = with_agent(&home, &Arc::new(FakeLaunchd::new(true)));

    let (_, out, _) = test.run(&["status"]);

    assert!(out.ends_with("daemon: not running\n"), "{out}");
}

#[test]
fn status_with_json_gives_the_daemon_state_of_each_kind() {
    let home = TestHome::new();
    let launchd = Arc::new(FakeLaunchd::new(false));
    let test = with_agent(&home, &launchd);
    let (_, out, _) = test.run(&["status", "--json"]);
    assert!(out.contains(r#""daemon_state":"not_installed""#), "{out}");

    write_plist(&home);
    let (_, out, _) = test.run(&["status", "--json"]);
    assert!(out.contains(r#""daemon_state":"not_loaded""#), "{out}");
    assert!(out.contains(r#""daemon":null"#), "{out}");

    test.start_daemon(1);
    let (_, out, _) = test.run(&["status", "--json"]);
    assert!(out.contains(r#""daemon_state":"running""#), "{out}");
}

#[test]
fn service_install_without_launchd_exits_with_service_unsupported() {
    let (code, _, err) = Test::new(Fixed).run(&["service", "install"]);

    assert_eq!(code, EXIT_SERVICE, "{err}");
    assert!(err.contains("deadband::service::unsupported"), "{err}");
}

#[test]
fn service_install_writes_the_plist_and_prints_one_line_per_step() {
    let home = TestHome::new();
    let launchd = Arc::new(FakeLaunchd::new(false));

    let (code, out, err) = with_agent(&home, &launchd).run(&["service", "install"]);

    assert_eq!(code, 0, "{err}");
    assert!(plist_path(&home).exists());
    assert_eq!(
        out,
        "wrote ~/Library/LaunchAgents/io.github.crumbkit.deadband.plist\nloaded io.github.crumbkit.deadband\n"
    );
}

#[test]
fn service_uninstall_without_yes_and_without_a_terminal_refuses_and_keeps_the_plist() {
    let home = TestHome::new();
    write_plist(&home);
    let launchd = Arc::new(FakeLaunchd::new(true));

    let (code, _, err) = with_agent(&home, &launchd).run(&["service", "uninstall"]);

    assert_eq!(code, 1, "{err}");
    assert!(err.contains("deadband::confirmation::needs_yes"), "{err}");
    assert!(plist_path(&home).exists());
    assert!(launchd.changes().is_empty(), "{:?}", launchd.changes());
}

#[test]
fn service_uninstall_with_no_input_and_without_yes_refuses() {
    let home = TestHome::new();
    write_plist(&home);
    let launchd = Arc::new(FakeLaunchd::new(true));

    let (code, _, err) = with_agent(&home, &launchd).run(&["service", "uninstall", "--no-input"]);

    assert_eq!(code, 1, "{err}");
    assert!(err.contains("deadband::confirmation::needs_yes"), "{err}");
    assert!(plist_path(&home).exists());
}

#[test]
fn service_uninstall_with_yes_unloads_the_agent_and_deletes_the_plist() {
    let home = TestHome::new();
    write_plist(&home);
    let launchd = Arc::new(FakeLaunchd::new(true));

    let (code, out, err) =
        with_agent(&home, &launchd).run(&["service", "uninstall", "--yes", "--no-input"]);

    assert_eq!(code, 0, "{err}");
    assert!(!plist_path(&home).exists());
    assert_eq!(
        out,
        "unloaded io.github.crumbkit.deadband\ndeleted ~/Library/LaunchAgents/io.github.crumbkit.deadband.plist\n"
    );
}
