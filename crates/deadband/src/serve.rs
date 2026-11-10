//! The daemon as one call that another program can make.
//!
//! [`run_daemon`] does what `deadband daemon` does before it prints a
//! failure. A program that embeds deadband builds a [`Machine`] as the
//! `deadband` binary's `main.rs` does: its probe, the socket path from
//! [`socket::path`](crate::socket::path), a log, the number of cores and
//! the [`Environment`](crate::environment::Environment) it read once.

use std::num::NonZeroUsize;

use crate::cli::Machine;
use crate::daemon::{self, Settings, Unbound, note};
use crate::probe::Probe;
use crate::reclaim::{self, CONFIG_FILE};

/// Reads the daemon's config file in the configuration directory. A file
/// that cannot be read turns reclaiming off, and the log says why in one
/// line. The log names the reclaimers the file lists.
fn reclaim_config<P>(machine: &Machine<P>) -> reclaim::Config {
    let Some(path) = machine
        .environment
        .config
        .as_ref()
        .map(|config| config.join(CONFIG_FILE))
    else {
        return reclaim::Config::default();
    };
    let config = reclaim::Config::load(&path).unwrap_or_else(|error| {
        note(
            &machine.log,
            format_args!(
                "deadband could not read {}: {error}, so the daemon runs no reclaimer and no on_stuck command",
                path.display()
            ),
        );
        reclaim::Config::default()
    });
    if !config.reclaimers.is_empty() {
        let names: Vec<_> = config
            .reclaimers
            .iter()
            .map(|each| each.name.as_str())
            .collect();
        note(
            &machine.log,
            format_args!(
                "the daemon asks {} to reclaim when memory or disk runs short",
                names.join(", ")
            ),
        );
    }
    config
}

/// Runs the daemon: listens on the machine's socket and admits jobs until
/// accepting a connection fails with an error that retrying cannot fix,
/// then returns why it stopped. A failure that can pass, such as too many
/// open files, is logged and retried.
///
/// `max_jobs` is the most jobs that run at once; `None` takes a quarter of
/// the logical cores, rounded up, and at least 1. The call binds the
/// socket, writes the "listens on" line to the machine's log, reads the
/// reclaim config from the configuration directory, and then serves until
/// it fails. A socket that another daemon answers on, or that cannot be
/// made, returns at once with the reason as an [`Unbound`].
///
/// The call has two effects on the whole process. It raises the soft limit
/// on open files. While it binds, it sets the umask to clear every group
/// and other permission bit, so the socket and its lock file are private to
/// the user, and then restores the earlier umask; a file that another
/// thread of the process creates during that moment gets the stricter mode.
///
/// The daemon is no longer sound once `run_daemon` returns. Its connection
/// threads keep running and can still grant a waiting client from the last
/// machine reading, because the sampler has stopped, and a second daemon
/// can bind the socket. A program must end its process at that point, as
/// the `deadband` binary does.
pub fn run_daemon<P: Probe + Clone + Send + 'static>(
    machine: &Machine<P>,
    max_jobs: Option<NonZeroUsize>,
) -> Unbound {
    let max_jobs = max_jobs.unwrap_or_else(|| daemon::default_max_jobs(machine.cores));
    daemon::raise_descriptor_limit();
    match daemon::bind(&machine.socket) {
        Ok(bound) => {
            note(
                &machine.log,
                format_args!(
                    "the daemon listens on {} and runs at most {max_jobs} jobs at once",
                    machine.socket.display()
                ),
            );
            let mut settings = Settings::new(
                max_jobs,
                machine.cores,
                machine.environment.home.clone(),
                machine.environment.priority_file(),
            );
            settings.reclaim = reclaim_config(machine);
            settings.history = machine.environment.history_directory();
            let error = daemon::serve(
                &bound.listener,
                machine.probe.clone(),
                settings,
                &machine.log,
            );
            Unbound::Socket {
                path: machine.socket.clone(),
                error,
            }
        }
        Err(unbound) => unbound,
    }
}
