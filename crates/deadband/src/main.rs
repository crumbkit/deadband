//! The `deadband` binary, which runs the command that the deadband library
//! holds against the machine it runs on.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;

use deadband::cli::Machine;

/// Reads the environment once (BASE-6), runs the command and returns its
/// exit code, so destructors run before the process ends (BASE-7).
fn main() -> ExitCode {
    #[expect(
        clippy::disallowed_methods,
        reason = "the entry point reads the environment once, for the variables that deadband::socket::VARIABLES lists"
    )]
    let socket = deadband::socket::path(|name| std::env::var_os(name));
    #[expect(
        clippy::disallowed_methods,
        reason = "the entry point reads the environment and the working directory once, for the variables that deadband::environment::VARIABLES lists"
    )]
    let environment = deadband::environment::Environment::read(
        |name| std::env::var_os(name),
        std::env::current_dir().ok(),
    );
    #[expect(
        clippy::disallowed_methods,
        reason = "the daemon's log and the line that says why `run` starts a job at once go to standard error, which crumbkit's Sink does not lend out"
    )]
    let stderr = std::io::stderr();
    deadband::cli::run(&Machine {
        probe: deadband::probe::System,
        socket,
        log: Arc::new(Mutex::new(stderr)),
        cores: thread::available_parallelism().ok(),
        environment,
    })
}
