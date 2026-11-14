//! What a job's arrival tells: the kind and the option names of its command
//! line, and the checkout and the workspace that its working directory lies
//! in.
//!
//! The kind rule names no tool in code. It keeps the program's file name, and
//! keeps subcommand words only for the programs that the config's
//! `kind_tools` lists, so a script's arguments never reach the history file.
//! It only keeps words of a plain form, so the record cannot hold an option
//! value, an environment variable or a path. A command line can hold a
//! secret, so the words that the rule does not keep never reach the file.

use std::fs::{self, File};
use std::io::Read as _;
use std::path::Path;

use super::now_ms;
use crate::protocol::{Ago, Job, Label};

/// The most bytes of a `.git` file that the daemon reads.
const GIT_FILE_BYTES: u64 = 4096;

/// The name of the file that marks a workspace directory. A tool that makes
/// workspaces, such as mxv, writes it, and the workspace is the name of the
/// directory that holds it.
pub const WORKSPACE_MARKER: &str = ".deadband-workspace";

/// The most directories that the walk up from a job's working directory
/// visits, the working directory included. A marker or a `.git` above them
/// is not found, and the job has no workspace or checkout.
const MAX_DEPTH: usize = 64;

/// The most subcommand words that the kind of a listed tool keeps.
const SUBCOMMANDS: usize = 2;

/// The most bytes in a word of a kind or in the name of an option. A longer
/// word is left out, because a long word is more likely to be a token.
const MAX_WORD: usize = 32;

/// The most option names that one line records. Further options are left
/// out, which keeps the line bounded.
const MAX_OPTIONS: usize = 32;

/// The program whose leading `NAME=value` words set the environment of the
/// program that follows them.
const ENV: &str = "env";

/// Returns the kind of `command`: the program's file name, and, when that
/// name is in `tools`, up to two plain lowercase subcommand words after it.
/// A leading `env` and the `NAME=value` words after it are skipped, so the
/// program is the word that follows them. The subcommands stop at the first
/// word that starts with `-`, because the word after an option may be its
/// value, and a word that is not plain is skipped. Only the program's file
/// name may hold digits and dots. A program that is not plain gives an empty
/// kind.
#[must_use]
pub fn kind(command: &str, tools: &[String]) -> String {
    let mut words = command.split_whitespace().peekable();
    if words.peek().map(|word| file_name(word)) == Some(ENV) {
        words.next();
        while words.next_if(|word| word.contains('=')).is_some() {}
    }
    let Some(program) = words.next().map(file_name).filter(|name| plain(name, true)) else {
        return String::new();
    };
    let subcommands = tools
        .iter()
        .any(|tool| tool == program)
        .then_some(words)
        .into_iter()
        .flatten()
        .take_while(|word| !word.starts_with('-'))
        .filter(|word| plain(word, false))
        .take(SUBCOMMANDS);
    std::iter::once(program)
        .chain(subcommands)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Returns the names of the options of `command`, at most 32, without any
/// value: each `--name`, with an `=value` removed, and each exact `-x`.
#[must_use]
pub fn options(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .skip(1)
        .filter_map(option_name)
        .take(MAX_OPTIONS)
        .collect()
}

/// Returns the option name that `word` holds, or `None` when it is not one.
fn option_name(word: &str) -> Option<String> {
    if let Some(rest) = word.strip_prefix("--") {
        let name = rest.split('=').next().filter(|name| long_name(name))?;
        return Some(format!("--{name}"));
    }
    let letter = word.strip_prefix('-')?;
    (letter.len() == 1 && letter.bytes().all(|byte| byte.is_ascii_alphabetic()))
        .then(|| word.to_owned())
}

/// Reports whether `name` has the form of a long option's name.
fn long_name(name: &str) -> bool {
    name.len() <= MAX_WORD
        && name.starts_with(|first: char| first.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Returns the file name of the program word `word`, the part after its last
/// `/`.
fn file_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// Reports whether `word` is a plain lowercase word: it starts with a
/// lowercase letter, holds only lowercase letters, `-` and `_`, also digits
/// and dots when `program` is true, and has at most [`MAX_WORD`] bytes.
fn plain(word: &str, program: bool) -> bool {
    word.len() <= MAX_WORD
        && word.starts_with(|first: char| first.is_ascii_lowercase())
        && word.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte == b'-'
                || byte == b'_'
                || (program && (byte.is_ascii_digit() || byte == b'.'))
        })
}

/// Where a job ran, found by walking up from its working directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Place {
    /// The final directory name of the main checkout, or `None` outside git.
    pub repo: Option<String>,
    /// The final directory name of the worktree, or `None` outside git.
    pub checkout: Option<String>,
    /// Whether a directory of the config's `warm_dirs` has an entry under
    /// the checkout, or `None` when `warm_dirs` is empty or the job is
    /// outside git.
    pub warm: Option<bool>,
    /// The name of the nearest directory at or above the working directory
    /// that holds [`WORKSPACE_MARKER`] as a regular file, or `None` when no
    /// such directory is found.
    pub workspace: Option<Label>,
}

