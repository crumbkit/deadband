//! Tests of `deadband service install` and `uninstall`, run against a fake
//! launchd and a temporary home.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::plist::{binary_path, render};
use super::{Counts, DaemonState, Install, LaunchAgent, ServiceFailed, install, state, uninstall};
use crate::testing::{FakeLaunchd, TestHome};

/// A daemon with one running job and one waiting job.
const TWO: Counts = Counts {
    running: 1,
    waiting: 1,
};

/// The temporary directory that the fake launchd returns as the per-user directory.
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

/// A plist that differs from what install renders.
const OLD: &str = "<plist version=\"1.0\"><dict></dict></plist>\n";

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

    let report = install(Some(&agent(&home, &launchd)), &PLAIN, |_| {
        Some(Counts::default())
    })
    .expect("installs");

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
        "wrote ~/Library/LaunchAgents/io.github.crumbkit.deadband.plist\nloaded io.github.crumbkit.deadband, daemon answers"
    );
}

#[test]
fn an_install_whose_daemon_never_answers_says_so_and_names_the_log() {
    let home = TestHome::new();
    let launchd = Arc::new(FakeLaunchd::new(false));

    let report = install(Some(&agent(&home, &launchd)), &PLAIN, |_| None).expect("installs");

    assert!(
        text(&report).ends_with(
            "loaded io.github.crumbkit.deadband, daemon does not answer; see ~/Library/Logs/deadband.log"
        ),
        "{}",
        text(&report)
    );
    let calls = launchd.calls.lock().expect("the calls lock").clone();
    assert!(calls.iter().any(|call| call == "pause"), "{calls:?}");
}

#[test]
fn an_install_waits_for_the_daemon_to_answer_after_loading_the_agent() {
    let home = TestHome::new();
    let launchd = Arc::new(FakeLaunchd::new(false));
    let mut asks = 0;

    let report = install(Some(&agent(&home, &launchd)), &PLAIN, |_| {
        asks += 1;
        (asks > 3).then(Counts::default)
    })
    .expect("installs");

    assert!(
        text(&report).ends_with("loaded io.github.crumbkit.deadband, daemon answers"),
        "{}",
        text(&report)
    );
}

#[test]
fn a_dry_run_install_does_not_wait_for_a_daemon() {
    let home = TestHome::new();
    let launchd = Arc::new(FakeLaunchd::new(false));
    let dry = Install {
        dry_run: true,
        ..PLAIN
    };

    let report = install(Some(&agent(&home, &launchd)), &dry, |_| None).expect("plans");

    assert!(
        text(&report).ends_with("would load io.github.crumbkit.deadband"),
        "{}",
        text(&report)
    );
}

#[test]
fn restart_prints_the_running_and_waiting_jobs_before_it_unloads_the_agent() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let launchd = Arc::new(FakeLaunchd::new(true));
    let restart = Install {
        restart: true,
        ..PLAIN
    };

    let report = install(Some(&agent(&home, &launchd)), &restart, |_| {
        Some(Counts {
            running: 2,
            waiting: 3,
        })
    })
    .expect("installs");

    assert!(
        text(&report).starts_with(
            "the daemon has 2 running and 3 waiting jobs; the running jobs keep running and the waiting jobs start at once\nunloaded io.github.crumbkit.deadband\n"
        ),
        "{}",
        text(&report)
    );
}

#[test]
fn restart_says_when_the_daemon_does_not_answer_and_still_reloads() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let launchd = Arc::new(FakeLaunchd::new(true));
    let restart = Install {
        restart: true,
        ..PLAIN
    };

    let report = install(Some(&agent(&home, &launchd)), &restart, |_| None).expect("installs");

    assert!(
        text(&report).starts_with(
            "the daemon did not answer, so deadband cannot say how many jobs it holds\nunloaded "
        ),
        "{}",
        text(&report)
    );
    assert_eq!(launchd.changes().len(), 2, "{:?}", launchd.changes());
}

#[test]
fn uninstall_of_a_loaded_agent_prints_the_jobs_the_stopped_daemon_held() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let launchd = Arc::new(FakeLaunchd::new(true));
    let held = Counts {
        running: 1,
        waiting: 1,
    };

    let report =
        uninstall(Some(&agent(&home, &launchd)), false, Some(held), &[]).expect("uninstalls");

    assert!(
        text(&report).starts_with(
            "the daemon has 1 running and 1 waiting job; the running jobs keep running and the waiting jobs start at once\nunloaded "
        ),
        "{}",
        text(&report)
    );
}

