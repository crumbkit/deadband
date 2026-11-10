//! Tests of the messages and their JSON lines.

use std::io::{BufReader, ErrorKind};

use serde::Deserialize;

use super::{
    Ago, Exit, Job, JobRecord, JobState, Jobs, LABEL_BYTES, Label, MAX_LINE, OutsideJobs, Reply,
    Request, StuckPeriod, Timeline, read_line, write_line,
};
use crate::admit::{Hold, Lane};

/// A job that the tests send.
fn job() -> Job {
    Job {
        label: "tests".to_owned(),
        pid: 42,
        command: "cargo nextest run".to_owned(),
        lane: Lane::Fast,
        watched: false,
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
        line(&Request::Done {
            exit_code: Some(3),
            signal: None
        }),
        "{\"done\":{\"exit_code\":3}}\n"
    );
}

#[test]
fn a_seat_is_one_json_line_that_reads_back_as_itself() {
    let seat = Request::Seat {
        job: job(),
        jobs: 3,
        ran_ms: 1500,
    };
    let text = line(&seat);
    assert_eq!(
        text,
        "{\"seat\":{\"job\":{\"label\":\"tests\",\"pid\":42,\"command\":\"cargo nextest run\",\"lane\":\"fast\"},\"jobs\":3,\"ran_ms\":1500}}\n"
    );
    let mut reader = BufReader::new(text.as_bytes());
    assert_eq!(
        read_line::<Request>(&mut reader).expect("the line reads"),
        Some(seat)
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
        stuck: Vec::new(),
        busiest: None,
        unreadable: false,
    });
    assert!(line(&jobs).starts_with("{\"jobs\":{\"max_jobs\":1,\"running\":[{"));
}

