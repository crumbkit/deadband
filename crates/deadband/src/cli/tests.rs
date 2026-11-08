//! Tests of `deadband status`, run against a fake probe, and of the
//! command's crumbkit conventions.

use clap::CommandFactory;
use crumbkit::{Code, Sink};

use super::{Cli, GUIDE, run_to};
use crate::probe::{
    MACHINE_UNREADABLE, MACHINE_UNSUPPORTED, Pressure, Probe, Sample, Swap, Unreadable,
};

/// A probe that returns one fixed sample.
struct Fixed;

impl Probe for Fixed {
    fn sample(&self) -> Result<Sample, Unreadable> {
        Ok(Sample {
            load_1m: 4.25,
            memory_pressure: Pressure::Warn,
            swap: Swap {
                used_bytes: 11_605 * 1_048_576,
                total_bytes: 13_312 * 1_048_576,
            },
        })
    }
}

/// A probe that cannot read the machine.
struct Broken;

impl Probe for Broken {
    fn sample(&self) -> Result<Sample, Unreadable> {
        Err(Unreadable::Load)
    }
}

/// Runs `deadband` with `args` against `probe`, and returns the exit code,
/// standard output and standard error.
fn run(args: &[&str], probe: &impl Probe) -> (u8, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_to(
        &mut Sink::new(&mut out, &mut err),
        probe,
        std::iter::once("deadband").chain(args.iter().copied()),
    );
    (
        code,
        String::from_utf8(out).expect("the output is UTF-8"),
        String::from_utf8(err).expect("the error output is UTF-8"),
    )
}

#[test]
fn status_prints_one_line_each_for_load_memory_pressure_and_swap() {
    let (code, out, err) = run(&["status"], &Fixed);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "load: 4.25 (1-minute average)\n\
         memory pressure: warn\n\
         swap: 11605 MiB used of 13312 MiB\n"
    );
}

#[test]
fn status_with_json_prints_the_sample_in_the_status_document() {
    let (code, out, err) = run(&["status", "--json"], &Fixed);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(r#""schema":"deadband/status/v1""#), "{out}");
    assert!(out.contains(r#""load_1m":4.25"#), "{out}");
    assert!(out.contains(r#""memory_pressure":"warn""#), "{out}");
    assert!(out.contains(r#""swap_used_bytes":12168724480"#), "{out}");
    assert!(out.contains(r#""swap_total_bytes":13958643712"#), "{out}");
}

#[test]
fn status_says_in_one_line_why_the_probe_failed() {
    let (code, out, err) = run(&["status"], &Broken);
    assert_eq!(code, 1);
    assert_eq!(out, "");
    assert!(
        err.contains("deadband could not read the 1-minute load average"),
        "{err}"
    );
}

#[test]
fn the_guide_gives_status_an_example_its_effects_and_its_document() {
    crumbkit_test::check_guide(&Cli::command(), &GUIDE, &[]);
    crumbkit_test::check_effects(&Cli::command(), &GUIDE, &[]);
    crumbkit_test::check_document_ids(&Cli::command(), &GUIDE, &[]);
}

#[test]
fn every_failure_code_is_distinct_and_printable() {
    let codes: &[&Code] = &[&MACHINE_UNREADABLE, &MACHINE_UNSUPPORTED];
    crumbkit_test::check_codes(&GUIDE, &[codes]);
}
