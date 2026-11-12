//! Tests of `deadband service install` and `uninstall`, run against a fake
//! launchd and a temporary home.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::plist::{binary_path, render};
use super::{DaemonState, Install, LaunchAgent, ServiceFailed, install, state, uninstall};
use crate::testing::{FakeLaunchd, TestHome};

/// The temporary directory that the fake launchd's getconf prints.
const TEMP: &str = "/var/folders/xy/abc123/T/";

/// Returns an agent for `home` whose binary is `/Users/me/.local/bin/deadband`.
fn agent(home: &Path, launchd: &Arc<FakeLaunchd>) -> LaunchAgent {
    LaunchAgent {
        home: Some(home.to_path_buf()),
        binary: Some(PathBuf::from("/Users/me/.local/bin/deadband")),
        uid: 501,
        launchd: Arc::clone(launchd) as _,
    }
}

fn plist_path(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents/io.github.crumbkit.deadband.plist")
}

/// Returns the plist that install writes for `home`.
fn rendered(home: &Path) -> String {
    render(
        Path::new("/Users/me/.local/bin/deadband"),
        None,
        home,
        Path::new(TEMP),
    )
}

fn write_plist(home: &Path, text: &str) {
    std::fs::create_dir_all(home.join("Library/LaunchAgents")).expect("writable");
    #[expect(clippy::disallowed_methods, reason = "the test writes a fixture")]
    std::fs::write(plist_path(home), text).expect("writable");
}

const PLAIN: Install = Install {
    max_jobs: None,
    restart: false,
    dry_run: false,
};

/// Returns the text that `report` prints.
fn text(report: &crumbkit::Prose<super::Report>) -> String {
    report.value().to_string()
}

#[test]
fn render_writes_the_binary_daemon_and_environment_into_the_plist() {
    let text = render(
        Path::new("/Users/me/.local/bin/deadband"),
        None,
        Path::new("/Users/me"),
        Path::new(TEMP),
    );

    assert_eq!(
        text,
        include_str!("../../../tests/golden/service/plain.plist")
    );
}

#[test]
fn render_with_max_jobs_passes_the_flag_to_the_daemon() {
    let text = render(
        Path::new("/Users/me/.local/bin/deadband"),
        NonZeroUsize::new(2),
        Path::new("/Users/me"),
        Path::new(TEMP),
    );

    assert_eq!(
        text,
        include_str!("../../../tests/golden/service/max-jobs.plist")
    );
}

#[test]
fn render_names_a_symlinked_binary_by_its_link_and_puts_the_links_directory_first_on_path() {
    let text = render(
        Path::new("/opt/homebrew/bin/deadband"),
        None,
        Path::new("/Users/me"),
        Path::new(TEMP),
    );

    assert_eq!(
        text,
        include_str!("../../../tests/golden/service/symlink.plist")
    );
}

#[test]
fn binary_path_keeps_a_symbolic_link_as_invoked() {
    let home = TestHome::new();
    let target = home.join("deadband-0.2.1");
    #[expect(clippy::disallowed_methods, reason = "the test writes a fixture")]
    std::fs::write(&target, "").expect("writable");
    let link = home.join("deadband");
    std::os::unix::fs::symlink(&target, &link).expect("a link");

    assert_eq!(binary_path(&link, None), link);
}

#[test]
fn binary_path_makes_a_relative_path_absolute_from_the_working_directory() {
    let path = binary_path(
        Path::new("target/debug/deadband"),
        Some(Path::new("/src/db")),
    );

    assert_eq!(path, PathBuf::from("/src/db/target/debug/deadband"));
}

#[test]
fn a_fresh_install_writes_the_plist_and_loads_the_agent() {
    let home = TestHome::new();
    let launchd = Arc::new(FakeLaunchd::new(false));

    let report = install(Some(&agent(&home, &launchd)), &PLAIN, || 0).expect("installs");

    assert_eq!(
        std::fs::read_to_string(plist_path(&home)).expect("written"),
        rendered(&home)
    );
    assert_eq!(
        launchd.changes(),
        [format!("bootstrap gui/501 {}", plist_path(&home).display())]
    );
    assert_eq!(
        text(&report),
        "wrote ~/Library/LaunchAgents/io.github.crumbkit.deadband.plist\nloaded io.github.crumbkit.deadband"
    );
}

