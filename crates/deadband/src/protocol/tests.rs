//! Tests of the messages and their JSON lines.

use std::io::{BufReader, ErrorKind};

use super::{Job, Jobs, MAX_LINE, Reply, Request, read_line, write_line};
use crate::admit::{Hold, Lane};

/// A job that the tests send.
fn job() -> Job {
    Job {
        label: "tests".to_owned(),
        pid: 42,
        command: "cargo nextest run".to_owned(),
        lane: Lane::Fast,
    }
}

/// Returns `message` as the line that `write_line` writes.
fn line<T: serde::Serialize>(message: &T) -> String {
    let mut bytes = Vec::new();
    write_line(&mut bytes, message).expect("a write to a vector succeeds");
    String::from_utf8(bytes).expect("the line is UTF-8")
}

#[test]
fn each_request_is_one_json_line() {
    assert_eq!(
        line(&Request::Ask(job())),
        "{\"ask\":{\"label\":\"tests\",\"pid\":42,\"command\":\"cargo nextest run\",\"lane\":\"fast\"}}\n"
    );
    assert_eq!(line(&Request::Status), "\"status\"\n");
    assert_eq!(
        line(&Request::Done { exit_code: Some(3) }),
        "{\"done\":{\"exit_code\":3}}\n"
    );
}

#[test]
fn each_reply_is_one_json_line() {
    assert_eq!(
        line(&Reply::Granted { jobs: 3 }),
        "{\"granted\":{\"jobs\":3}}\n"
    );
    assert_eq!(
        line(&Reply::Queued {
            ahead: 2,
            held: Some(Hold::Full { max_jobs: 1 })
        }),
        "{\"queued\":{\"ahead\":2,\"held\":{\"hold\":\"full\",\"max_jobs\":1}}}\n"
    );
    let jobs = Reply::Jobs(Jobs {
        max_jobs: 1,
        running: vec![job()],
        waiting: Vec::new(),
        held: None,
        reclaim: None,
    });
    assert!(line(&jobs).starts_with("{\"jobs\":{\"max_jobs\":1,\"running\":[{"));
}

#[test]
fn read_line_reads_back_each_line_that_write_line_wrote() {
    let text = line(&Request::Ask(job())) + &line(&Request::Done { exit_code: None });
    let mut reader = BufReader::new(text.as_bytes());
    assert_eq!(
        read_line::<Request>(&mut reader).expect("the line reads"),
        Some(Request::Ask(job()))
    );
    assert_eq!(
        read_line::<Request>(&mut reader).expect("the line reads"),
        Some(Request::Done { exit_code: None })
    );
    assert_eq!(
        read_line::<Request>(&mut reader).expect("the end reads"),
        None
    );
}

#[test]
fn read_line_refuses_a_line_that_is_not_a_message() {
    let mut reader = BufReader::new("{\"fly\":1}\n".as_bytes());
    let error = read_line::<Request>(&mut reader).expect_err("the line is refused");
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn read_line_refuses_a_line_longer_than_the_limit() {
    let long = format!(
        "\"{}\"\n",
        "a".repeat(usize::try_from(MAX_LINE).expect("fits"))
    );
    let mut reader = BufReader::new(long.as_bytes());
    let error = read_line::<Request>(&mut reader).expect_err("the line is refused");
    assert_eq!(error.kind(), ErrorKind::InvalidData);
}

#[test]
fn read_line_accepts_a_line_of_exactly_the_limit() {
    let length = usize::try_from(MAX_LINE).expect("fits");
    // Two quotes and the newline make up the rest of the line.
    let text = "a".repeat(length - 3);
    let line = format!("\"{text}\"\n");
    assert_eq!(line.len(), length);
    let mut reader = BufReader::new(line.as_bytes());
    assert_eq!(
        read_line::<String>(&mut reader).expect("the line reads"),
        Some(text)
    );
}

#[test]
fn a_job_without_a_lane_waits_in_the_normal_lane() {
    let line = "{\"ask\":{\"label\":\"a\",\"pid\":1,\"command\":\"a\"}}\n";
    let mut reader = BufReader::new(line.as_bytes());
    let Some(Request::Ask(job)) = read_line::<Request>(&mut reader).expect("the line reads") else {
        panic!("the line is an ask");
    };
    assert_eq!(job.lane, Lane::Normal);
}

#[test]
fn a_jobs_reply_from_a_daemon_that_sends_no_hold_reads_as_no_hold() {
    let text = "{\"jobs\":{\"max_jobs\":1,\"running\":[],\"waiting\":[]}}\n";
    let reply = read_line::<Reply>(&mut BufReader::new(text.as_bytes()))
        .expect("the line reads")
        .expect("a reply");
    assert!(
        matches!(reply, Reply::Jobs(Jobs { held: None, .. })),
        "{reply:?}"
    );
}
