//! Admission for a program that starts a job: the steps `deadband run` takes
//! before it starts one, as one call that another program can make.
//!
//! [`admit`] decides the job's lane, asks the daemon whether the job may
//! start, waits while the daemon holds it, and returns an [`Admitted`] that
//! gives the job's parallelism variables. When the daemon cannot be asked,
//! the job starts at once with the default share of the cores, and the line
//! passed to `notify` says why. A daemon that is missing, gone or slow to
//! answer does not block a job: `admit` waits at most 2 seconds for the
//! daemon's first answer. That limit does not cover sending the request, which
//! has no deadline, so a daemon that has stopped reading can block `admit`
//! when the command text is longer than the socket's send buffer (about
//! 8 KiB on macOS). Once the daemon has queued a job, `admit` waits for the
//! grant with no deadline, and the wait cannot be cancelled.
//!
//! The job's `pid` in the daemon's listing is the calling process's id
//! (`std::process::id()`). A program that holds the lease for a child
//! process reports its own pid, not the child's.
//!
//! `admit` writes nothing to a log or to standard error. Each line it has
//! to report goes to the `notify` callback, and the caller chooses where
//! the line goes.

use std::io;
use std::num::NonZeroUsize;
use std::path::Path;

use crate::admit::{Hold, Lane, Limits};
use crate::client::{self, ANSWER_WITHIN, Admission, Lease};
use crate::daemon;
use crate::environment::Environment;
use crate::priority;
use crate::protocol::Job;
use crate::throttle::{self, NORMAL_NICE};

#[cfg(test)]
mod tests;

/// What a program asks `admit` for: one job to start.
#[derive(Debug, Clone, Copy)]
pub struct Asked<'a> {
    /// The path of the daemon's socket.
    pub socket: &'a Path,
    /// What deadband read from the environment. The priority file is
    /// matched against `environment.cwd`, so a long-lived program that
    /// starts a job in another directory must pass an `Environment` whose
    /// `cwd` is the job's directory. `environment.preset` records the
    /// parallelism variables the program had when it read its
    /// environment, so a job that is given its own value for one of them
    /// needs that variable in `preset`, or `admit` overwrites it.
    pub environment: &'a Environment,
    /// The number of logical cores, or `None` when it is unknown.
    pub cores: Option<NonZeroUsize>,
    /// The label that `deadband status` shows for the job.
    pub label: &'a str,
    /// The command text that `deadband status` shows for the job.
    pub command: &'a str,
    /// The lane the job waits in, or `None` to take the fast lane when the
    /// priority file matches the job and the normal lane otherwise.
    pub lane: Option<Lane>,
}

/// A job that may start, with the lease that counts it as running.
///
/// The job counts as running in the daemon until [`Admitted::end`] is
/// called or this value is dropped, which closes the connection.
#[derive(Debug)]
#[must_use = "dropping an Admitted frees the job's place in the daemon"]
pub struct Admitted {
    /// The daemon's grant, or `None` when the job starts without one.
    lease: Option<Lease>,
    /// The lane the job waits in, or `None` when `admit` asked nothing.
    lane: Option<Lane>,
    /// The variables the job starts with.
    variables: Vec<(&'static str, String)>,
}

impl Admitted {
    /// Returns the lane the job was admitted in, or `None` when the job
    /// runs inside a granted job or bypasses admission, so `admit` asked
    /// nothing.
    #[must_use]
    pub const fn lane(&self) -> Option<Lane> {
        self.lane
    }

