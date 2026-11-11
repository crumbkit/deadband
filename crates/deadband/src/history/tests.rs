//! Tests of the kind rule, the record types and the writer.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;

use super::writer::Writer;
use super::{
    Arrival, Departure, FILE, JobEnd, JobLine, Line, MachineLine, Place, ROTATED_FILE, Recorder,
    Tally, VERSION, WORKSPACE_MARKER, kind, options,
};
use crate::admit::Lane;
use crate::daemon::Log;
use crate::probe::{ChildUsage, Pressure};
use crate::protocol::{Exit, Job, Label};

/// How many lines each writer thread has handled, by its state directory.
/// Only test builds count them, so that a daemon test can wait for the
/// writer's thread instead of sleeping.
static HANDLED: Mutex<BTreeMap<PathBuf, u64>> = Mutex::new(BTreeMap::new());

/// Counts one line that the writer of `directory` has handled, whether it
/// wrote the line or lost it.
pub(crate) fn count_handled(directory: &Path) {
    let mut handled = HANDLED.lock().unwrap_or_else(PoisonError::into_inner);
    *handled.entry(directory.to_owned()).or_default() += 1;
}

/// Returns how many lines the writer of `directory` has handled.
pub(crate) fn handled(directory: &Path) -> u64 {
    let handled = HANDLED.lock().unwrap_or_else(PoisonError::into_inner);
    handled.get(directory).copied().unwrap_or_default()
}

/// A new directory in the temporary directory. Dropping it removes it.
#[derive(Debug)]
pub(crate) struct Scratch(PathBuf);

impl Scratch {
    /// Returns a new empty directory.
    pub(crate) fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("db-hist-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("the directory is made");
        Self(path)
    }

    /// Returns the directory's path.
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Writes `text` to `path`, as the fixture of a test.
pub(crate) fn write_file(path: impl AsRef<Path>, text: impl AsRef<[u8]>) -> std::io::Result<()> {
    #[expect(
        clippy::disallowed_methods,
        reason = "a test writes its fixture, and a torn or partial fixture only fails that test"
    )]
    std::fs::write(path, text)
}

/// Reads the lines of the history file at `path`, each as JSON.
pub(crate) fn lines_of(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line is JSON"))
        .collect()
}

/// Returns a machine line marked with `at_ms`.
fn machine(at_ms: u64) -> Line {
    Line::Machine(MachineLine {
        v: VERSION,
        at_ms,
        load_1m: Some(1.5),
        pressure: Some(Pressure::Normal),
        running: 1,
        waiting: 0,
        held: None,
        max_jobs: 4,
        cores: Some(14),
        dropped: 0,
    })
}

/// Returns a writer of `directory` that rotates past `limit` bytes, and its log.
fn writer(directory: &Path, limit: u64) -> (Writer, Arc<Mutex<Vec<u8>>>) {
    let written = Arc::new(Mutex::new(Vec::new()));
    let log: Log = written.clone();
    (Writer::new(directory.to_owned(), limit, log), written)
}

/// Returns the lines that `log` holds.
fn logged(log: &Mutex<Vec<u8>>) -> Vec<String> {
    let text = String::from_utf8(log.lock().expect("locks").clone()).expect("UTF-8");
    text.lines().map(str::to_owned).collect()
}

/// Returns a job that runs `command` in `cwd`.
fn job_in(command: &str, cwd: Option<&Path>) -> Job {
    Job {
        label: "tests".to_owned(),
        pid: 7,
        command: command.to_owned(),
        lane: Lane::Normal,
        watched: false,
        cwd: cwd.map(|cwd| cwd.display().to_string()),
        workspace: None,
        short: None,
    }
}

/// Returns `name` under `parent`, made as a directory.
fn directory(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::create_dir_all(&path).expect("the directory is made");
    path
}

/// Returns a main checkout named `name` under `parent`.
fn checkout(parent: &Path, name: &str) -> PathBuf {
    let path = directory(parent, name);
    directory(&path, ".git");
    path
}

/// Returns the warm directories that the tests name.
fn warm_dirs() -> Vec<String> {
    vec!["target".to_owned()]
}