#[test]
fn an_unchanged_rerun_changes_nothing_and_says_so() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let launchd = Arc::new(FakeLaunchd::new(true));

    let report = install(Some(&agent(&home, &launchd)), &PLAIN, || 0).expect("installs");

    assert!(launchd.changes().is_empty(), "{:?}", launchd.changes());
    assert_eq!(
        text(&report),
        "left ~/Library/LaunchAgents/io.github.crumbkit.deadband.plist unchanged\nio.github.crumbkit.deadband is already loaded, so nothing changed"
    );
}

#[test]
fn an_unchanged_plist_that_launchd_has_not_loaded_is_loaded() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let launchd = Arc::new(FakeLaunchd::new(false));

    install(Some(&agent(&home, &launchd)), &PLAIN, || 0).expect("installs");

    assert_eq!(
        launchd.changes(),
        [format!("bootstrap gui/501 {}", plist_path(&home).display())]
    );
}

#[test]
fn a_changed_plist_with_no_jobs_reloads_the_agent() {
    let home = TestHome::new();
    write_plist(&home, "<plist version=\"1.0\"><dict></dict></plist>\n");
    let launchd = Arc::new(FakeLaunchd::new(true));

    let report = install(Some(&agent(&home, &launchd)), &PLAIN, || 0).expect("installs");

    assert_eq!(
        std::fs::read_to_string(plist_path(&home)).expect("written"),
        rendered(&home)
    );
    assert_eq!(
        launchd.changes(),
        [
            "bootout gui/501/io.github.crumbkit.deadband".to_owned(),
            format!("bootstrap gui/501 {}", plist_path(&home).display())
        ]
    );
    assert!(
        text(&report).contains("\nunloaded io.github.crumbkit.deadband\nloaded"),
        "{}",
        text(&report)
    );
}

#[test]
fn a_changed_plist_with_jobs_is_written_and_the_running_daemon_is_left_alone() {
    let home = TestHome::new();
    write_plist(&home, "<plist version=\"1.0\"><dict></dict></plist>\n");
    let launchd = Arc::new(FakeLaunchd::new(true));

    let report = install(Some(&agent(&home, &launchd)), &PLAIN, || 2).expect("installs");

    assert_eq!(
        std::fs::read_to_string(plist_path(&home)).expect("written"),
        rendered(&home)
    );
    assert!(launchd.changes().is_empty(), "{:?}", launchd.changes());
    assert!(
        text(&report).ends_with(
            "the daemon has 2 jobs, so it keeps running with the old plist; run deadband service install again when no job runs, or add --restart"
        ),
        "{}",
        text(&report)
    );
}

#[test]
fn a_changed_plist_with_jobs_and_restart_reloads_the_agent() {
    let home = TestHome::new();
    write_plist(&home, "<plist version=\"1.0\"><dict></dict></plist>\n");
    let launchd = Arc::new(FakeLaunchd::new(true));
    let restart = Install {
        restart: true,
        ..PLAIN
    };

    install(Some(&agent(&home, &launchd)), &restart, || 2).expect("installs");

    assert_eq!(launchd.changes().len(), 2, "{:?}", launchd.changes());
    assert!(
        launchd.changes()[0].starts_with("bootout"),
        "{:?}",
        launchd.changes()
    );
}

#[test]
fn a_dry_run_prints_the_plist_and_the_steps_and_changes_nothing() {
    let home = TestHome::new();
    let launchd = Arc::new(FakeLaunchd::new(false));
    let dry = Install {
        dry_run: true,
        ..PLAIN
    };

    let report = install(Some(&agent(&home, &launchd)), &dry, || 0).expect("plans");

    assert!(!plist_path(&home).exists());
    assert!(launchd.changes().is_empty(), "{:?}", launchd.changes());
    assert_eq!(
        text(&report),
        format!(
            "{}would write ~/Library/LaunchAgents/io.github.crumbkit.deadband.plist\nwould load io.github.crumbkit.deadband",
            rendered(&home)
        )
    );
}

