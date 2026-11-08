//! The `deadband` binary, which runs the command that the deadband library
//! holds against the machine it runs on.

use std::process::ExitCode;

/// Runs the command and returns its exit code, so destructors run before
/// the process ends (BASE-7).
fn main() -> ExitCode {
    deadband::cli::run(&deadband::probe::System)
}