/// Returns the tool list `names`.
fn tools(names: &[&str]) -> Vec<String> {
    names.iter().map(|&name| name.to_owned()).collect()
}

#[test]
fn the_kind_of_a_program_that_is_not_listed_is_its_file_name_only() {
    assert_eq!(kind("./deploy.sh hunterpass", &[]), "deploy.sh");
}

#[test]
fn the_kind_of_a_program_alone_is_its_file_name() {
    assert_eq!(kind("./deploy.sh", &[]), "deploy.sh");
    assert_eq!(kind("make", &[]), "make");
}

#[test]
fn the_kind_of_an_unlisted_tool_leaves_out_its_next_word() {
    assert_eq!(kind("mytool secretword", &tools(&["cargo"])), "mytool");
}

#[test]
fn the_kind_of_a_listed_tool_keeps_its_next_word() {
    assert_eq!(
        kind("mytool secretword", &tools(&["mytool"])),
        "mytool secretword"
    );
}

#[test]
fn the_kind_of_a_listed_tool_keeps_its_subcommands_and_skips_words_that_are_not_plain() {
    assert_eq!(
        kind("cargo +1.99.1 nextest run --locked", &tools(&["cargo"])),
        "cargo nextest run"
    );
}

#[test]
fn the_kind_of_a_listed_tool_stops_at_the_first_word_that_starts_with_a_dash() {
    assert_eq!(kind("tool --password hunter", &tools(&["tool"])), "tool");
    assert_eq!(kind("tool -p hunter", &tools(&["tool"])), "tool");
}

#[test]
fn an_empty_tool_list_gives_the_program_alone() {
    assert_eq!(kind("cargo nextest run", &[]), "cargo");
}

#[test]
fn a_listed_tool_keeps_at_most_two_subcommands_and_matches_by_file_name() {
    assert_eq!(
        kind("/usr/local/bin/cargo nextest run extra", &tools(&["cargo"])),
        "cargo nextest run"
    );
}

#[test]
fn the_kind_of_a_listed_tool_keeps_no_path_environment_variable_capital_or_long_word() {
    let long = "a".repeat(33);
    assert_eq!(
        kind(
            &format!("tool FOO=bar /opt/x some/where Upper {long} sync"),
            &tools(&["tool"])
        ),
        "tool sync"
    );
}

#[test]
fn the_kind_keeps_a_word_of_the_longest_length_it_allows() {
    let longest = "a".repeat(32);
    assert_eq!(
        kind(&format!("tool {longest}"), &tools(&["tool"])),
        format!("tool {longest}")
    );
}

#[test]
fn only_the_program_of_a_kind_may_hold_digits() {
    assert_eq!(
        kind("python3 ab12 run", &tools(&["python3"])),
        "python3 run"
    );
}

#[test]
fn a_program_that_is_not_plain_gives_an_empty_kind() {
    assert_eq!(kind("./Build.sh test", &tools(&["Build.sh"])), "");
    assert_eq!(kind("", &[]), "");
}

#[test]
fn a_leading_env_and_its_assignments_are_skipped_so_the_kind_starts_at_the_program() {
    let cargo = tools(&["cargo"]);
    assert_eq!(kind("env TOKEN=abc A=b cargo test", &cargo), "cargo test");
    assert_eq!(
        kind("/usr/bin/env TOKEN=abc /opt/bin/cargo test", &cargo),
        "cargo test"
    );
}

#[test]
fn a_program_whose_name_only_starts_with_env_is_kept_as_the_program() {
    assert_eq!(kind("envoy run TOKEN=abc", &tools(&["envoy"])), "envoy run");
}

#[test]
fn an_env_with_only_assignments_gives_an_empty_kind() {
    assert_eq!(kind("env TOKEN=abc", &[]), "");
}

#[test]
fn a_program_whose_file_name_holds_a_dot_keeps_it() {
    assert_eq!(
        kind(
            "./scripts/wait-for-stack.sh up",
            &tools(&["wait-for-stack.sh"])
        ),
        "wait-for-stack.sh up"
    );
}

#[test]
fn a_later_word_with_a_dot_and_a_dotted_hidden_program_are_left_out() {
    assert_eq!(kind("tool file.txt run", &tools(&["tool"])), "tool run");
    assert_eq!(kind("/opt/.hidden run", &tools(&[".hidden"])), "");
    assert_eq!(kind("TOKEN=a.b run", &[]), "");
}