impl Place {
    /// Walks up from `cwd` to the first directory that holds `.git`, and
    /// looks for `warm_dirs` under it, and to the first directory that holds
    /// [`WORKSPACE_MARKER`]. The walk visits at most 64 directories. A path
    /// that is relative or cannot be read gives an empty place. This call
    /// reads the disk, so the daemon makes it before it takes the queue
    /// lock.
    #[must_use]
    pub fn find(cwd: &Path, warm_dirs: &[String]) -> Self {
        if !cwd.is_absolute() {
            return Self::default();
        }
        let mut found = None;
        let mut workspace = None;
        for directory in cwd.ancestors().take(MAX_DEPTH) {
            if found.is_none()
                && let Ok(metadata) = fs::symlink_metadata(directory.join(".git"))
            {
                found = Some((directory, metadata.is_file()));
            }
            // Only a regular file counts, and the daemon never opens it. A
            // later tool could have the daemon read a name from the file.
            if workspace.is_none()
                && fs::symlink_metadata(directory.join(WORKSPACE_MARKER))
                    .is_ok_and(|metadata| metadata.is_file())
            {
                workspace = final_name(directory).map(|name| Label::new(&name));
            }
            if found.is_some() && workspace.is_some() {
                break;
            }
        }
        let Some((root, linked)) = found else {
            return Self {
                workspace,
                ..Self::default()
            };
        };
        let checkout = final_name(root);
        let repo = linked
            .then(|| main_checkout(root))
            .flatten()
            .or_else(|| checkout.clone());
        let warm = (!warm_dirs.is_empty()).then(|| {
            warm_dirs.iter().any(|each| {
                fs::read_dir(root.join(each)).is_ok_and(|mut entries| entries.next().is_some())
            })
        });
        Self {
            repo,
            checkout,
            warm,
            workspace,
        }
    }
}

/// Returns the final directory name of `path`.
fn final_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// Returns the name of the main checkout that the `.git` file in `root`
/// points to, or `None` when the file names no worktree, as a submodule's
/// does. The file reads `gitdir: <main>/.git/worktrees/<name>`.
fn main_checkout(root: &Path) -> Option<String> {
    let mut text = String::new();
    File::open(root.join(".git"))
        .ok()?
        .take(GIT_FILE_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    let gitdir = Path::new(text.strip_prefix("gitdir:")?.trim());
    let worktrees = gitdir.parent().filter(|each| each.ends_with("worktrees"))?;
    let git = worktrees.parent()?;
    let main = if git.ends_with(".git") {
        git.parent()?
    } else {
        git
    };
    final_name(main)
}

/// What the daemon learns about a job when it arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrival {
    /// When the job asked, in Unix milliseconds.
    pub at_ms: u64,
    /// Whether a new daemon took over the job while it ran.
    pub seated: bool,
    /// The job's kind.
    pub kind: String,
    /// The names of the job's options.
    pub options: Vec<String>,
    /// Where the job ran.
    pub place: Place,
}

impl Arrival {
    /// Returns what `job` tells at this moment. `ran` says how long ago a
    /// job that a new daemon takes over started, and it is `None` for a job
    /// that asks to start. A job that has run has had the chance to fill its
    /// own build directory, so its `warm` is `None`.
    ///
    /// This call reads the disk, so the daemon makes it before it takes the
    /// queue lock.
    #[must_use]
    pub fn read(job: &Job, warm_dirs: &[String], kind_tools: &[String], ran: Option<Ago>) -> Self {
        let mut place = job
            .cwd
            .as_deref()
            .map_or_else(Place::default, |cwd| Place::find(Path::new(cwd), warm_dirs));
        if ran.is_some() {
            place.warm = None;
        }
        Self {
            at_ms: now_ms().saturating_sub(ran.map_or(0, Ago::as_millis)),
            seated: ran.is_some(),
            kind: kind(&job.command, kind_tools),
            options: options(&job.command),
            place,
        }
    }
}
