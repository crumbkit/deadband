//! Tests of choosing a job's lane from the words of the priority file.

use std::path::Path;

use super::lane;
use crate::admit::Lane;

/// The lane of a job labelled `label` in `cwd`, with `/home/me` as home.
fn of(words: &str, label: &str, cwd: &str) -> Lane {
    lane(words, label, Some(Path::new(cwd)), Some(Path::new("/home/me")))
}

#[test]
fn a_job_runs_in_the_normal_lane_when_no_word_matches() {
    assert_eq!(of("urgent\n/work/hot\n", "tests", "/work/cold"), Lane::Normal);
}

#[test]
fn a_job_whose_label_is_a_word_runs_in_the_fast_lane() {
    assert_eq!(of("urgent\n", "urgent", "/work/cold"), Lane::Fast);
}

#[test]
fn a_job_whose_directory_has_a_word_as_one_of_its_parts_runs_in_the_fast_lane() {
    assert_eq!(of("hot\n", "tests", "/work/hot/crate"), Lane::Fast);
    assert_eq!(of("hot\n", "tests", "/work/hotter"), Lane::Normal);
}

#[test]
fn a_job_at_or_under_a_listed_path_runs_in_the_fast_lane() {
    assert_eq!(of("/work/hot/\n", "tests", "/work/hot"), Lane::Fast);
    assert_eq!(of("/work/hot\n", "tests", "/work/hot/crate"), Lane::Fast);
    assert_eq!(of("/work/hot\n", "tests", "/work/hotter"), Lane::Normal);
}

#[test]
fn a_path_that_starts_with_a_tilde_is_under_home() {
    assert_eq!(of("~/hot\n", "tests", "/home/me/hot/a"), Lane::Fast);
    assert_eq!(of("~/hot\n", "tests", "/home/you/hot/a"), Lane::Normal);
}

#[test]
fn comments_blank_lines_and_the_exclusive_word_match_nothing() {
    let words = "# urgent\n\n   \nexclusive\ncold # a comment\n";
    assert_eq!(of(words, "urgent", "/work"), Lane::Normal);
    assert_eq!(of(words, "exclusive", "/work"), Lane::Normal);
    assert_eq!(of(words, "cold", "/work"), Lane::Fast);
}
