//! The `deadband` command line.

use std::ffi::OsString;
use std::io::Write;

use clap::{Parser, Subcommand};
use crumbkit::help::Guide;
use crumbkit::{Sink, ViewFlags};

use crate::probe::Probe;

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

/// deadband's guide.
pub const GUIDE: Guide = Guide::new();

/// Parses `args`, runs the command against `probe` and prints the result to
/// `sink`. Returns the exit code.
pub fn run_to<O: Write, W: Write, T: Into<OsString>>(
    _sink: &mut Sink<O, W>,
    _probe: &impl Probe,
    _args: impl IntoIterator<Item = T>,
) -> u8 {
    1
}
