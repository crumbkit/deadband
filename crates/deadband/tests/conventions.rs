//! The convention tests of the `deadband` command, which hold its command
//! tree, tool record and failure codes to crumbkit's conventions with
//! crumbkit-test.
//!
//! They live in an integration test because the recovery check runs the
//! built binary, whose path cargo sets only for integration tests.

// The golden files list `deadband top`, so these checks run only with the
// default features. A build without them leaves the checks out, and a second
// set of golden files would replace that limit.
#![cfg(feature = "tui")]

use clap::CommandFactory as _;
use crumbkit_test::{Allow, Check, Examples, Recoveries, Rule, Suite, Unprovoked};
use deadband::cli::{Cli, TOOL};
use deadband::probe::{MACHINE_UNREADABLE, MACHINE_UNSUPPORTED};
use deadband::tui::{TOP_NOT_A_TERMINAL, TOP_TERMINAL_FAILED};

/// deadband's opt-outs from crumbkit's conventions, which every check reads.
const ALLOW: &[Allow] = &[
    Allow::new(
        Rule::ShellExitCode,
        "run",
        "run exits with 126 when the job's program is found but cannot start and with 127 when it is not found, the meanings that env and timeout give these codes",
    ),
    Allow::root_verbs(
        &["status", "run", "daemon", "top"],
        "People type `deadband status`, `deadband run`, `deadband daemon` and `deadband top` as one verb, and deadband has no resource to put them under.",
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
    Unprovoked::new(
        &TOP_NOT_A_TERMINAL,
        "Its recovery is `deadband status`, which succeeds only on macOS, so following it depends on the machine the test runs on. The refusal and the recovery it names are asserted in `cli/tests.rs` and `tests/run.rs`.",
    ),
    Unprovoked::new(
        &TOP_TERMINAL_FAILED,
        "Only a terminal that fails while the view draws provokes it, and no command line makes a terminal fail.",
    ),
];

/// The settings of deadband's recovery check.
const RECOVERIES: Recoveries<'_> = Recoveries::new(
    EXAMPLES,
    &[
        &[&MACHINE_UNREADABLE, &MACHINE_UNSUPPORTED],
        &[&TOP_NOT_A_TERMINAL, &TOP_TERMINAL_FAILED],
    ],
)
.unprovoked(UNPROVOKED);

/// Everything the checks read, which `conventions!` expands to one test for
/// each check.
const SUITE: Suite<'_> = Suite::new(|| (Cli::command(), TOOL))
    .allow(ALLOW)
    .verbs(&["install", "uninstall", "check", "print"])
    .examples(EXAMPLES)
    .recoveries(RECOVERIES)
    .skip(
        Check::PathWeights,
        "The help paths have no weight budget yet; a later change measures them and sets one.",
    )
    .skip(
        Check::Examples,
        "The `status` examples print live values, the `run` example would start a build and the `daemon` example does not exit, so no example has fixed output.",
    )
    .skip(
        Check::Twins,
        "The twin pairs of deadband's codes are not written yet; a later change writes them.",
    );

crumbkit_test::conventions!(SUITE);
