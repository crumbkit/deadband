//! The `deadband` command line.

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use crumbkit::help::{Effects, Example, Guide, Leaf};
use crumbkit::{Document, Prose, Sink, ViewFlags};

use crate::probe::{Probe, Sample, Unreadable};

#[cfg(test)]
mod tests;

/// Keep a busy machine usable while many heavy jobs compete for it.
#[derive(Debug, Parser)]
#[command(name = "deadband")]
pub struct Cli {
    /// The command to run.
    #[command(subcommand)]
    pub command: Command,

    /// The flags that choose how the result prints.
    #[command(flatten)]
    pub view: ViewFlags,
}

/// The commands of `deadband`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the machine's load, memory pressure and swap.
    Status,
}

/// The document that `deadband status` prints.
pub const STATUS: Document<Prose<Sample>> = Document::new(
    "deadband",
    "status",
    1,
    "One sample of the machine's 1-minute load average, memory pressure and swap.",
);

/// deadband's guide: the examples, effects and document of each command.
pub const GUIDE: Guide = Guide::new().leaves(&[Leaf::new(&["status"])
    .examples(&[
        Example::new(&[], "Print the machine's load, memory pressure and swap"),
        Example::new(&["--json"], "Print the same sample as a JSON document"),
    ])
    .effects(Effects::read_only())
    .document(STATUS)]);

/// Parses the process's command line, runs the command against `probe` and
/// prints the result to standard output and standard error.
pub fn run(probe: &impl Probe) -> ExitCode {
    let (cli, output) = match crumbkit::parse::<Cli>(Cli::command(), &GUIDE) {
        Ok(parsed) => parsed,
        Err(stop) => return stop.exit_code(),
    };
    match cli.command {
        Command::Status => crumbkit::finish(output, STATUS, status(probe)),
    }
}

/// Parses `args`, runs the command against `probe` and prints the result to
/// `sink`. Returns the exit code.
pub fn run_to<O: Write, W: Write, T: Into<OsString>>(
    sink: &mut Sink<O, W>,
    probe: &impl Probe,
    args: impl IntoIterator<Item = T>,
) -> u8 {
    let parsed = crumbkit::parse_to::<Cli, _, _, _, _>(sink, Cli::command(), &GUIDE, args);
    let (cli, output) = match parsed {
        Ok(parsed) => parsed,
        Err(stop) => return stop.exit(),
    };
    match cli.command {
        Command::Status => crumbkit::finish_to(sink, output, STATUS, status(probe)),
    }
}

/// Takes one sample with `probe` for `deadband status`.
fn status(probe: &impl Probe) -> Result<Prose<Sample>, Unreadable> {
    probe.sample().map(Prose::new)
}