#[test]
fn install_refuses_an_existing_file_that_is_not_a_plist_and_writes_nothing() {
    let home = TestHome::new();
    write_plist(&home, "not a plist\n");
    let launchd = Arc::new(FakeLaunchd::new(false));

    let refused = install(Some(&agent(&home, &launchd)), &PLAIN, || 0);

    assert!(
        matches!(refused, Err(ServiceFailed::Unreadable { .. })),
        "{refused:?}"
    );
    assert_eq!(
        std::fs::read_to_string(plist_path(&home)).expect("kept"),
        "not a plist\n"
    );
    assert!(launchd.changes().is_empty(), "{:?}", launchd.changes());
}

#[test]
fn install_without_a_launch_agent_refuses_as_unsupported() {
    let refused = install(None, &PLAIN, || 0);

    assert!(
        matches!(refused, Err(ServiceFailed::Unsupported)),
        "{refused:?}"
    );
}

#[test]
fn a_failed_bootstrap_leaves_the_plist_installed_but_not_loaded() {
    let home = TestHome::new();
    let launchd = Arc::new(FakeLaunchd::failing(false, "bootstrap"));
    let agent = agent(&home, &launchd);

    let refused = install(Some(&agent), &PLAIN, || 0);

    assert!(
        matches!(refused, Err(ServiceFailed::Launchctl(_))),
        "{refused:?}"
    );
    assert_eq!(state(&agent), Some(DaemonState::NotLoaded));
}

#[test]
fn uninstall_of_an_agent_that_is_not_loaded_deletes_the_plist_without_bootout() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let launchd = Arc::new(FakeLaunchd::new(false));

    let report = uninstall(Some(&agent(&home, &launchd)), false).expect("uninstalls");

    assert!(!plist_path(&home).exists());
    assert!(launchd.changes().is_empty(), "{:?}", launchd.changes());
    assert_eq!(
        text(&report),
        "io.github.crumbkit.deadband was not loaded\ndeleted ~/Library/LaunchAgents/io.github.crumbkit.deadband.plist"
    );
}

#[test]
fn uninstall_of_a_loaded_agent_boots_it_out_and_then_deletes_the_plist() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let launchd = Arc::new(FakeLaunchd::new(true));

    uninstall(Some(&agent(&home, &launchd)), false).expect("uninstalls");

    assert!(!plist_path(&home).exists());
    assert_eq!(
        launchd.changes(),
        ["bootout gui/501/io.github.crumbkit.deadband"]
    );
}

#[test]
fn a_failed_bootout_keeps_the_plist() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let launchd = Arc::new(FakeLaunchd::failing(true, "bootout"));

    let refused = uninstall(Some(&agent(&home, &launchd)), false);

    assert!(
        matches!(refused, Err(ServiceFailed::Launchctl(_))),
        "{refused:?}"
    );
    assert!(plist_path(&home).exists());
}

#[test]
fn a_dry_run_of_uninstall_changes_nothing() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let launchd = Arc::new(FakeLaunchd::new(true));

    let report = uninstall(Some(&agent(&home, &launchd)), true).expect("plans");

    assert!(plist_path(&home).exists());
    assert!(launchd.changes().is_empty(), "{:?}", launchd.changes());
    assert!(
        text(&report).starts_with("would unload"),
        "{}",
        text(&report)
    );
}

#[test]
fn state_names_a_missing_plist_and_a_plist_that_launchd_has_not_loaded() {
    let home = TestHome::new();
    let unloaded = Arc::new(FakeLaunchd::new(false));
    assert_eq!(
        state(&agent(&home, &unloaded)),
        Some(DaemonState::NotInstalled)
    );

    write_plist(&home, &rendered(&home));
    assert_eq!(
        state(&agent(&home, &unloaded)),
        Some(DaemonState::NotLoaded)
    );

    let loaded = Arc::new(FakeLaunchd::new(true));
    assert_eq!(state(&agent(&home, &loaded)), None);
}