#[test]
fn a_job_line_of_a_command_behind_env_holds_no_assignment() {
    let arrival = Arrival::read(
        &job_in("env TOKEN=abc cargo test", None),
        &[],
        &tools(&["cargo"]),
        None,
    );
    let departure = Departure {
        label: Label::new("tests"),
        lane: Lane::Normal,
        ahead: 0,
        threads: Some(1),
        ran: Some(Duration::from_secs(1)),
        exit: Some(Exit::Exited { code: 0 }),
        usage: None,
        tally: None,
    };
    let line = JobLine::new(arrival, departure, super::now_ms());
    let text = serde_json::to_string(&Line::Job(Box::new(line))).expect("serializes");
    assert!(!text.contains("TOKEN"), "{text}");
    assert!(!text.contains("abc"), "{text}");
    assert!(text.contains("\"kind\":\"cargo test\""), "{text}");
}

#[test]
fn options_keep_long_names_without_values_and_exact_single_letters() {
    assert_eq!(
        options("cargo test --workspace --features=tui,other -p name"),
        ["--workspace", "--features", "-p"]
    );
}

#[test]
fn options_leave_out_every_other_form() {
    assert_eq!(
        options("tool -j8 -abc -1 -- --/etc/passwd --=x plain /a/b FOO=--bar"),
        Vec::<String>::new()
    );
}

#[test]
fn options_keep_at_most_32_names() {
    let command = format!("tool {}", vec!["--flag"; 40].join(" "));
    assert_eq!(options(&command).len(), 32);
}

#[test]
fn a_directory_inside_a_main_checkout_names_it_as_repo_and_checkout() {
    let scratch = Scratch::new();
    let root = checkout(scratch.path(), "deadband");
    let inner = directory(&root, "crates/deadband");
    assert_eq!(
        Place::find(&inner, &[]),
        Place {
            repo: Some("deadband".to_owned()),
            checkout: Some("deadband".to_owned()),
            warm: None,
            workspace: None,
        }
    );
}

#[test]
fn a_directory_inside_a_workspace_names_the_workspace_by_its_directory() {
    let scratch = Scratch::new();
    let workspace = directory(scratch.path(), "triage");
    write_file(workspace.join(WORKSPACE_MARKER), "").expect("the marker writes");
    let root = checkout(&workspace, "api");
    let place = Place::find(&directory(&root, "src"), &[]);
    assert_eq!(place.workspace, Some(Label::new("triage")));
    assert_eq!(place.repo.as_deref(), Some("api"));
}

#[test]
fn a_workspace_is_found_outside_git_and_in_the_workspace_directory_itself() {
    let scratch = Scratch::new();
    let workspace = directory(scratch.path(), "notes");
    write_file(workspace.join(WORKSPACE_MARKER), "").expect("the marker writes");
    let inner = directory(&workspace, "drafts");
    assert_eq!(
        Place::find(&inner, &[]).workspace,
        Some(Label::new("notes"))
    );
    assert_eq!(
        Place::find(&workspace, &[]).workspace,
        Some(Label::new("notes"))
    );
    assert_eq!(Place::find(&inner, &[]).repo, None);
}

#[test]
fn a_marker_that_is_a_directory_or_a_symbolic_link_does_not_count_and_the_walk_goes_on_up() {
    let scratch = Scratch::new();
    let outer = directory(scratch.path(), "outer");
    write_file(outer.join(WORKSPACE_MARKER), "").expect("the marker writes");
    let linked = directory(&outer, "linked");
    std::os::unix::fs::symlink(outer.join(WORKSPACE_MARKER), linked.join(WORKSPACE_MARKER))
        .expect("the link is made");
    let folder = directory(&linked, "folder");
    directory(&folder, WORKSPACE_MARKER);
    assert_eq!(
        Place::find(&folder, &[]).workspace,
        Some(Label::new("outer"))
    );
}

#[test]
fn a_directory_outside_any_workspace_has_no_workspace() {
    let scratch = Scratch::new();
    let root = checkout(scratch.path(), "repo");
    assert_eq!(Place::find(&root, &[]).workspace, None);
}