    /// Returns the variables to start the job with: the threads in each
    /// parallelism variable that the caller did not set, and
    /// `DEADBAND_HELD` when the daemon granted the job. The list is empty
    /// when `admit` asked nothing.
    #[must_use]
    pub fn variables(&self) -> &[(&'static str, String)] {
        &self.variables
    }

    /// Lowers the calling process to nice 5 when the job is in the normal
    /// lane, and reports in one line to `notify` when that fails. A process
    /// that already runs at a higher nice value stays where it is. A job
    /// started afterwards inherits the priority. It does nothing for a job
    /// in the fast lane or one that asked nothing.
    ///
    /// Lowering is the caller's choice, and it lowers the calling process
    /// itself, for the rest of its life: a program that lowers its own
    /// priority should do so in a process of its own, as `deadband run` does.
    pub fn lower_priority(&self, notify: &mut impl FnMut(&str)) {
        if self.lane != Some(Lane::Normal) {
            return;
        }
        if let Err(error) = throttle::lower_own_priority(NORMAL_NICE) {
            notify(&format!(
                "deadband could not lower the job's priority: {error}, so the job runs at its caller's priority"
            ));
        }
    }

    /// Tells the daemon that the job ended with `exit_code`, which is
    /// `None` when a signal ended it or it did not start. It does nothing
    /// when the job holds no lease.
    pub fn end(self, exit_code: Option<i32>) {
        if let Some(lease) = self.lease {
            lease.end(exit_code);
        }
    }
}

/// Returns the line that says why a job waits behind `ahead` jobs.
fn waits(ahead: usize, held: Option<Hold>) -> String {
    let why = held.map_or_else(
        || "for headroom".to_owned(),
        |hold| format!("because {hold}"),
    );
    match ahead {
        0 => format!("the job waits {why}, and no other job waits ahead of it"),
        1 => format!("the job waits {why}, and 1 job waits ahead of it"),
        n => format!("the job waits {why}, and {n} jobs wait ahead of it"),
    }
}

/// Returns the lane of the job: the one the caller named, or the fast lane
/// when the priority file matches the job. A priority file that cannot be
/// read matches nothing, and `notify` receives why.
fn lane_of(asked: &Asked<'_>, notify: &mut impl FnMut(&str)) -> Lane {
    if let Some(lane) = asked.lane {
        return lane;
    }
    let environment = asked.environment;
    let Some(file) = environment.priority_file() else {
        return Lane::Normal;
    };
    let words = match std::fs::read_to_string(&file) {
        Ok(words) => words,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Lane::Normal,
        Err(error) => {
            notify(&format!(
                "deadband could not read {}: {error}, so the job waits in the normal lane",
                file.display()
            ));
            return Lane::Normal;
        }
    };
    priority::lane(
        &words,
        asked.label,
        environment.cwd.as_deref(),
        environment.home.as_deref(),
    )
}

/// Asks the daemon to admit the job `asked` describes, and waits until it
/// does.
///
/// When the daemon cannot be asked, the job starts at once and `notify`
/// receives why in one line. A job that starts without a grant still gets
/// the default share of the cores: the cores divided by the default most
/// jobs, at least 1 thread, and one more thread in the fast lane. On a
/// machine with 4 or fewer cores the default allows one job at once, so the
/// share is every core. A job inside a granted job, or one that asks to
/// bypass admission, starts at once: `admit` asks nothing, reports nothing
/// and returns no lease, lane or variables.
pub fn admit(asked: &Asked<'_>, notify: &mut impl FnMut(&str)) -> Admitted {
    let environment = asked.environment;
    if environment.held || environment.bypass {
        return Admitted {
            lease: None,
            lane: None,
            variables: Vec::new(),
        };
    }
    let lane = lane_of(asked, notify);
    let job = Job {
        label: asked.label.to_owned(),
        pid: std::process::id(),
        command: asked.command.to_owned(),
        lane,
    };
    let queued = |ahead: usize, held: Option<Hold>| notify(&waits(ahead, held));
    let lease = match client::ask(asked.socket, &job, ANSWER_WITHIN, queued) {
        Admission::Granted(lease) => Some(lease),
        Admission::Open(unasked) => {
            notify(&format!("{unasked}, so the job starts now"));
            None
        }
    };
    let variables = match &lease {
        Some(lease) => environment.job_variables(lease.jobs(), true),
        None => {
            let fallback = Limits {
                max_jobs: daemon::default_max_jobs(asked.cores),
                cores: asked.cores,
                reserve_lanes: false,
            };
            environment.job_variables(fallback.parallelism(lane), false)
        }
    };
    Admitted {
        lease,
        lane: Some(lane),
        variables,
    }
}
