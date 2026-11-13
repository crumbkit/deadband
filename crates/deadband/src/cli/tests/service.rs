//! Tests of `deadband service` and of the daemon's state that `deadband
//! status` names, run against a fake launchd and a temporary home.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use super::{Fixed, Test, ask};
use crumbkit::Sink;

use crate::cli::{EXIT_SERVICE, LaunchAgent, run_to};
use crate::daemon::{Settings, bind, serve};
use crate::protocol::Reply;
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
fn status_names_the_log_when_launchd_has_loaded_the_agent_and_no_daemon_answers() {
    let home = TestHome::new();
    write_plist(&home);
    let test = with_agent(&home, &Arc::new(FakeLaunchd::new(true)));

    let (_, out, _) = test.run(&["status"]);

    assert!(
        out.ends_with("daemon: loaded but not answering; see ~/Library/Logs/deadband.log\n"),
        "{out}"
    );
}

#[test]
fn status_says_launchd_could_not_be_asked_when_launchctl_fails() {
    let home = TestHome::new();
    write_plist(&home);
    let test = with_agent(&home, &Arc::new(FakeLaunchd::failing(true, "print")));

    let (_, out, _) = test.run(&["status"]);

    assert!(
        out.ends_with(
            "daemon: not running, and launchd could not be asked whether it loaded the agent\n"
        ),
        "{out}"
    );
    let (_, json, _) = test.run(&["status", "--json"]);
    assert!(
        json.contains(r#""daemon_state":"launchd_unasked""#),
        "{json}"
    );
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

    *launchd.loaded.lock().expect("the loaded lock") = true;
    let (_, out, _) = test.run(&["status", "--json"]);
    assert!(out.contains(r#""daemon_state":"loaded_silent""#), "{out}");
    *launchd.loaded.lock().expect("the loaded lock") = false;

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
        "wrote ~/Library/LaunchAgents/io.github.crumbkit.deadband.plist\nloaded io.github.crumbkit.deadband, daemon does not answer; see ~/Library/Logs/deadband.log\n"
    );
}

#[test]
fn service_uninstall_asks_with_the_jobs_of_the_daemon_under_the_getconf_directory() {
    let dir = short_dir("ask");
    let home = TestHome::new();
    write_plist(&home);
    let launchd = Arc::new(FakeLaunchd {
        temp: Some(dir.0.clone()),
        ..FakeLaunchd::new(true)
    });
    let test = with_agent(&home, &launchd);
    // The caller's socket has no daemon; the one that launchd runs listens
    // under the getconf directory.
    let real = daemon_in(&test, &dir, 1);
    let (_job, _) = ask(&real, "build");
    let (_waiting, queued) = ask(&real, "lint");
    assert!(!matches!(queued, Reply::Granted { .. }), "{queued:?}");

    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut sink = Sink::with_input(&mut out, &mut err, std::io::empty(), true);
    let code = run_to(
        &mut sink,
        &test.machine,
        ["deadband", "service", "uninstall"],
    );
    assert_eq!(code, 1);
    let err = String::from_utf8(err).expect("UTF-8");
    assert!(
        err.contains("The daemon has 1 running and 1 waiting job. Unload deadband's launchd agent and delete its plist? [y/N] "),
        "{err}"
    );

    let (code, out, err) = test.run(&["service", "uninstall", "--yes", "--no-input"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("the daemon has 1 running and 1 waiting job; "),
        "{out}"
    );
}

#[test]
fn service_uninstall_names_the_socket_and_lock_files_that_stay() {
    let home = TestHome::new();
    write_plist(&home);
    let launchd = Arc::new(FakeLaunchd::new(true));
    let test = with_agent(&home, &launchd);
    test.start_daemon(1);
    let socket = test.machine.socket.display().to_string();

    let (code, out, err) = test.run(&["service", "uninstall", "--yes", "--no-input"]);

    assert_eq!(code, 0, "{err}");
    let last = out.lines().last().expect("a line");
    assert!(
        last.starts_with(&format!("left in place: {socket}, {socket}.lock")),
        "{out}"
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
        out.lines().skip(1).take(2).collect::<Vec<_>>(),
        [
            "unloaded io.github.crumbkit.deadband",
            "deleted ~/Library/LaunchAgents/io.github.crumbkit.deadband.plist"
        ]
    );
}

/// A directory under `/tmp`, short enough for a socket path, that only this
/// test uses. Dropping it removes the directory.
struct ShortDir(PathBuf);

impl Drop for ShortDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Returns a directory named for `name` under `/tmp`.
fn short_dir(name: &str) -> ShortDir {
    let dir = ShortDir(PathBuf::from(format!(
        "/tmp/dbs-{}-{name}",
        std::process::id()
    )));
    std::fs::create_dir_all(&dir.0).expect("writable");
    dir
}

/// Starts a daemon that runs `max_jobs` jobs at once on the socket under
/// `dir`, where a launch agent's daemon listens, and returns that socket.
fn daemon_in(test: &Test<Fixed>, dir: &ShortDir, max_jobs: usize) -> PathBuf {
    let real = dir.0.join("deadband.sock");
    let bound = bind(&real).expect("the socket binds");
    let log = Arc::clone(&test.machine.log);
    thread::spawn(move || {
        let bound = bound;
        let max_jobs = NonZeroUsize::new(max_jobs).expect("positive");
        let settings = Settings::new(max_jobs, NonZeroUsize::new(4), None, None);
        serve(&bound.listener, Fixed, settings, &log)
    });
    real
}

#[test]
fn service_uninstall_of_an_unloaded_agent_does_not_ask_a_daemon_about_its_jobs() {
    let home = TestHome::new();
    write_plist(&home);
    let launchd = Arc::new(FakeLaunchd::new(false));
    let test = with_agent(&home, &launchd);
    test.start_daemon(1);
    let (_job, _) = ask(&test.machine.socket, "build");

    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut sink = Sink::with_input(&mut out, &mut err, std::io::empty(), true);
    run_to(
        &mut sink,
        &test.machine,
        ["deadband", "service", "uninstall"],
    );

    let err = String::from_utf8(err).expect("UTF-8");
    assert!(
        err.contains("Unload deadband's launchd agent and delete its plist? [y/N] "),
        "{err}"
    );
    assert!(!err.contains("The daemon has"), "{err}");
}

#[test]
fn service_install_counts_the_jobs_of_the_daemon_under_the_getconf_directory_whatever_the_callers_tmpdir()
 {
    let dir = short_dir("install");
    let home = TestHome::new();
    std::fs::create_dir_all(home.join("Library/LaunchAgents")).expect("writable");
    #[expect(clippy::disallowed_methods, reason = "the test writes a fixture")]
    std::fs::write(
        plist_path(&home),
        "<plist version=\"1.0\"><dict></dict></plist>\n",
    )
    .expect("writable");
    let launchd = Arc::new(FakeLaunchd {
        temp: Some(dir.0.clone()),
        ..FakeLaunchd::new(true)
    });
    // The caller's socket, from its own TMPDIR, has no daemon; the daemon
    // that launchd runs listens under the getconf directory and runs a job.
    let test = with_agent(&home, &launchd);
    let real = daemon_in(&test, &dir, 2);
    let (_job, reply) = ask(&real, "build");
    assert!(matches!(reply, Reply::Granted { .. }), "{reply:?}");

    let (code, out, err) = test.run(&["service", "install"]);

    assert_eq!(code, 0, "{err}");
    assert!(out.starts_with("the daemon has 1 job, "), "{out}");
    assert!(launchd.changes().is_empty(), "{:?}", launchd.changes());
}
