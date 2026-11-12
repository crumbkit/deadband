//! The plan's acceptance checks of reclaiming: the daemon runs the
//! reclaimer of its config file under memory pressure, and `deadband
//! status --json` shows the reclaim. `deadband daemon` reads the file.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crumbkit::Sink;

use super::{REPLY_WITHIN, Test, ask};
use crate::admit::GIB;
use crate::cli::{Machine, run_to};
use crate::daemon::{Settings, bind, serve};
use crate::probe::{Disk, Pressure, Probe, Sample, Unreadable};
use crate::protocol::Reply;
use crate::reclaim::{CONFIG_FILE, Config};

/// A probe whose memory pressure is always warn, the test hook that forces
/// the daemon to reclaim.
#[derive(Clone)]
struct Warn;

impl Probe for Warn {
    fn sample(&self) -> Result<Sample, Unreadable> {
        Ok(Sample {
            load_1m: 1.0,
            memory_pressure: Pressure::Warn,
            swap_used_bytes: 0,
            swap_total_bytes: 0,
        })
    }

    fn disk(&self, _path: &Path) -> std::io::Result<Disk> {
        Ok(Disk {
            free_bytes: 500 * GIB,
            total_bytes: 1000 * GIB,
        })
    }
}

/// A configuration directory with a `config.toml` that names one
/// reclaimer, a shell script that appends its arguments to `calls` and
/// runs `body`. It is removed when the test ends.
struct Reclaimer(PathBuf);

impl Reclaimer {
    fn with(timeout: &str, body: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let directory = std::env::temp_dir().join(format!("db-reclaim-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("the directory is made");
        let script = directory.join("reclaimer.sh");
        let calls = directory.join("calls");
        let config = format!(
            "reclaim_after = \"10s\"\nreclaim_timeout = \"{timeout}\"\n\n\
             [[reclaim]]\nname = \"workspaces\"\ncommand = [\"/bin/sh\", \"{}\"]\n",
            script.display()
        );
        #[expect(
            clippy::disallowed_methods,
            reason = "the test writes the config file and the script a person would write"
        )]
        {
            std::fs::write(
                &script,
                format!("echo \"$*\" >> '{}'\n{body}\n", calls.display()),
            )
            .expect("the script writes");
            std::fs::write(directory.join("config.toml"), config).expect("the config writes");
        }
        Self(directory)
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.0.join("calls")).unwrap_or_default()
    }
}

impl Drop for Reclaimer {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Starts `deadband daemon` on the test's machine in a thread, as the
/// command line would, and waits until it answers.
fn start_daemon_command(test: &Test<Warn>) {
    let machine = Machine {
        probe: Warn,
        socket: test.machine.socket.clone(),
        log: test.machine.log.clone(),
        cores: test.machine.cores,
        environment: test.machine.environment.clone(),
        service: None,
    };
    thread::spawn(move || {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        run_to(
            &mut Sink::new(&mut out, &mut err),
            &machine,
            ["deadband", "daemon", "--max-jobs", "2"],
        )
    });
    wait_for_daemon(test);
}

/// How long memory stays short before the test daemon reclaims. The config
/// file refuses less than 10 seconds, so the test sets it after reading the
/// file.
const TEST_AFTER: Duration = Duration::from_millis(50);

/// Starts a daemon on the test's machine with the reclaimers of the
/// config file in `reclaimer`, which reclaims after [`TEST_AFTER`], and
/// waits until it answers.
fn start_daemon(test: &Test<Warn>, reclaimer: &Reclaimer) {
    let bound = bind(&test.machine.socket).expect("the test socket binds");
    let log = Arc::clone(&test.machine.log);
    let mut config = Config::load(&reclaimer.0.join(CONFIG_FILE)).expect("the config reads");
    config.after = TEST_AFTER;
    thread::spawn(move || {
        let bound = bound;
        let max_jobs = NonZeroUsize::new(2).expect("positive");
        let mut settings = Settings::new(max_jobs, NonZeroUsize::new(4), None, None);
        settings.reclaim = config;
        serve(&bound.listener, Warn, settings, &log)
    });
    wait_for_daemon(test);
}

/// Waits until the daemon on the test's socket answers.
fn wait_for_daemon(test: &Test<Warn>) {
    let deadline = Instant::now() + REPLY_WITHIN;
    while crate::client::jobs(&test.machine.socket, REPLY_WITHIN).is_err() {
        assert!(Instant::now() < deadline, "the daemon answers");
        thread::yield_now();
    }
}

/// Asks for `deadband status --json` until `done` holds for its output, or
/// fails after [`REPLY_WITHIN`].
fn status_until(test: &Test<Warn>, done: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + REPLY_WITHIN;
    loop {
        let (code, out, err) = test.run(&["status", "--json"]);
        assert_eq!(code, 0, "{err}");
        if done(&out) {
            return out;
        }
        assert!(Instant::now() < deadline, "{out}\n{}", test.log());
    }
}

#[test]
fn under_warn_pressure_the_daemon_runs_the_reclaimer_dry_then_for_real_and_status_shows_it() {
    let reclaimer = Reclaimer::with(
        "10s",
        r#"echo '{"data":[{"name":"idle-one","state":"stopped","freed":5368709120}]}'"#,
    );
    let test = Test::new(Warn);
    start_daemon(&test, &reclaimer);
    let out = status_until(&test, |out| out.contains(r#""freed_bytes":5368709120"#));
    assert!(out.contains(r#""reclaimer":"workspaces""#), "{out}");
    assert!(out.contains(r#""need":"memory=4G""#), "{out}");
    assert!(out.contains(r#""failed":null"#), "{out}");
    let calls = reclaimer.calls();
    assert!(
        calls.starts_with("--need memory=4G --dry-run\n--need memory=4G\n"),
        "{calls}"
    );
    let log = test.log();
    assert!(
        log.contains("the reclaimer workspaces would free 5 GiB for memory=4G: idle-one"),
        "{log}"
    );
}

#[test]
fn a_reclaimer_that_runs_past_its_timeout_is_logged_and_admission_continues() {
    let reclaimer = Reclaimer::with("100ms", "exec sleep 3");
    let test = Test::new(Warn);
    start_daemon(&test, &reclaimer);
    let out = status_until(&test, |out| out.contains("ran past its timeout"));
    assert!(out.contains(r#""reclaimer":"workspaces""#), "{out}");
    let log = test.log();
    assert!(
        log.contains("deadband: the reclaimer workspaces ran past its timeout of 100ms"),
        "{log}"
    );
    let (_job, reply) = ask(&test.machine.socket, "build");
    assert_eq!(reply, Reply::Granted { jobs: 2 });
}

#[test]
fn the_daemon_command_reads_its_reclaimers_from_the_config_file() {
    let reclaimer = Reclaimer::with("10s", "exit 0");
    let mut test = Test::new(Warn);
    test.machine.environment.config = Some(reclaimer.0.clone());
    start_daemon_command(&test);
    let log = test.log();
    assert!(
        log.contains("the daemon asks workspaces to reclaim when memory or disk runs short"),
        "{log}"
    );
}

#[test]
fn the_daemon_command_runs_no_reclaimer_and_says_why_when_its_config_is_refused() {
    let reclaimer = Reclaimer::with("0s", "exit 0");
    let mut test = Test::new(Warn);
    test.machine.environment.config = Some(reclaimer.0.clone());
    start_daemon_command(&test);
    let log = test.log();
    assert!(
        log.contains("reclaim_timeout is 0")
            && log.contains("so the daemon runs no reclaimer and no on_stuck command"),
        "{log}"
    );
}
