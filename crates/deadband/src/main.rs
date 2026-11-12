//! The `deadband` binary, which runs the command that the deadband library
//! holds against the machine it runs on.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;

use deadband::cli::{LaunchAgent, Machine};

/// Reads the environment once (BASE-6), runs the command and returns its
/// exit code, so destructors run before the process ends (BASE-7).
fn main() -> ExitCode {
    // A shell's completion request ends the run here, before the run reads
    // anything else. deadband parses against `Cli::command()`, so completion
    // offers what parsing accepts.
    if let Some(code) = crumbkit::complete(<deadband::cli::Cli as clap::CommandFactory>::command) {
        return code;
    }
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
        service: launch_agent(),
    })
}

/// Returns deadband's launchd agent: the home directory, the binary's path
/// as invoked, and the user's id. It reads the environment, the working
/// directory and the process's executable path once.
#[cfg(target_os = "macos")]
fn launch_agent() -> Option<LaunchAgent> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the entry point reads the working directory once, to make a relative executable path absolute"
    )]
    let cwd = std::env::current_dir().ok();
    let binary = std::env::current_exe()
        .ok()
        .map(|exe| deadband::cli::service::plist::binary_path(&exe, cwd.as_deref()));
    Some(LaunchAgent {
        home: crumbkit::files::home(),
        binary,
        uid: rustix::process::getuid().as_raw(),
        launchd: Arc::new(deadband::job::launchd::System),
    })
}

/// Returns `None`, because deadband installs its service only with launchd.
#[cfg(not(target_os = "macos"))]
const fn launch_agent() -> Option<LaunchAgent> {
    None
}