#[test]
fn the_walk_stops_after_64_directories() {
    let scratch = Scratch::new();
    write_file(scratch.path().join(WORKSPACE_MARKER), "").expect("the marker writes");
    let shallow = (0..63).fold(scratch.path().to_owned(), |path, _| path.join("d"));
    std::fs::create_dir_all(&shallow).expect("the directories are made");
    let name = scratch
        .path()
        .file_name()
        .expect("a name")
        .to_string_lossy();
    assert_eq!(
        Place::find(&shallow, &[]).workspace,
        Some(Label::new(&name))
    );
    let deep = shallow.join("d");
    std::fs::create_dir_all(&deep).expect("the directory is made");
    assert_eq!(Place::find(&deep, &[]).workspace, None);
}

#[test]
fn a_workspace_name_loses_its_control_characters_and_is_cut_to_64_bytes() {
    let scratch = Scratch::new();
    let name = format!("a\u{1b}[2J{}", "w".repeat(80));
    let workspace = directory(scratch.path(), &name);
    write_file(workspace.join(WORKSPACE_MARKER), "").expect("the marker writes");
    let shown = Place::find(&workspace, &[]).workspace.expect("a workspace");
    assert!(shown.as_str().starts_with("a [2Jw"), "{shown:?}");
    assert_eq!(shown.as_str().len(), 64);
}

#[test]
fn a_directory_inside_a_worktree_names_its_main_checkout_and_its_own_directory() {
    let scratch = Scratch::new();
    let main = checkout(scratch.path(), "deadband");
    let tree = directory(scratch.path(), "job-history");
    let gitdir = main.join(".git/worktrees/job-history");
    write_file(tree.join(".git"), format!("gitdir: {}\n", gitdir.display()))
        .expect("the file writes");
    assert_eq!(
        Place::find(&directory(&tree, "src"), &[]),
        Place {
            repo: Some("deadband".to_owned()),
            checkout: Some("job-history".to_owned()),
            warm: None,
            workspace: None,
        }
    );
}

#[test]
fn a_git_file_that_names_no_worktree_counts_its_own_directory_as_the_repo() {
    let scratch = Scratch::new();
    let tree = directory(scratch.path(), "module");
    write_file(tree.join(".git"), "gitdir: ../.git/modules/module\n").expect("writes");
    let place = Place::find(&tree, &[]);
    assert_eq!(place.repo.as_deref(), Some("module"));
    assert_eq!(place.checkout.as_deref(), Some("module"));
}

#[test]
fn a_directory_outside_git_names_nothing_and_is_not_warm_or_cold() {
    let scratch = Scratch::new();
    assert_eq!(Place::find(scratch.path(), &warm_dirs()), Place::default());
}

#[test]
fn a_relative_or_missing_directory_gives_an_empty_place() {
    let scratch = Scratch::new();
    assert_eq!(
        Place::find(Path::new("src"), &warm_dirs()),
        Place::default()
    );
    let missing = scratch.path().join("no-such-directory");
    assert_eq!(Place::find(&missing, &warm_dirs()), Place::default());
}

#[test]
fn a_checkout_is_warm_when_a_warm_directory_has_an_entry() {
    let scratch = Scratch::new();
    let root = checkout(scratch.path(), "repo");
    directory(&root, "target/debug");
    assert_eq!(Place::find(&root, &warm_dirs()).warm, Some(true));
}

#[test]
fn a_checkout_is_cold_when_its_warm_directory_is_empty_or_missing() {
    let scratch = Scratch::new();
    let root = checkout(scratch.path(), "repo");
    assert_eq!(Place::find(&root, &warm_dirs()).warm, Some(false));
    directory(&root, "target");
    assert_eq!(Place::find(&root, &warm_dirs()).warm, Some(false));
}

#[test]
fn a_checkout_has_no_warmth_when_the_config_names_no_warm_directory() {
    let scratch = Scratch::new();
    let root = checkout(scratch.path(), "repo");
    directory(&root, "target/debug");
    assert_eq!(Place::find(&root, &[]).warm, None);
}

