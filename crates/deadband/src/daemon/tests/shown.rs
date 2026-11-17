//! Tests of the workspace and the short command that the daemon gives each
//! job when it arrives, which `status` and `top` show.

use std::path::{Path, PathBuf};

use super::timeline::{ended, row, timeline, timeline_until};
use super::{Client, Dial, granted, job, jobs, start_with, until_running};
use crate::admit::Threads;
use crate::history::WORKSPACE_MARKER;
use crate::history::tests::{Scratch, write_file};
use crate::probe::Pressure;
use crate::protocol::{Ago, Job, Label, Request, Short};

/// Returns a checkout named `checkout` inside a workspace named
/// `workspace` under `parent`, whose directory holds the marker.
fn checkout_in_workspace(parent: &Path, workspace: &str, checkout: &str) -> PathBuf {
    let root = parent.join(workspace);
    let tree = root.join(checkout);
    std::fs::create_dir_all(tree.join(".git")).expect("the directory is made");
    write_file(root.join(WORKSPACE_MARKER), "").expect("the marker writes");
    tree
}

/// Returns `job` labelled `label` that runs `command` in `cwd`.
fn job_in(label: &str, command: &str, cwd: Option<&Path>) -> Job {
    Job {
        command: command.to_owned(),
        cwd: cwd.map(|cwd| cwd.display().to_string()),
        ..job(label)
    }
}

/// Starts a daemon of one job that records no history and whose
/// `kind_tools` is `tools`.
fn daemon(tools: &[&str]) -> crate::testing::TestSocket {
    let tools: Vec<String> = tools.iter().map(|&tool| tool.to_owned()).collect();
    let (path, _log) = start_with(4, &Dial::new(Pressure::Normal), move |settings| {
        settings.history = None;
        settings.reclaim.kind_tools = tools;
    });
    path
}

#[test]
fn a_job_inside_a_workspace_shows_its_workspace_and_short_command_on_a_daemon_without_history() {
    let scratch = Scratch::new();
    let tree = checkout_in_workspace(scratch.path(), "triage", "api");
    let path = daemon(&["cargo"]);
    let mut client = Client::connect(&path);
    let cwd = tree.join("crates");
    std::fs::create_dir_all(&cwd).expect("the directory is made");
    client.send(&Request::Ask(job_in(
        "tests",
        "cargo test --workspace",
        Some(&cwd),
    )));
    assert_eq!(client.reply(), granted(4));
    let listed = jobs(&path);
    assert_eq!(listed.running[0].workspace, Some(Label::new("triage")));
    assert_eq!(listed.running[0].short, Some(Short::new("cargo test")));
    let shown = timeline(&path);
    assert_eq!(row(&shown, "tests").workspace, Some(Label::new("triage")));
    assert_eq!(row(&shown, "tests").short, Some(Short::new("cargo test")));
}

#[test]
fn a_job_outside_any_workspace_has_no_workspace() {
    let scratch = Scratch::new();
    let tree = scratch.path().join("repo");
    std::fs::create_dir_all(tree.join(".git")).expect("the directory is made");
    let path = daemon(&[]);
    let mut client = Client::connect(&path);
    client.send(&Request::Ask(job_in("docs", "cargo doc", Some(&tree))));
    assert_eq!(client.reply(), granted(4));
    let listed = jobs(&path);
    assert_eq!(listed.running[0].workspace, None);
    assert_eq!(listed.running[0].short, Some(Short::new("cargo")));
}

#[test]
fn a_workspace_that_the_client_names_is_kept_over_the_marker() {
    let scratch = Scratch::new();
    let tree = checkout_in_workspace(scratch.path(), "triage", "api");
    let path = daemon(&[]);
    let mut client = Client::connect(&path);
    client.send(&Request::Ask(Job {
        workspace: Some(Label::new("reading-list")),
        ..job_in("reading-list", "make", Some(&tree))
    }));
    assert_eq!(client.reply(), granted(4));
    assert_eq!(
        jobs(&path).running[0].workspace,
        Some(Label::new("reading-list"))
    );
}

#[test]
fn a_job_from_an_older_client_with_no_directory_gets_a_short_command_and_no_workspace() {
    let path = daemon(&["cargo"]);
    let mut client = Client::connect(&path);
    client.send(&Request::Ask(job_in(
        "lint",
        "cargo clippy --all-targets",
        None,
    )));
    assert_eq!(client.reply(), granted(4));
    let listed = jobs(&path);
    assert_eq!(listed.running[0].workspace, None);
    assert_eq!(listed.running[0].short, Some(Short::new("cargo clippy")));
}

#[test]
fn the_daemon_replaces_a_short_command_that_a_client_sends() {
    let path = daemon(&[]);
    let mut client = Client::connect(&path);
    client.send(&Request::Ask(Job {
        short: Some(Short::new("tool s3cr3t")),
        ..job_in("deploy", "tool --token s3cr3t deploy", None)
    }));
    assert_eq!(client.reply(), granted(4));
    assert_eq!(jobs(&path).running[0].short, Some(Short::new("tool")));
}

#[test]
fn a_command_that_keeps_no_word_gets_an_empty_short_command() {
    let path = daemon(&[]);
    let mut client = Client::connect(&path);
    client.send(&Request::Ask(job_in("script", "./Deploy.sh", None)));
    assert_eq!(client.reply(), granted(4));
    assert_eq!(jobs(&path).running[0].short, Some(Short::new("")));
}

#[test]
fn an_ended_job_keeps_its_workspace_and_short_command_on_the_timeline() {
    let scratch = Scratch::new();
    let tree = checkout_in_workspace(scratch.path(), "triage", "api");
    let path = daemon(&[]);
    let mut client = Client::connect(&path);
    client.send(&Request::Ask(job_in("suite", "make check", Some(&tree))));
    assert_eq!(client.reply(), granted(4));
    drop(client);
    let shown = timeline_until(&path, |shown| ended(shown, "suite"));
    assert_eq!(row(&shown, "suite").workspace, Some(Label::new("triage")));
    assert_eq!(row(&shown, "suite").short, Some(Short::new("make")));
}

#[test]
fn a_seated_job_gets_its_workspace_and_short_command() {
    let scratch = Scratch::new();
    let tree = checkout_in_workspace(scratch.path(), "triage", "api");
    let path = daemon(&[]);
    let mut client = Client::connect(&path);
    client.send(&Request::Seat {
        job: job_in("build", "make all", Some(&tree)),
        threads: Threads::new(1),
        ran: Ago::from_millis(1000),
    });
    let listed = until_running(&path, 1);
    assert_eq!(listed.running[0].workspace, Some(Label::new("triage")));
    assert_eq!(listed.running[0].short, Some(Short::new("make")));
}
