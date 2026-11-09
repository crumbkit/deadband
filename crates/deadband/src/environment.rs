//! The snapshot of the environment that `deadband run` reads, and the
//! variables it gives a granted job (BASE-28).
//!
//! The entry point reads the environment and passes a reader to
//! [`Environment::read`], so this module reads no variable itself (BASE-6).

use std::ffi::OsString;
use std::path::PathBuf;

use crate::socket::Variable;

#[cfg(test)]
mod tests;

/// The variable that marks a job started inside a granted job.
pub const HELD: &str = "DEADBAND_HELD";

/// The variable that asks `deadband run` to skip admission.
pub const BYPASS: &str = "DEADBAND_BYPASS";

/// The variables that tell build and test tools how many threads to use.
/// `run` sets each to the job's threads unless the caller set it.
pub const PARALLELISM: [&str; 5] = [
    "CARGO_BUILD_JOBS",
    "NEXTEST_TEST_THREADS",
    "RUST_TEST_THREADS",
    "CMAKE_BUILD_PARALLEL_LEVEL",
    "MAKEFLAGS",
];

/// The variable whose value takes the form `-j<threads>`.
const MAKEFLAGS: &str = "MAKEFLAGS";

/// The name of the priority file in deadband's configuration directory.
pub const PRIORITY_FILE: &str = "priority";

/// Every environment variable that [`Environment::read`] reads, in the
/// order it reads them.
pub const VARIABLES: &[Variable] = &[
    Variable {
        name: HELD,
        purpose: "marks a job that a granted job started, so it runs at once without asking",
        form: "any value but empty or 0; `deadband run` sets it to 1 for the job it runs",
        absent: "deadband run asks the daemon",
    },
    Variable {
        name: BYPASS,
        purpose: "skips admission, so the job runs at once without asking",
        form: "any value but empty or 0",
        absent: "deadband run asks the daemon",
    },
    Variable {
        name: "HOME",
        purpose: "the home directory, whose disk the daemon reads, and which holds the configuration directory",
        form: "a directory path",
        absent: "the disk holds no job",
    },
    Variable {
        name: "XDG_CONFIG_HOME",
        purpose: "the configuration directory, which holds `deadband/priority` and `deadband/config.toml`",
        form: "a directory path",
        absent: "deadband uses ~/.config",
    },
    Variable {
        name: "CARGO_BUILD_JOBS",
        purpose: "the threads of a cargo build, which `run` leaves as the caller set it",
        form: "a number",
        absent: "deadband run sets it to the job's threads",
    },
    Variable {
        name: "NEXTEST_TEST_THREADS",
        purpose: "the threads of a nextest run, which `run` leaves as the caller set it",
        form: "a number",
        absent: "deadband run sets it to the job's threads",
    },
    Variable {
        name: "RUST_TEST_THREADS",
        purpose: "the threads of a libtest run, which `run` leaves as the caller set it",
        form: "a number",
        absent: "deadband run sets it to the job's threads",
    },
    Variable {
        name: "CMAKE_BUILD_PARALLEL_LEVEL",
        purpose: "the threads of a CMake build, which `run` leaves as the caller set it",
        form: "a number",
        absent: "deadband run sets it to the job's threads",
    },
    Variable {
        name: MAKEFLAGS,
        purpose: "the flags of make, which `run` leaves as the caller set them",
        form: "make's flags",
        absent: "deadband run sets it to -j and the job's threads",
    },
];

/// What `deadband run` and the daemon read from the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    /// `DEADBAND_HELD` is set to anything but 0: the job runs inside a granted job.
    pub held: bool,
    /// `DEADBAND_BYPASS` is set to anything but 0: the job skips admission.
    pub bypass: bool,
    /// The home directory.
    pub home: Option<PathBuf>,
    /// deadband's configuration directory.
    pub config: Option<PathBuf>,
    /// The working directory of `deadband run`.
    pub cwd: Option<PathBuf>,
    /// The parallelism variables that the caller set.
    pub preset: Vec<&'static str>,
}

impl Environment {
    /// Reads each variable of [`VARIABLES`] with `read`, which returns
    /// `None` for an absent variable, and keeps `cwd` as the working
    /// directory. An empty variable counts as absent.
    pub fn read(read: impl Fn(&str) -> Option<OsString>, cwd: Option<PathBuf>) -> Self {
        let set = |name: &str| read(name).filter(|value| !value.is_empty());
        let switched_on = |name: &str| set(name).is_some_and(|value| value != "0");
        let held = switched_on(HELD);
        let bypass = switched_on(BYPASS);
        let home = set("HOME").map(PathBuf::from);
        let config = set("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(".config")))
            .map(|directory| directory.join(crate::NAME));
        let preset = PARALLELISM
            .into_iter()
            .filter(|name| set(name).is_some())
            .collect();
        Self {
            held,
            bypass,
            home,
            config,
            cwd,
            preset,
        }
    }

    /// Returns the path of the priority file, or `None` when the
    /// configuration directory is unknown.
    #[must_use]
    pub fn priority_file(&self) -> Option<PathBuf> {
        self.config
            .as_ref()
            .map(|config| config.join(PRIORITY_FILE))
    }

    /// Returns the variables that a job with `jobs` threads gets: each
    /// parallelism variable that the caller did not set, and
    /// `DEADBAND_HELD` when the daemon `held` a place for the job.
    #[must_use]
    pub fn job_variables(&self, jobs: usize, held: bool) -> Vec<(&'static str, String)> {
        let mut variables = Vec::new();
        if held {
            variables.push((HELD, "1".to_owned()));
        }
        for name in PARALLELISM {
            if self.preset.contains(&name) {
                continue;
            }
            let value = if name == MAKEFLAGS {
                format!("-j{jobs}")
            } else {
                jobs.to_string()
            };
            variables.push((name, value));
        }
        variables
    }
}
