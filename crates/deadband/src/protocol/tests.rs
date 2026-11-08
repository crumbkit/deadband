//! Tests of the messages and their JSON lines.

use std::io::{BufReader, ErrorKind};

use super::{Job, Jobs, MAX_LINE, Reply, Request, read_line, write_line};

/// A job that the tests send.
fn job() -> Job {
    Job {
        label: "tests".to_owned(),
        pid: 42,
        command: "cargo nextest run".to_owned(),
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
        "{\"ask\":{\"label\":\"tests\",\"pid\":42,\"command\":\"cargo nextest run\"}}\n"
    );
    assert_eq!(line(&Request::Status), "\"status\"\n");
    assert_eq!(
        line(&Request::Done { exit_code: Some(3) }),
        "{\"done\":{\"exit_code\":3}}\n"
    );
}

#[test]
fn each_reply_is_one_json_line() {
    assert_eq!(line(&Reply::Granted), "\"granted\"\n");
    assert_eq!(
        line(&Reply::Queued { ahead: 2 }),
        "{\"queued\":{\"ahead\":2}}\n"
    );
    let jobs = Reply::Jobs(Jobs {
        max_jobs: 1,
        running: vec![job()],
        waiting: Vec::new(),
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