#[test]
fn an_arrival_reads_the_command_and_the_working_directory() {
    let scratch = Scratch::new();
    let root = checkout(scratch.path(), "repo");
    directory(&root, "target/debug");
    let job = job_in("cargo nextest run --workspace", Some(&root));
    let arrival = Arrival::read(&job, &warm_dirs(), &tools(&["cargo"]), None);
    assert_eq!(arrival.kind, "cargo nextest run");
    assert_eq!(arrival.options, ["--workspace"]);
    assert_eq!(arrival.place.repo.as_deref(), Some("repo"));
    assert_eq!(arrival.place.warm, Some(true));
    assert!(!arrival.seated);
}

#[test]
fn a_seated_arrival_has_no_warmth_because_the_job_has_already_run() {
    let scratch = Scratch::new();
    let root = checkout(scratch.path(), "repo");
    directory(&root, "target/debug");
    let job = job_in("make", Some(&root));
    let asked = super::now_ms();
    let arrival = Arrival::read(&job, &warm_dirs(), &[], Some(5000));
    assert!(arrival.seated);
    assert_eq!(arrival.place.warm, None);
    assert_eq!(arrival.place.repo.as_deref(), Some("repo"));
    assert!(arrival.at_ms + 5000 >= asked, "{arrival:?}");
    assert!(arrival.at_ms < asked, "{arrival:?}");
}

#[test]
fn a_tally_counts_the_load_at_the_start_as_its_first_reading() {
    let mut tally = Tally::started(Some(2.0), 1);
    tally.sample(4.0, Pressure::Normal, Duration::from_secs(1), 1);
    let line = line_of_tally(tally);
    assert_eq!(line.load_at_start, Some(2.0));
    assert_eq!(line.load_mean, Some(3.0));
    assert_eq!(line.load_max, Some(4.0));
    assert_eq!(line.samples, 2);
}

#[test]
fn a_tally_with_no_reading_has_no_load_figures() {
    let line = line_of_tally(Tally::started(None, 1));
    assert_eq!(line.load_at_start, None);
    assert_eq!(line.load_mean, None);
    assert_eq!(line.load_max, None);
    assert_eq!(line.samples, 0);
}

#[test]
fn a_seated_tally_has_no_load_at_its_start_but_counts_later_readings() {
    let mut tally = Tally::seated(1);
    tally.sample(3.0, Pressure::Normal, Duration::from_secs(1), 1);
    let line = line_of_tally(tally);
    assert_eq!(line.load_at_start, None);
    assert_eq!(line.load_mean, Some(3.0));
    assert_eq!(line.samples, 1);
}

#[test]
fn a_tally_counts_the_seconds_of_pressure_above_normal_only() {
    let mut tally = Tally::started(Some(1.0), 1);
    let every = Duration::from_secs(1);
    tally.sample(1.0, Pressure::Warn, every, 1);
    tally.sample(1.0, Pressure::Critical, every, 1);
    tally.sample(1.0, Pressure::Normal, every, 1);
    assert_eq!(line_of_tally(tally).pressure_s, 2);
}

#[test]
fn a_tally_keeps_the_most_jobs_that_ran_at_once() {
    let mut tally = Tally::started(Some(1.0), 2);
    tally.running(4);
    tally.sample(1.0, Pressure::Normal, Duration::from_secs(1), 3);
    assert_eq!(line_of_tally(tally).running_max, 4);
}

/// Returns the line of a job that ran with `tally`.
fn line_of_tally(tally: Tally) -> JobLine {
    let arrival = Arrival::read(&job_in("make", None), &[], &[], None);
    let departure = Departure {
        label: Label::new("tests"),
        lane: Lane::Normal,
        ahead: 0,
        threads: Some(3),
        ran: Some(Duration::from_secs(1)),
        exit: Some(Exit::Exited { code: 0 }),
        usage: None,
        tally: Some(tally),
    };
    JobLine::new(arrival, departure, super::now_ms())
}

