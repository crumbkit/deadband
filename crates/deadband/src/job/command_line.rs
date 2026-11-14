//! A command line with its program, which a config file or `deadband run`
//! names and the job module starts.

use std::ffi::OsStr;
use std::process::Command;

/// A program and its arguments. A command line always names a program, so
/// what reads a command checks once that it is not empty, and nothing that
/// starts it checks again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine<W = String> {
    /// The program, which the operating system finds on `PATH` unless it is
    /// a path.
    pub program: W,
    /// The arguments, each passed as a separate value. No shell reads them.
    pub args: Vec<W>,
}

impl<W> CommandLine<W> {
    /// Returns the command line whose first word is the program, or `None`
    /// for no words.
    pub fn from_words(words: impl IntoIterator<Item = W>) -> Option<Self> {
        let mut words = words.into_iter();
        Some(Self {
            program: words.next()?,
            args: words.collect(),
        })
    }
}

impl<W: AsRef<OsStr>> CommandLine<W> {
    /// Returns a [`Command`] that runs this program with these arguments,
    /// for the job module to give its streams and start.
    pub(crate) fn command(&self) -> Command {
        #[expect(
            clippy::disallowed_methods,
            reason = "crates/deadband/src/job/command_line.rs builds the command that the job module starts; each module that starts it states PROC-2's items"
        )]
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command
    }
}

impl CommandLine {
    /// Returns the command line of `words`, for a test.
    ///
    /// # Panics
    ///
    /// Panics for no words.
    #[cfg(test)]
    pub(crate) fn of(words: &[&str]) -> Self {
        Self::from_words(words.iter().map(|word| (*word).to_owned()))
            .expect("a test command names a program")
    }
}

#[cfg(test)]
mod tests {
    use super::CommandLine;

    #[test]
    fn no_words_name_no_command_line() {
        assert_eq!(CommandLine::<String>::from_words(Vec::new()), None);
    }

    #[test]
    fn the_first_word_is_the_program_and_the_rest_are_its_arguments() {
        let line = CommandLine::from_words(["notify", "--to", "me"].map(str::to_owned))
            .expect("three words name a program");
        assert_eq!(line.program, "notify");
        assert_eq!(line.args, ["--to", "me"]);
    }

    #[test]
    fn a_command_line_builds_a_command_with_its_program_and_arguments() {
        let command = CommandLine::of(&["test", "a b", "=", "a b"]).command();
        assert_eq!(command.get_program(), "test");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["a b", "=", "a b"]);
    }
}
