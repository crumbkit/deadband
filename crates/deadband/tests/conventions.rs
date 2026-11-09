//! The convention tests of the `deadband` command, which hold its command
//! tree, tool record and failure codes to crumbkit's conventions with
//! crumbkit-test.
//!
//! They live in an integration test because the recovery check runs the
//! built binary, whose path cargo sets only for integration tests.

use clap::CommandFactory as _;
use crumbkit_test::{Allow, Check, Examples, Recoveries, Rule, Suite, Unprovoked};
use deadband::cli::{Cli, TOOL};
use deadband::probe::{MACHINE_UNREADABLE, MACHINE_UNSUPPORTED};

/// deadband's opt-outs from crumbkit's conventions, which every check reads.
const ALLOW: &[Allow] = &[
    Allow::new(
        Rule::ShellExitCode,
        "run",
        "run passes on its job's exit status, and gives 126 and 127 their env and timeout meanings for a program that cannot start or is not found",
    ),
    Allow::root_verbs(
        &["status", "run", "daemon"],
        "People type `deadband status`, `deadband run` and `deadband daemon` as one verb, and deadband has no resource to put them under.",
    ),
];

/// deadband's built binary and the directory of its golden files.
const EXAMPLES: Examples<'_> = Examples::new(env!("CARGO_BIN_EXE_deadband"), "tests/golden");

/// deadband's declared codes, which no command line provokes on demand.
const UNPROVOKED: &[Unprovoked<'_>] = &[
    Unprovoked::new(
        &MACHINE_UNREADABLE,
        "Only a failed call to the kernel provokes it, and no command line makes the kernel fail.",
    ),
    Unprovoked::new(
        &MACHINE_UNSUPPORTED,
        "Only a platform other than macOS provokes it, so whether `deadband status` provokes it depends on the machine the test runs on.",
    ),
];

/// The settings of deadband's recovery check.
const RECOVERIES: Recoveries<'_> =
    Recoveries::new(EXAMPLES, &[&[&MACHINE_UNREADABLE, &MACHINE_UNSUPPORTED]])
        .unprovoked(UNPROVOKED);

/// Everything the checks read, which `conventions!` expands to one test for
/// each check.
const SUITE: Suite<'_> = Suite::new(|| (Cli::command(), TOOL))
    .allow(ALLOW)
    .examples(EXAMPLES)
    .recoveries(RECOVERIES)
    .skip(
        Check::PathWeights,
        "The help paths have no weight budget yet; a later change measures them and sets one.",
    )
    .skip(
        Check::Examples,
        "The `run` example would start a build and the `daemon` example does not exit, so none of the three examples can run as they stand.",
    )
    .skip(
        Check::CommandReference,
        "The check runs `deadband commands list`, which deadband does not mount.",
    )
    .skip(
        Check::Twins,
        "The twin pairs of deadband's codes are not written yet; a later change writes them.",
    );

crumbkit_test::conventions!(SUITE);