#[test]
fn a_job_line_names_each_field_of_the_format() {
    let line = serde_json::to_value(Line::Job(Box::new(line_of_tally(Tally::started(
        Some(1.0),
        1,
    )))))
    .expect("the line serializes");
    let mut names: Vec<&str> = line
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "ahead",
            "checkout",
            "child_cpu_ms",
            "child_peak_rss_bytes",
            "ended_at_ms",
            "exit",
            "kind",
            "label",
            "lane",
            "load_at_start",
            "load_max",
            "load_mean",
            "options",
            "pressure_s",
            "queued_at_ms",
            "repo",
            "running_max",
            "samples",
            "seated",
            "started_at_ms",
            "threads",
            "type",
            "v",
            "warm",
        ]
    );
    assert_eq!(line["type"], "job");
    assert_eq!(line["v"], 1);
}

#[test]
fn a_job_line_holds_no_option_value_environment_variable_or_absolute_path() {
    let scratch = Scratch::new();
    let root = checkout(scratch.path(), "repo");
    let job = job_in(
        "/opt/bin/tool --token=hunter-two SECRET=value /private/path --key hunter",
        Some(&root),
    );
    let arrival = Arrival::read(&job, &[], &[], None);
    let departure = Departure {
        label: Label::new("tests"),
        lane: Lane::Normal,
        ahead: 0,
        threads: Some(1),
        ran: Some(Duration::from_secs(1)),
        exit: Some(Exit::Exited { code: 0 }),
        usage: Some(ChildUsage {
            cpu_ms: 1,
            peak_rss_bytes: 2,
        }),
        tally: None,
    };
    let line = JobLine::new(arrival, departure, super::now_ms());
    let text = serde_json::to_string(&Line::Job(Box::new(line))).expect("serializes");
    for secret in ["hunter", "SECRET", "value", "/opt", "/private", "db-hist"] {
        assert!(!text.contains(secret), "{secret} is in {text}");
    }
    assert!(text.contains("\"kind\":\"tool\""), "{text}");
    assert!(text.contains("\"--token\""), "{text}");
    assert!(text.contains("\"repo\":\"repo\""), "{text}");
}

#[test]
fn a_job_that_left_before_it_ran_has_no_start_threads_or_figures() {
    let arrival = Arrival::read(&job_in("make", None), &[], &[], None);
    let departure = Departure {
        label: Label::new("tests"),
        lane: Lane::Fast,
        ahead: 2,
        threads: None,
        ran: None,
        exit: None,
        usage: None,
        tally: None,
    };
    let line = JobLine::new(arrival, departure, super::now_ms());
    assert_eq!(line.started_at_ms, None);
    assert_eq!(line.threads, None);
    assert_eq!(line.exit, JobEnd::Left);
    assert_eq!(line.ahead, 2);
    assert_eq!(line.load_mean, None);
}

#[test]
fn a_seated_job_counts_as_starting_when_it_asked() {
    let arrival = Arrival::read(&job_in("make", None), &[], &[], Some(1000));
    let at_ms = arrival.at_ms;
    let departure = Departure {
        label: Label::new("tests"),
        lane: Lane::Normal,
        ahead: 0,
        threads: Some(1),
        ran: Some(Duration::from_secs(9)),
        exit: Some(Exit::Signalled { signal: 15 }),
        usage: None,
        tally: None,
    };
    let line = JobLine::new(arrival, departure, super::now_ms());
    assert!(line.seated);
    assert_eq!(line.started_at_ms, Some(at_ms));
    assert_eq!(line.exit, JobEnd::Signalled { signal: 15 });
}

#[test]
fn the_writer_appends_each_line_to_a_private_file_in_a_private_directory() {
    let scratch = Scratch::new();
    let state = scratch.path().join("state/deadband");
    let (mut writer, log) = writer(&state, 1024 * 1024);
    writer.append(&machine(1));
    writer.append(&machine(2));
    let lines = lines_of(&state.join(FILE));
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0]["type"], "machine");
    assert_eq!(lines[0]["at_ms"], 1);
    assert_eq!(lines[1]["at_ms"], 2);
    let mode = |path: &Path| {
        std::fs::metadata(path)
            .expect("the path reads")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&state), 0o700);
    assert_eq!(mode(&state.join(FILE)), 0o600);
    assert!(logged(&log).is_empty());
}

#[test]
fn the_writer_keeps_a_file_at_or_under_the_limit() {
    let scratch = Scratch::new();
    let (mut writer, _) = writer(scratch.path(), 1024 * 1024);
    writer.append(&machine(1));
    assert!(!scratch.path().join(ROTATED_FILE).exists());
    assert_eq!(lines_of(&scratch.path().join(FILE)).len(), 1);
}