#[test]
fn uninstall_of_a_loaded_agent_says_when_the_daemon_did_not_answer() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let launchd = Arc::new(FakeLaunchd::new(true));

    let report = uninstall(Some(&agent(&home, &launchd)), false, None, &[]).expect("uninstalls");

    assert!(
        text(&report).starts_with(
            "the daemon did not answer, so deadband cannot say how many jobs it holds\nunloaded "
        ),
        "{}",
        text(&report)
    );
}

#[test]
fn uninstall_of_an_agent_that_is_not_loaded_prints_no_job_counts() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let launchd = Arc::new(FakeLaunchd::new(false));

    let report = uninstall(Some(&agent(&home, &launchd)), false, Some(TWO), &[]).expect("done");

    assert!(
        text(&report).starts_with("io.github.crumbkit.deadband was not loaded\n"),
        "{}",
        text(&report)
    );
}

#[test]
fn uninstall_lists_the_files_it_leaves_and_skips_those_that_do_not_exist() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    std::fs::create_dir_all(home.join("Library/Logs")).expect("writable");
    #[expect(clippy::disallowed_methods, reason = "the test writes a fixture")]
    std::fs::write(home.join("Library/Logs/deadband.log"), "").expect("writable");
    let socket = home.join("deadband.sock");
    #[expect(clippy::disallowed_methods, reason = "the test writes a fixture")]
    std::fs::write(&socket, "").expect("writable");
    let missing = home.join("gone");
    let launchd = Arc::new(FakeLaunchd::new(false));

    let report = uninstall(
        Some(&agent(&home, &launchd)),
        false,
        None,
        &[socket.clone(), missing],
    )
    .expect("uninstalls");

    let last = text(&report).lines().last().expect("a line").to_owned();
    assert_eq!(
        last,
        "left in place: ~/deadband.sock, ~/Library/Logs/deadband.log"
    );
}

#[test]
fn a_dry_run_of_uninstall_says_which_files_it_would_leave() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let socket = home.join("deadband.sock");
    #[expect(clippy::disallowed_methods, reason = "the test writes a fixture")]
    std::fs::write(&socket, "").expect("writable");
    let launchd = Arc::new(FakeLaunchd::new(false));

    let report = uninstall(
        Some(&agent(&home, &launchd)),
        true,
        None,
        std::slice::from_ref(&socket),
    )
    .expect("plans");

    let last = text(&report).lines().last().expect("a line").to_owned();
    assert_eq!(last, "would leave in place: ~/deadband.sock");
}

#[test]
fn an_unchanged_rerun_changes_nothing_and_says_so() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let launchd = Arc::new(FakeLaunchd::new(true));

    let report = install(Some(&agent(&home, &launchd)), &PLAIN, |_| {
        Some(Counts::default())
    })
    .expect("installs");

    assert!(launchd.changes().is_empty(), "{:?}", launchd.changes());
    assert_eq!(
        text(&report),
        "~/Library/LaunchAgents/io.github.crumbkit.deadband.plist is up to date and io.github.crumbkit.deadband is loaded, so nothing changed"
    );
}

#[test]
fn an_unchanged_plist_that_launchd_has_not_loaded_is_loaded() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let launchd = Arc::new(FakeLaunchd::new(false));

    install(Some(&agent(&home, &launchd)), &PLAIN, |_| {
        Some(Counts::default())
    })
    .expect("installs");

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

    let report = install(Some(&agent(&home, &launchd)), &PLAIN, |_| {
        Some(Counts::default())
    })
    .expect("installs");

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
        text(&report).starts_with(
            "unloaded io.github.crumbkit.deadband\nwrote ~/Library/LaunchAgents/io.github.crumbkit.deadband.plist\nloaded"
        ),
        "{}",
        text(&report)
    );
}

#[test]
fn a_changed_plist_with_jobs_keeps_the_old_plist_and_leaves_the_running_daemon_alone() {
    let home = TestHome::new();
    write_plist(&home, OLD);
    let launchd = Arc::new(FakeLaunchd::new(true));

    let report = install(Some(&agent(&home, &launchd)), &PLAIN, |_| Some(TWO)).expect("installs");

    assert_eq!(
        std::fs::read_to_string(plist_path(&home)).expect("kept"),
        OLD
    );
    assert!(launchd.changes().is_empty(), "{:?}", launchd.changes());
    assert_eq!(
        text(&report),
        "the daemon has 2 jobs, so install left the old plist in place; the new plist waits for deadband service install when no job runs, or for --restart",
    );
}

