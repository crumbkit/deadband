//! Tests of the rule that names a job by its workspace and its label.

use super::{name, shows_label};
use crate::protocol::{Label, Short};

/// Returns the name of a job in `workspace`, labelled `label`, whose short
/// command is `short`.
fn named(workspace: Option<&str>, label: &str, short: Option<&str>) -> String {
    name(
        workspace.map(Label::new).as_ref(),
        &Label::new(label),
        short.map(Short::new).as_ref(),
    )
}

#[test]
fn a_label_that_equals_the_workspace_is_left_out() {
    assert!(!shows_label(
        Some("reading-list"),
        "reading-list",
        Some("cargo build")
    ));
    assert_eq!(
        named(Some("reading-list"), "reading-list", Some("cargo build")),
        "reading-list"
    );
}

#[test]
fn a_label_that_differs_from_the_workspace_and_the_command_shows_after_the_workspace() {
    assert!(shows_label(
        Some("triage"),
        "api-tests",
        Some("cargo nextest run")
    ));
    assert_eq!(
        named(Some("triage"), "api-tests", Some("cargo nextest run")),
        "triage · api-tests"
    );
}

#[test]
fn a_label_that_equals_the_first_word_of_the_short_command_is_left_out() {
    assert!(!shows_label(
        Some("board-sync"),
        "cargo",
        Some("cargo test")
    ));
    assert_eq!(
        named(Some("board-sync"), "cargo", Some("cargo test")),
        "board-sync"
    );
}

#[test]
fn a_label_that_only_starts_with_the_first_word_shows() {
    assert!(shows_label(Some("triage"), "car", Some("cargo test")));
}

#[test]
fn with_no_workspace_the_name_is_the_label_even_when_the_job_column_leaves_it_out() {
    assert!(!shows_label(None, "tool", Some("tool")));
    assert_eq!(named(None, "tool", Some("tool")), "tool");
    assert!(shows_label(None, "crumbkit-docs", Some("cargo doc")));
    assert_eq!(
        named(None, "crumbkit-docs", Some("cargo doc")),
        "crumbkit-docs"
    );
}

#[test]
fn with_no_short_command_the_label_shows() {
    assert!(shows_label(Some("triage"), "script", Some("")));
    assert!(shows_label(None, "build", None));
    assert_eq!(named(Some("triage"), "script", Some("")), "triage · script");
    assert_eq!(named(None, "build", None), "build");
}

#[test]
fn a_label_that_equals_the_workspace_is_named_once_even_with_no_short_command() {
    assert_eq!(named(Some("triage"), "triage", Some("")), "triage");
}