#[test]
fn the_writer_rotates_a_file_past_the_limit_and_replaces_the_earlier_one() {
    let scratch = Scratch::new();
    let rotated = scratch.path().join(ROTATED_FILE);
    write_file(&rotated, "old\n").expect("the file writes");
    let (mut writer, _) = writer(scratch.path(), 10);
    writer.append(&machine(1));
    assert_eq!(lines_of(&rotated)[0]["at_ms"], 1);
    assert!(!scratch.path().join(FILE).exists());
    writer.append(&machine(2));
    assert_eq!(lines_of(&rotated)[0]["at_ms"], 2);
    assert_eq!(lines_of(&rotated).len(), 1);
}

#[test]
fn a_rotation_that_fails_is_logged_once_and_the_writer_keeps_appending_and_retries() {
    let scratch = Scratch::new();
    let rotated = scratch.path().join(ROTATED_FILE);
    directory(&rotated, "blocks-the-rename");
    let (mut writer, log) = writer(scratch.path(), 10);
    writer.append(&machine(1));
    writer.append(&machine(2));
    assert_eq!(lines_of(&scratch.path().join(FILE)).len(), 2);
    assert_eq!(logged(&log).len(), 1, "{:?}", logged(&log));
    std::fs::remove_dir_all(&rotated).expect("the directory goes");
    writer.append(&machine(3));
    assert_eq!(lines_of(&rotated).len(), 3);
}

#[test]
fn opening_a_file_with_a_torn_last_line_adds_a_newline_so_later_lines_stay_whole() {
    let scratch = Scratch::new();
    let file = scratch.path().join(FILE);
    write_file(&file, "{\"torn\":").expect("the file writes");
    let (mut writer, _) = writer(scratch.path(), 1024 * 1024);
    writer.append(&machine(1));
    let text = std::fs::read_to_string(&file).expect("the file reads");
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("{\"torn\":"));
    let whole: Value = serde_json::from_str(lines.next().expect("a second line")).expect("JSON");
    assert_eq!(whole["at_ms"], 1);
}

#[test]
fn opening_a_file_whose_last_line_is_whole_adds_nothing() {
    let scratch = Scratch::new();
    let file = scratch.path().join(FILE);
    write_file(&file, "{\"whole\":1}\n").expect("the file writes");
    let (mut writer, _) = writer(scratch.path(), 1024 * 1024);
    writer.append(&machine(1));
    let text = std::fs::read_to_string(&file).expect("the file reads");
    assert_eq!(text.lines().count(), 2, "{text}");
    assert!(!text.contains("\n\n"), "{text}");
}

#[test]
fn a_directory_that_cannot_be_made_is_logged_once_and_the_recovery_is_logged_once() {
    let scratch = Scratch::new();
    let blocker = scratch.path().join("blocker");
    write_file(&blocker, "").expect("the file writes");
    let state = blocker.join("deadband");
    let (mut writer, log) = writer(&state, 1024 * 1024);
    writer.append(&machine(1));
    writer.append(&machine(2));
    assert_eq!(logged(&log).len(), 1, "{:?}", logged(&log));
    std::fs::remove_file(&blocker).expect("the file goes");
    writer.append(&machine(3));
    assert_eq!(logged(&log).len(), 2, "{:?}", logged(&log));
    assert_eq!(lines_of(&state.join(FILE)).len(), 1);
}

#[test]
fn a_writer_that_failed_and_then_finds_the_lock_held_does_not_say_it_writes_again() {
    let scratch = Scratch::new();
    let blocker = scratch.path().join("blocker");
    write_file(&blocker, "").expect("the file writes");
    let state = blocker.join("deadband");
    let (mut second, log) = writer(&state, 1024 * 1024);
    second.append(&machine(1));
    std::fs::remove_file(&blocker).expect("the file goes");
    let (mut first, _) = writer(&state, 1024 * 1024);
    first.append(&machine(2));
    second.append(&machine(3));
    let said = logged(&log);
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[0].contains("could not write"), "{said:?}");
    assert!(said[1].contains("another daemon"), "{said:?}");
}