#[test]
fn a_rerun_after_jobs_held_the_reload_reloads_once_the_daemon_is_idle() {
    let home = TestHome::new();
    write_plist(&home, OLD);
    let launchd = Arc::new(FakeLaunchd::new(true));
    let agent = agent(&home, &launchd);
    install(Some(&agent), &PLAIN, |_| Some(TWO)).expect("installs");

    install(Some(&agent), &PLAIN, |_| Some(Counts::default())).expect("installs");

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
}

#[test]
fn restart_reloads_a_loaded_agent_whose_plist_is_unchanged() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let launchd = Arc::new(FakeLaunchd::new(true));
    let restart = Install {
        restart: true,
        ..PLAIN
    };

    install(Some(&agent(&home, &launchd)), &restart, |_| Some(TWO)).expect("installs");

    assert_eq!(
        launchd.changes(),
        [
            "bootout gui/501/io.github.crumbkit.deadband".to_owned(),
            format!("bootstrap gui/501 {}", plist_path(&home).display())
        ]
    );
}

#[test]
fn install_asks_the_daemon_on_the_socket_under_the_getconf_temporary_directory() {
    let home = TestHome::new();
    write_plist(&home, OLD);
    let launchd = Arc::new(FakeLaunchd::new(true));
    let mut asked = None;

    install(Some(&agent(&home, &launchd)), &PLAIN, |socket| {
        asked = Some(socket.to_path_buf());
        Some(TWO)
    })
    .expect("installs");

    assert_eq!(asked, Some(PathBuf::from(TEMP).join("deadband.sock")));
}

#[test]
fn a_failed_bootout_during_install_keeps_the_old_plist_so_a_rerun_reloads() {
    let home = TestHome::new();
    write_plist(&home, OLD);
    let failing = Arc::new(FakeLaunchd::failing(true, "bootout"));

    let refused = install(Some(&agent(&home, &failing)), &PLAIN, |_| {
        Some(Counts::default())
    });

    assert!(
        matches!(refused, Err(ServiceFailed::Launchctl(_))),
        "{refused:?}"
    );
    assert_eq!(
        std::fs::read_to_string(plist_path(&home)).expect("kept"),
        OLD
    );
    let launchd = Arc::new(FakeLaunchd::new(true));
    install(Some(&agent(&home, &launchd)), &PLAIN, |_| {
        Some(Counts::default())
    })
    .expect("installs");
    assert_eq!(launchd.changes().len(), 2, "{:?}", launchd.changes());
}

#[test]
fn a_failed_bootstrap_after_bootout_leaves_the_agent_not_loaded_and_a_rerun_loads_it() {
    let home = TestHome::new();
    write_plist(&home, OLD);
    let failing = Arc::new(FakeLaunchd::failing(true, "bootstrap"));
    let failing_agent = agent(&home, &failing);

    let refused = install(Some(&failing_agent), &PLAIN, |_| Some(Counts::default()));

    assert!(
        matches!(refused, Err(ServiceFailed::Launchctl(_))),
        "{refused:?}"
    );
    assert_eq!(state(&failing_agent), Some(DaemonState::NotLoaded));
    let launchd = Arc::new(FakeLaunchd::new(false));
    install(Some(&agent(&home, &launchd)), &PLAIN, |_| {
        Some(Counts::default())
    })
    .expect("installs");
    assert_eq!(
        launchd.changes(),
        [format!("bootstrap gui/501 {}", plist_path(&home).display())]
    );
}