#[test]
fn read_line_reads_back_each_line_that_write_line_wrote() {
    let text = line(&Request::Ask(job()))
        + &line(&Request::Done {
            exit_code: None,
            signal: None,
        });
    let mut reader = BufReader::new(text.as_bytes());
    assert_eq!(
        read_line::<Request>(&mut reader).expect("the line reads"),
        Some(Request::Ask(job()))
    );
    assert_eq!(
        read_line::<Request>(&mut reader).expect("the line reads"),
        Some(Request::Done {
            exit_code: None,
            signal: None
        })
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

#[test]
fn a_timeline_request_is_one_json_line() {
    assert_eq!(line(&Request::Timeline), "\"timeline\"\n");
}

#[test]
fn each_line_that_an_older_client_sends_still_reads() {
    let lines: [(&str, Request); 3] = [
        (
            "{\"ask\":{\"label\":\"tests\",\"pid\":42,\"command\":\"cargo nextest run\",\"lane\":\"fast\"}}\n",
            Request::Ask(job()),
        ),
        ("\"status\"\n", Request::Status),
        (
            "{\"done\":{\"exit_code\":3}}\n",
            Request::Done {
                exit_code: Some(3),
                signal: None,
            },
        ),
    ];
    for (text, request) in lines {
        let read = read_line::<Request>(&mut BufReader::new(text.as_bytes())).expect("reads");
        assert_eq!(read, Some(request), "{text}");
        assert_eq!(line(&read), text);
    }
}

#[test]
fn a_done_line_that_carries_a_signal_names_it() {
    assert_eq!(
        line(&Request::Done {
            exit_code: None,
            signal: Some(9)
        }),
        "{\"done\":{\"exit_code\":null,\"signal\":9}}\n"
    );
}

/// The request as a daemon that knows no `signal` field reads it.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OlderRequest {
    Done { exit_code: Option<i32> },
}

#[test]
fn an_older_daemon_reads_a_done_line_that_carries_a_signal() {
    let text = line(&Request::Done {
        exit_code: None,
        signal: Some(9),
    });
    let read: Option<OlderRequest> =
        read_line(&mut BufReader::new(text.as_bytes())).expect("the line reads");
    assert_eq!(read, Some(OlderRequest::Done { exit_code: None }));
}

/// A row of every kind that the timeline tests send.
fn timeline() -> Timeline {
    let record = |label: &str, queued, state| JobRecord {
        label: Label::new(label),
        lane: Lane::Normal,
        queued: Ago::from_millis(queued),
        state,
        stuck: Vec::new(),
    };
    Timeline {
        up: Ago::from_millis(9),
        jobs: vec![
            record(
                "a",
                8,
                JobState::Running {
                    started: Ago::from_millis(7),
                },
            ),
            record("b", 6, JobState::Waiting),
            record(
                "c",
                5,
                JobState::Finished {
                    started: Ago::from_millis(4),
                    ended: Ago::from_millis(3),
                    exit: Exit::Signalled { signal: 9 },
                },
            ),
            record(
                "d",
                2,
                JobState::Left {
                    ended: Ago::from_millis(1),
                },
            ),
        ],
        hidden: 0,
        held: Some(Hold::Full { max_jobs: 1 }),
        busiest: None,
        unreadable: false,
    }
}

#[test]
fn a_timeline_reply_is_one_json_line_that_reads_back() {
    let reply = Reply::Timeline(timeline());
    let text = line(&reply);
    assert_eq!(
        text,
        "{\"timeline\":{\"up\":9,\"jobs\":[\
         {\"label\":\"a\",\"lane\":\"normal\",\"queued\":8,\"state\":{\"running\":{\"started\":7}}},\
         {\"label\":\"b\",\"lane\":\"normal\",\"queued\":6,\"state\":\"waiting\"},\
         {\"label\":\"c\",\"lane\":\"normal\",\"queued\":5,\"state\":{\"finished\":{\"started\":4,\"ended\":3,\"exit\":{\"signalled\":{\"signal\":9}}}}},\
         {\"label\":\"d\",\"lane\":\"normal\",\"queued\":2,\"state\":{\"left\":{\"ended\":1}}}\
         ],\"hidden\":0,\"held\":{\"hold\":\"full\",\"max_jobs\":1}}}\n"
    );
    assert_eq!(
        read_line::<Reply>(&mut BufReader::new(text.as_bytes())).expect("reads"),
        Some(reply)
    );
}

#[test]
fn the_largest_timeline_reply_fits_in_one_line() {
    let row = JobRecord {
        // A quote takes two bytes of JSON, so this label is the longest to
        // write.
        label: Label::new(&"\"".repeat(LABEL_BYTES)),
        lane: Lane::Normal,
        queued: Ago::from_millis(u64::MAX),
        state: JobState::Finished {
            started: Ago::from_millis(u64::MAX),
            ended: Ago::from_millis(u64::MAX),
            exit: Exit::Signalled { signal: i32::MIN },
        },
        stuck: vec![
            StuckPeriod {
                since: Ago::from_millis(u64::MAX),
                until: Some(Ago::from_millis(u64::MAX)),
            };
            4
        ],
    };
    let reply = Reply::Timeline(Timeline {
        up: Ago::from_millis(u64::MAX),
        jobs: vec![row; 100],
        hidden: usize::MAX,
        held: Some(Hold::Load {
            projected: f64::MAX,
            cores: usize::MAX,
        }),
        busiest: Some(OutsideJobs::Busy {
            name: "\"".repeat(LABEL_BYTES),
            cpu_percent: u32::MAX,
        }),
        unreadable: true,
    });
    let length = line(&reply).len();
    assert!(
        u64::try_from(length).expect("fits") <= MAX_LINE,
        "the reply takes {length} bytes"
    );
    assert_eq!(
        read_line::<Reply>(&mut BufReader::new(line(&reply).as_bytes())).expect("reads"),
        Some(reply)
    );
}

#[test]
fn a_label_replaces_each_control_character_with_a_space() {
    assert_eq!(Label::new("a\u{1b}[2Jb\n\u{9b}c").as_str(), "a [2Jb  c");
}

#[test]
fn a_label_without_control_characters_stays_as_it_is() {
    assert_eq!(Label::new("board-cli test").as_str(), "board-cli test");
}

#[test]
fn a_label_that_a_daemon_sends_loses_its_escapes() {
    let label: Label = serde_json::from_str("\"a\\u001b[2J\"").expect("reads");
    assert_eq!(label.as_str(), "a [2J");
}

#[test]
fn a_long_label_is_cut_on_a_character_boundary() {
    let text = format!("{}é", "a".repeat(LABEL_BYTES - 1));
    assert_eq!(Label::new(&text).as_str(), "a".repeat(LABEL_BYTES - 1));
}

#[test]
fn a_label_of_exactly_the_limit_keeps_every_byte() {
    let text = "a".repeat(LABEL_BYTES);
    assert_eq!(Label::new(&text).as_str(), text);
}

#[test]
fn an_exit_code_reports_an_exit_and_a_signal_reports_a_signal() {
    assert_eq!(Exit::reported(Some(2), None), Exit::Exited { code: 2 });
    assert_eq!(Exit::reported(None, Some(9)), Exit::Signalled { signal: 9 });
}

#[test]
fn a_done_with_neither_code_nor_signal_reports_lost() {
    assert_eq!(Exit::reported(None, None), Exit::Lost);
}

#[test]
fn an_age_counts_the_whole_milliseconds_between_two_instants() {
    let start = std::time::Instant::now();
    let later = start + std::time::Duration::from_millis(1500);
    assert_eq!(Ago::between(start, later), Ago::from_millis(1500));
    assert_eq!(Ago::between(later, start), Ago::from_millis(0));
}

#[test]
fn a_job_from_an_older_client_is_not_watched() {
    let line = r#"{"label":"a","pid":1,"command":"b"}"#;
    let job: Job = serde_json::from_str(line).expect("the job reads");
    assert!(!job.watched);
}

#[test]
fn a_watched_job_says_so_on_the_wire() {
    let line = serde_json::to_string(&Job {
        watched: true,
        ..job()
    })
    .expect("a job serializes");
    assert!(line.contains("\"watched\":true"), "{line}");
}

#[test]
fn a_stuck_notice_round_trips() {
    let reply = Reply::Stuck {
        quiet: Ago::from_millis(300_000),
        again: false,
        busiest: Some(OutsideJobs::Quiet),
    };
    let text = serde_json::to_string(&reply).expect("a reply serializes");
    assert_eq!(serde_json::from_str::<Reply>(&text).expect("reads"), reply);
}

#[test]
fn jobs_from_an_older_daemon_have_no_stuck_job() {
    let line = r#"{"max_jobs":1,"running":[],"waiting":[]}"#;
    let jobs: Jobs = serde_json::from_str(line).expect("the jobs read");
    assert!(jobs.stuck.is_empty() && jobs.busiest.is_none() && !jobs.unreadable);
}

#[test]
fn a_timeline_from_an_older_daemon_has_no_stuck_periods() {
    let line = r#"{"up":5,"jobs":[{"label":"a","lane":"normal","queued":1,"state":"waiting"}],"hidden":0,"held":null}"#;
    let timeline: Timeline = serde_json::from_str(line).expect("the timeline reads");
    assert!(timeline.jobs[0].stuck.is_empty());
    assert!(timeline.busiest.is_none() && !timeline.unreadable);
}
