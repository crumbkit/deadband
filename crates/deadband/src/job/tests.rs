//! Tests of starting a job and reading how it ended.

use std::ffi::OsString;
use std::io::ErrorKind;

use super::{exit_code, run};

fn words(command: &[&str]) -> Vec<OsString> {
    command.iter().map(OsString::from).collect()
}

#[test]
fn run_returns_the_exit_code_of_the_job() {
    let status = run(&words(&["sh", "-c", "exit 3"]), &[]).expect("sh starts");
    assert_eq!(exit_code(status), 3);
}

#[test]
fn run_passes_each_argument_as_a_separate_value() {
    let status = run(&words(&["test", "a b", "=", "a b"]), &[]).expect("test starts");
    assert_eq!(exit_code(status), 0);
}

#[test]
fn a_job_that_a_signal_ends_exits_with_128_plus_the_signal() {
    let status = run(&words(&["sh", "-c", "kill -TERM $$"]), &[]).expect("sh starts");
    assert_eq!(exit_code(status), 143);
}

#[test]
fn run_fails_with_not_found_for_a_program_that_does_not_exist() {
    let error = run(&words(&["deadband-test-no-such-program"]), &[]).expect_err("nothing starts");
    assert_eq!(error.kind(), ErrorKind::NotFound);
}

#[test]
fn run_refuses_an_empty_command() {
    let error = run(&[], &[]).expect_err("nothing starts");
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
}

#[test]
fn run_adds_the_variables_it_is_given_to_the_jobs_environment() {
    let variables = [("DEADBAND_TEST_VARIABLE", "7".to_owned())];
    let status = run(
        &words(&["sh", "-c", "test \"$DEADBAND_TEST_VARIABLE\" = 7"]),
        &variables,
    )
    .expect("sh starts");
    assert_eq!(exit_code(status), 0);
}

mod busiest {
    use std::collections::BTreeSet;
    use std::time::Duration;

    use crate::job::busiest::{Census, Failed, parse};
    use crate::protocol::OutsideJobs;

    const LIST: &[u8] = b"470.0   123 /usr/libexec/syspolicyd\n 90.5  4567 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome\n  0.0     1 /sbin/launchd\n";

    fn sh(script: &str) -> Vec<String> {
        vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()]
    }

    #[test]
    fn the_busiest_process_outside_the_jobs_is_named() {
        assert_eq!(
            parse(LIST, &BTreeSet::new()),
            Ok(OutsideJobs::Busy {
                name: "syspolicyd".to_owned(),
                cpu_percent: 470
            })
        );
    }

    #[test]
    fn a_process_in_a_job_tree_is_left_out() {
        assert_eq!(
            parse(LIST, &BTreeSet::from([123])),
            Ok(OutsideJobs::Busy {
                name: "Google Chrome".to_owned(),
                cpu_percent: 90
            })
        );
    }

    #[test]
    fn a_process_under_half_a_core_is_not_named() {
        let list = b" 49.9   123 /usr/libexec/syspolicyd\n";
        assert_eq!(parse(list, &BTreeSet::new()), Ok(OutsideJobs::Quiet));
    }

    #[test]
    fn a_process_at_half_a_core_is_named() {
        let list = b" 50.0   123 /usr/libexec/syspolicyd\n";
        assert_eq!(
            parse(list, &BTreeSet::new()),
            Ok(OutsideJobs::Busy {
                name: "syspolicyd".to_owned(),
                cpu_percent: 50
            })
        );
    }

    #[test]
    fn a_decimal_comma_reads_as_a_decimal_point() {
        let list = b" 75,5   123 /usr/libexec/syspolicyd\n";
        assert!(matches!(
            parse(list, &BTreeSet::new()),
            Ok(OutsideJobs::Busy {
                cpu_percent: 75,
                ..
            })
        ));
    }

    #[test]
    fn output_that_is_not_a_process_list_is_refused() {
        assert!(parse(b"ps: something went wrong\n", &BTreeSet::new()).is_err());
        assert!(parse(b"", &BTreeSet::new()).is_err());
    }

    #[test]
    fn a_control_character_in_a_name_becomes_a_space() {
        let list = b"99.0 123 /tmp/bad\x1b[31mname\n";
        assert_eq!(
            parse(list, &BTreeSet::new()),
            Ok(OutsideJobs::Busy {
                name: "bad [31mname".to_owned(),
                cpu_percent: 99
            })
        );
    }

    #[test]
    fn a_census_reads_the_commands_output() {
        let read = Census::default().read(
            &sh("echo '88.0 77 /bin/busy'"),
            Duration::from_secs(10),
            &BTreeSet::new(),
        );
        assert!(matches!(
            read,
            Ok(OutsideJobs::Busy {
                cpu_percent: 88,
                ..
            })
        ));
    }

    #[test]
    fn a_failing_command_names_no_process() {
        let read = Census::default().read(&sh("exit 3"), Duration::from_secs(10), &BTreeSet::new());
        assert!(matches!(read, Err(Failed::Exit(Some(3)))));
    }

    #[test]
    fn a_command_that_cannot_start_names_no_process() {
        let read = Census::default().read(
            &["/nonexistent/ps".to_owned()],
            Duration::from_secs(10),
            &BTreeSet::new(),
        );
        assert!(matches!(read, Err(Failed::Start(_))));
    }

    #[test]
    fn unparsable_output_names_no_process() {
        let read = Census::default().read(
            &sh("echo nonsense"),
            Duration::from_secs(10),
            &BTreeSet::new(),
        );
        assert!(matches!(read, Err(Failed::Unparsable(_))));
    }

    #[test]
    fn a_hung_command_times_out_and_blocks_a_second_run_until_it_exits() {
        let census = Census::default();
        let hung = sh("sleep 1; echo '88.0 77 /bin/busy'");
        let first = census.read(&hung, Duration::from_millis(50), &BTreeSet::new());
        assert!(matches!(first, Err(Failed::TimedOut)));
        let second = census.read(&hung, Duration::from_millis(50), &BTreeSet::new());
        assert!(matches!(second, Err(Failed::Running)));
    }
}

mod on_stuck {
    use std::time::Duration;

    use crate::job::on_stuck::{Ended, run};

    fn sh(script: &str) -> Vec<String> {
        vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()]
    }

    #[test]
    fn the_command_receives_its_input_on_standard_input() {
        let ended = run(
            &sh("test \"$(cat)\" = hello"),
            b"hello".to_vec(),
            Duration::from_secs(10),
            || panic!("the command runs within its deadline"),
        );
        assert_eq!(ended.expect("sh starts"), Ended::Exited(Some(0)));
    }

    #[test]
    fn a_failing_command_reports_its_exit_code() {
        let ended = run(&sh("exit 4"), Vec::new(), Duration::from_secs(10), || {
            panic!("the command runs within its deadline")
        });
        assert_eq!(ended.expect("sh starts"), Ended::Exited(Some(4)));
    }

    #[test]
    fn a_command_past_its_deadline_is_waited_for_and_never_signalled() {
        let mut timed_out = false;
        let ended = run(
            &sh("sleep 0.3; exit 5"),
            Vec::new(),
            Duration::from_millis(20),
            || timed_out = true,
        );
        assert!(timed_out);
        assert_eq!(ended.expect("sh starts"), Ended::TimedOut);
    }

    #[test]
    fn an_empty_command_is_refused() {
        let ended = run(&[], Vec::new(), Duration::from_secs(1), || {});
        assert_eq!(
            ended.expect_err("nothing can start").kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
}
