//! The commands that describe deadband to an agent: `deadband commands
//! list`, which lists the command tree with each command's arguments, and
//! `deadband schemas list` and `deadband schemas show`, which serve the
//! JSON Schema of each document deadband prints and of its config file.

use std::io::Write;

use clap::Subcommand;
use crumbkit::commands::{self, COMMANDS_LIST, CommandsList};
use crumbkit::schemas::{self, SCHEMAS_LIST, SCHEMAS_SHOW, SchemasList, SchemasShow};
use crumbkit::{Run, Sink};

use super::TOOL;

/// The commands of `deadband commands`.
#[derive(Debug, Subcommand)]
pub enum CommandsCommand {
    /// List deadband's commands with their arguments, examples and columns.
    List(CommandsList),
}

/// The commands of `deadband schemas`.
#[derive(Debug, Subcommand)]
pub enum SchemasCommand {
    /// List the JSON documents that deadband prints and the files it reads.
    List(SchemasList),
    /// Print the JSON Schema of each document or file format.
    Show(SchemasShow),
}

/// Runs `deadband commands list` and prints the result to `sink`.
pub(super) fn commands<O: Write, W: Write>(
    sink: &mut Sink<O, W>,
    mut output: Run,
    command: &CommandsCommand,
) -> u8 {
    match command {
        CommandsCommand::List(args) => {
            // `list` builds the command tree in `output`, so it runs before
            // `finish_to`.
            let result = commands::list(&mut output, &TOOL, args);
            crumbkit::finish_to(sink, output, COMMANDS_LIST, result)
        }
    }
}

/// Runs `deadband schemas list` or `deadband schemas show` and prints the
/// result to `sink`.
pub(super) fn schemas<O: Write, W: Write>(
    sink: &mut Sink<O, W>,
    mut output: Run,
    command: &SchemasCommand,
) -> u8 {
    match command {
        SchemasCommand::List(args) => {
            let result = schemas::list(&mut output, &TOOL, args);
            crumbkit::finish_to(sink, output, SCHEMAS_LIST, result)
        }
        SchemasCommand::Show(args) => {
            let result = schemas::show(&mut output, &TOOL, args);
            crumbkit::finish_to(sink, output, SCHEMAS_SHOW, result)
        }
    }
}