#[test]
fn a_writer_that_failed_and_then_takes_the_lock_says_it_writes_again() {
    let scratch = Scratch::new();
    let blocker = scratch.path().join("blocker");
    write_file(&blocker, "").expect("the file writes");
    let state = blocker.join("deadband");
    let (mut writer, log) = writer(&state, 1024 * 1024);
    writer.append(&machine(1));
    std::fs::remove_file(&blocker).expect("the file goes");
    writer.append(&machine(2));
    let said = logged(&log);
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[1].contains("written again"), "{said:?}");
}

/// Returns a recorder whose channel holds `capacity` lines, its channel's
/// end, and its log.
fn recorder(
    capacity: usize,
) -> (
    Recorder,
    std::sync::mpsc::Receiver<Line>,
    Arc<Mutex<Vec<u8>>>,
) {
    let written = Arc::new(Mutex::new(Vec::new()));
    let log: Log = written.clone();
    let (recorder, lines) = Recorder::channel(capacity, log);
    (recorder, lines, written)
}

#[test]
fn a_recorder_with_room_hands_on_every_line_and_drops_none() {
    let (recorder, lines, log) = recorder(2);
    recorder.send(machine(1));
    recorder.send(machine(2));
    assert_eq!(recorder.dropped(), 0);
    assert_eq!(lines.try_iter().count(), 2);
    assert!(logged(&log).is_empty());
}

#[test]
fn a_recorder_whose_channel_is_full_drops_the_line_and_counts_it_without_waiting_or_logging() {
    let (recorder, lines, log) = recorder(2);
    for at_ms in 0..3 {
        recorder.send(machine(at_ms));
    }
    assert_eq!(recorder.dropped(), 1);
    assert_eq!(lines.try_iter().count(), 2);
    assert!(logged(&log).is_empty());
}

#[test]
fn a_recorder_whose_writer_is_gone_counts_each_line_as_dropped_and_logs_one_line_once() {
    let (recorder, lines, log) = recorder(2);
    drop(lines);
    recorder.send(machine(1));
    recorder.clone().send(machine(2));
    assert_eq!(recorder.dropped(), 2);
    let lines = logged(&log);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("stopped"), "{lines:?}");
}

#[test]
fn a_second_writer_on_the_same_directory_records_nothing_and_logs_one_line() {
    let scratch = Scratch::new();
    let (mut first, first_log) = writer(scratch.path(), 1024 * 1024);
    first.append(&machine(1));
    let (mut second, second_log) = writer(scratch.path(), 10);
    second.append(&machine(2));
    second.append(&machine(3));
    first.append(&machine(4));
    let lines = lines_of(&scratch.path().join(FILE));
    let at: Vec<&Value> = lines.iter().map(|line| &line["at_ms"]).collect();
    assert_eq!(at, [1, 4]);
    assert!(!scratch.path().join(ROTATED_FILE).exists());
    let said = logged(&second_log);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("another daemon"), "{said:?}");
    assert!(logged(&first_log).is_empty());
}

#[test]
fn a_writer_records_once_the_writer_that_held_the_directory_is_gone() {
    let scratch = Scratch::new();
    let (mut first, _) = writer(scratch.path(), 1024 * 1024);
    first.append(&machine(1));
    drop(first);
    let (mut second, log) = writer(scratch.path(), 1024 * 1024);
    second.append(&machine(2));
    assert_eq!(lines_of(&scratch.path().join(FILE)).len(), 2);
    assert!(logged(&log).is_empty());
}

#[test]
fn a_fresh_writer_on_a_directory_that_holds_only_the_rotated_file_starts_a_new_file() {
    let scratch = Scratch::new();
    let rotated = scratch.path().join(ROTATED_FILE);
    write_file(&rotated, "{\"old\":1}\n").expect("the file writes");
    let (mut writer, log) = writer(scratch.path(), 1024 * 1024);
    writer.append(&machine(1));
    assert_eq!(lines_of(&scratch.path().join(FILE))[0]["at_ms"], 1);
    assert_eq!(lines_of(&rotated), [serde_json::json!({"old": 1})]);
    assert!(logged(&log).is_empty());
}