#[test]
fn a_reload_waits_until_launchd_has_unloaded_the_old_daemon_before_bootstrap() {
    let home = TestHome::new();
    write_plist(&home, OLD);
    let launchd = Arc::new(FakeLaunchd {
        linger: 3,
        ..FakeLaunchd::new(true)
    });

    install(Some(&agent(&home, &launchd)), &PLAIN, |_| {
        Some(Counts::default())
    })
    .expect("installs");

    let calls = launchd.calls.lock().expect("the calls lock").clone();
    let pauses = calls.iter().filter(|call| call.as_str() == "pause").count();
    assert_eq!(pauses, 3, "{calls:?}");
    assert!(
        calls
            .last()
            .is_some_and(|call| call.starts_with("bootstrap")),
        "{calls:?}"
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

    install(Some(&agent(&home, &launchd)), &restart, |_| Some(TWO)).expect("installs");

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

    let report = install(Some(&agent(&home, &launchd)), &dry, |_| {
        Some(Counts::default())
    })
    .expect("plans");

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

    let refused = install(Some(&agent(&home, &launchd)), &PLAIN, |_| {
        Some(Counts::default())
    });

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
    let refused = install(None, &PLAIN, |_| Some(Counts::default()));

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

    let refused = install(Some(&agent), &PLAIN, |_| Some(Counts::default()));

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

    let report = uninstall(Some(&agent(&home, &launchd)), false, None, &[]).expect("uninstalls");

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

    uninstall(Some(&agent(&home, &launchd)), false, None, &[]).expect("uninstalls");

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

    let refused = uninstall(Some(&agent(&home, &launchd)), false, None, &[]);

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

    let report = uninstall(Some(&agent(&home, &launchd)), true, None, &[]).expect("plans");

    assert!(plist_path(&home).exists());
    assert!(launchd.changes().is_empty(), "{:?}", launchd.changes());
    assert!(
        text(&report).contains("\nwould unload"),
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

#[test]
fn a_loaded_agent_whose_daemon_does_not_answer_holds_the_reload_and_keeps_the_old_plist() {
    let home = TestHome::new();
    write_plist(&home, OLD);
    let launchd = Arc::new(FakeLaunchd::new(true));

    let report = install(Some(&agent(&home, &launchd)), &PLAIN, |_| None).expect("installs");

    assert_eq!(
        std::fs::read_to_string(plist_path(&home)).expect("kept"),
        OLD
    );
    assert!(launchd.changes().is_empty(), "{:?}", launchd.changes());
    assert_eq!(
        text(&report),
        "the daemon did not answer, so install left the old plist in place; the new plist waits for deadband service install when the daemon answers with no jobs, or for --restart",
    );
}

#[test]
fn a_reload_whose_old_daemon_outlasts_the_polls_fails_at_bootstrap_after_fifty_polls() {
    let home = TestHome::new();
    write_plist(&home, OLD);
    let launchd = Arc::new(FakeLaunchd {
        linger: 60,
        ..FakeLaunchd::new(true)
    });

    let refused = install(Some(&agent(&home, &launchd)), &PLAIN, |_| {
        Some(Counts::default())
    });

    assert!(
        matches!(refused, Err(ServiceFailed::Launchctl(_))),
        "{refused:?}"
    );
    let calls = launchd.calls.lock().expect("the calls lock").clone();
    let pauses = calls.iter().filter(|call| call.as_str() == "pause").count();
    assert_eq!(pauses, 50, "{calls:?}");
}

/// Makes the plist's directory read-only, or writable again.
fn set_read_only(home: &Path, read_only: bool) {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = if read_only { 0o555 } else { 0o755 };
    std::fs::set_permissions(
        home.join("Library/LaunchAgents"),
        std::fs::Permissions::from_mode(mode),
    )
    .expect("the mode changes");
}

#[test]
fn a_write_that_fails_after_bootout_leaves_the_old_plist_not_loaded_and_a_rerun_loads_the_new_one()
{
    let home = TestHome::new();
    write_plist(&home, OLD);
    let launchd = Arc::new(FakeLaunchd::new(true));
    let agent = agent(&home, &launchd);
    set_read_only(&home, true);

    let refused = install(Some(&agent), &PLAIN, |_| Some(Counts::default()));

    set_read_only(&home, false);
    assert!(
        matches!(refused, Err(ServiceFailed::Write { .. })),
        "{refused:?}"
    );
    assert_eq!(
        std::fs::read_to_string(plist_path(&home)).expect("kept"),
        OLD
    );
    assert_eq!(state(&agent), Some(DaemonState::NotLoaded));
    install(Some(&agent), &PLAIN, |_| Some(Counts::default())).expect("installs");
    assert_eq!(
        std::fs::read_to_string(plist_path(&home)).expect("written"),
        rendered(&home)
    );
}

#[test]
fn a_delete_that_fails_after_uninstalls_bootout_keeps_the_plist_and_a_rerun_deletes_it() {
    let home = TestHome::new();
    write_plist(&home, &rendered(&home));
    let launchd = Arc::new(FakeLaunchd::new(true));
    let agent = agent(&home, &launchd);
    set_read_only(&home, true);

    let refused = uninstall(Some(&agent), false, None, &[]);

    set_read_only(&home, false);
    assert!(
        matches!(refused, Err(ServiceFailed::Delete { .. })),
        "{refused:?}"
    );
    assert!(plist_path(&home).exists());
    uninstall(Some(&agent), false, None, &[]).expect("uninstalls");
    assert!(!plist_path(&home).exists());
}
