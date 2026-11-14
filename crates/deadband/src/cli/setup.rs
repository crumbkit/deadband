//! The commands that set deadband up for an agent and a shell: `deadband
//! skills install|check|print`, which manage the agent skill that deadband
//! embeds, and `deadband completion install|check|print`, which manage the
//! shell completion.

use std::io::Write;

use clap::{CommandFactory as _, Subcommand};
use crumbkit::completion::{
    self, COMPLETION_CHECK, COMPLETION_INSTALL, COMPLETION_PRINT, CompletionCheck,
    CompletionInstall, CompletionPrint,
};
use crumbkit::{Run, Sink};
use crumbkit_skills::Skill;

use super::Cli;

/// deadband's agent skill, which `deadband skills install` writes into the
/// agent skills directories.
pub const SKILL: Skill = Skill::new(
    "deadband",
    env!("CARGO_PKG_VERSION"),
    include_str!("../../skill/SKILL.md"),
);

/// The commands for deadband's shell completion.
#[derive(Debug, Subcommand)]
pub enum CompletionCommand {
    /// Install deadband's completion where the shell loads it.
    Install(CompletionInstall),
    /// Check that deadband's installed completion file matches this
    /// deadband.
    Check(CompletionCheck),
    /// Print deadband's completion registration script for a shell.
    Print(CompletionPrint),
}

/// Runs `deadband completion install|check|print` and prints the result to
/// `sink`.
pub(super) fn completion<O: Write, W: Write>(
    sink: &mut Sink<O, W>,
    output: Run,
    command: &CompletionCommand,
) -> u8 {
    match command {
        CompletionCommand::Install(args) => {
            let result = completion::install(Cli::command, args);
            crumbkit::finish_to(sink, output, COMPLETION_INSTALL, result)
        }
        CompletionCommand::Check(args) => {
            let result = completion::check(Cli::command, args);
            crumbkit::finish_to(sink, output, COMPLETION_CHECK, result)
        }
        CompletionCommand::Print(args) => {
            let result = completion::print(Cli::command, args);
            crumbkit::finish_to(sink, output, COMPLETION_PRINT, result)
        }
    }
}
