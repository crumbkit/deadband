//! The recovery check of the `deadband` command, which runs the built
//! binary and so lives in an integration test, where cargo sets the
//! binary's path.

use clap::CommandFactory;
use crumbkit_test::{Examples, Recoveries, Unprovoked};
use deadband::cli::{Cli, GUIDE};
use deadband::probe::{MACHINE_UNREADABLE, MACHINE_UNSUPPORTED};

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

#[test]
fn every_recovery_leads_to_a_command_that_runs() {
    let recoveries = Recoveries::new(EXAMPLES, &[&[&MACHINE_UNREADABLE, &MACHINE_UNSUPPORTED]])
        .unprovoked(UNPROVOKED);
    crumbkit_test::check_recoveries(&Cli::command(), &GUIDE, &recoveries, "tests/golden");
}
