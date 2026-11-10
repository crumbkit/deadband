//! The channel to the writer thread, and the thread that appends each line
//! to the history file.
//!
//! The writer has one file open at most. It opens the file when a line
//! arrives and none is open, and it closes the file after a failed write or
//! a rotation, so the next line opens it again. Each line is one write, and
//! the open adds a newline to a file whose last line a crash tore. A failure
//! is logged once when writing starts to fail and once when it recovers, and
//! it never reaches the job.
//!
//! Before it opens the file, the writer takes an exclusive lock on
//! `jobs.lock` in the state directory and holds it for its life, so one
//! writer exists per state directory even when two daemons listen on
//! different sockets. A writer that finds the lock held records nothing and
//! logs one line. If the writer thread stops, the first line that cannot
//! reach it is logged once.

use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions, Permissions, TryLockError};
use std::io::{self, Write as _};
use std::os::unix::fs::{
    DirBuilderExt as _, FileExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;

use super::{CHANNEL, FILE, Line, MAX_BYTES, ROTATED_FILE};
use crate::daemon::{Log, note, take_lock};

/// The mode of the state directory: only its owner may enter it.
const DIRECTORY_MODE: u32 = 0o700;

/// The mode of the history file: only its owner may read or write it.
const FILE_MODE: u32 = 0o600;

/// The name of the lock file in the state directory, which the writer holds
/// for its life.
pub const LOCK_FILE: &str = "jobs.lock";

/// The daemon's handle on the writer thread.
#[derive(Clone)]
pub struct Recorder {
    /// The channel to the writer.
    sender: SyncSender<Line>,
    /// How many lines found the channel full or the writer gone.
    dropped: Arc<AtomicU64>,
    /// Where the recorder says that the writer has stopped.
    log: Log,
    /// Whether the log has said so.
    told: Arc<AtomicBool>,
}

impl fmt::Debug for Recorder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Recorder")
            .field("dropped", &self.dropped)
            .finish_non_exhaustive()
    }
}

impl Recorder {
    /// Starts the writer thread for the state directory `directory`. A
    /// thread that cannot start leaves the daemon without a history, and
    /// `log` says why in one line.
    #[must_use]
    pub fn start(directory: PathBuf, log: &Log) -> Option<Self> {
        let (recorder, lines) = Self::channel(CHANNEL, Arc::clone(log));
        let writer = Writer::new(directory, MAX_BYTES, Arc::clone(log));
        let spawned = thread::Builder::new()
            .name("deadband-history".to_owned())
            .spawn(move || writer.run(&lines));
        match spawned {
            Ok(_) => Some(recorder),
            Err(error) => {
                note(
                    log,
                    format_args!(
                        "deadband could not start the thread that writes job history: {error}, so the daemon records no job"
                    ),
                );
                None
            }
        }
    }

    /// Returns a recorder and the end of its channel, which holds at most
    /// `capacity` lines.
    pub(super) fn channel(capacity: usize, log: Log) -> (Self, Receiver<Line>) {
        let (sender, lines) = mpsc::sync_channel(capacity);
        let recorder = Self {
            sender,
            dropped: Arc::new(AtomicU64::new(0)),
            log,
            told: Arc::new(AtomicBool::new(false)),
        };
        (recorder, lines)
    }

    /// Hands `line` to the writer without waiting. A line that cannot wait
    /// in the channel is dropped and counted.
    pub fn send(&self, line: Line) {
        let Err(error) = self.sender.try_send(line) else {
            return;
        };
        self.dropped.fetch_add(1, Ordering::Relaxed);
        if matches!(error, TrySendError::Disconnected(_))
            && !self.told.swap(true, Ordering::Relaxed)
        {
            note(
                &self.log,
                format_args!(
                    "the thread that writes job history has stopped, so the daemon records no more jobs and every job runs as usual"
                ),
            );
        }
    }

    /// Returns how many lines the daemon has dropped since it started.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// The open history file and how long it is.
#[derive(Debug)]
struct Open {
    /// The file, opened for appending.
    file: File,
    /// The file's length in bytes.
    size: u64,
}

/// The writer of the history file.
pub(super) struct Writer {
    /// The directory that holds the file.
    directory: PathBuf,
    /// The size past which the file rotates.
    limit: u64,
    /// Where the writer says that writing failed or recovered.
    log: Log,
    /// The lock on the state directory, once the writer holds it.
    lock: Option<File>,
    /// Whether another writer held the lock when this writer last tried.
    refused: bool,
    /// The open file, or `None` when the writer is closed.
    open: Option<Open>,
    /// Whether the last write failed.
    failing: bool,
    /// Whether the last rotation failed.
    rotation_failing: bool,
}

impl Writer {
    /// Returns a closed writer of the history file in `directory`, which
    /// rotates the file once it is longer than `limit` bytes.
    pub(super) fn new(directory: PathBuf, limit: u64, log: Log) -> Self {
        Self {
            directory,
            limit,
            log,
            lock: None,
            refused: false,
            open: None,
            failing: false,
            rotation_failing: false,
        }
    }

    /// Takes the directory's lock, then appends each line of `lines` until
    /// the channel closes. A daemon that finds the lock held says so at
    /// once.
    pub(super) fn run(mut self, lines: &Receiver<Line>) {
        if let Err(error) = self.claim() {
            self.fail(&error);
        }
        for line in lines {
            self.append(&line);
        }
    }

    /// Appends `line` as one line, and rotates the file when that took it
    /// past the limit. A failure loses the line, and the next line tries
    /// again.
    pub(super) fn append(&mut self, line: &Line) {
        match self.write(line) {
            Ok(()) => {
                if self.failing {
                    self.failing = false;
                    note(&self.log, format_args!("job history is written again"));
                }
                self.rotate_past_limit();
            }
            Err(error) => self.fail(&error),
        }
    }

    /// Closes the file after a failed write and logs the first failure.
    fn fail(&mut self, error: &io::Error) {
        // A partial write may have left a torn line, and the next open
        // repairs it.
        self.open = None;
        if !self.failing {
            self.failing = true;
            note(
                &self.log,
                format_args!(
                    "deadband could not write job history in {}: {error}, so the daemon loses those records and every job runs as usual",
                    self.directory.display()
                ),
            );
        }
    }

    /// Writes `line` to the open file, and opens the file if it is closed.
    /// While another writer holds the directory's lock, the line is lost and
    /// the first loss is logged.
    fn write(&mut self, line: &Line) -> io::Result<()> {
        if !self.claim()? {
            return Ok(());
        }
        let mut text = serde_json::to_vec(line).map_err(io::Error::other)?;
        text.push(b'\n');
        let open = match &mut self.open {
            Some(open) => open,
            closed => closed.insert(open_file(&self.directory)?),
        };
        open.file.write_all(&text)?;
        let written = u64::try_from(text.len()).unwrap_or(u64::MAX);
        open.size = open.size.saturating_add(written);
        Ok(())
    }

    /// Takes the lock on the state directory unless the writer holds it,
    /// and returns whether the writer holds it now.
    fn claim(&mut self) -> io::Result<bool> {
        if self.lock.is_some() {
            return Ok(true);
        }
        make_directory(&self.directory)?;
        match take_lock(&self.directory.join(LOCK_FILE)) {
            Ok(lock) => {
                self.lock = Some(lock);
                self.refused = false;
                Ok(true)
            }
            Err(TryLockError::WouldBlock) => {
                if !self.refused {
                    self.refused = true;
                    note(
                        &self.log,
                        format_args!(
                            "another daemon writes job history in {}, so this daemon records nothing while that daemon runs, and every job runs as usual",
                            self.directory.display()
                        ),
                    );
                }
                Ok(false)
            }
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    /// Renames the file to the rotated name when it has passed the limit.
    /// The size check comes after a successful write. A failed rename is
    /// logged once, the writer keeps appending, and the next write tries
    /// again.
    fn rotate_past_limit(&mut self) {
        if self
            .open
            .as_ref()
            .is_none_or(|open| open.size <= self.limit)
        {
            return;
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "crates/deadband/src/history/writer.rs rotates an append-only log; rust-standards.toml's DUR-1 exception says why this file does not use an atomic writer"
        )]
        let renamed = fs::rename(self.directory.join(FILE), self.directory.join(ROTATED_FILE));
        match renamed {
            Ok(()) => {
                self.open = None;
                self.rotation_failing = false;
            }
            Err(error) => {
                if !self.rotation_failing {
                    self.rotation_failing = true;
                    note(
                        &self.log,
                        format_args!(
                            "deadband could not rotate the job history in {}: {error}, so the file keeps growing until a later rotation works",
                            self.directory.display()
                        ),
                    );
                }
            }
        }
    }
}

/// Opens the history file in `directory`, making the directory and the file
/// when they do not exist, and adds a newline when the last line is torn.
fn open_file(directory: &Path) -> io::Result<Open> {
    make_directory(directory)?;
    #[expect(
        clippy::disallowed_methods,
        reason = "crates/deadband/src/history/writer.rs appends to a log; rust-standards.toml's DUR-1 exception says why this file does not use an atomic writer"
    )]
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .mode(FILE_MODE)
        .open(directory.join(FILE))?;
    file.set_permissions(Permissions::from_mode(FILE_MODE))?;
    let mut size = file.metadata()?.len();
    if let Some(last) = size.checked_sub(1) {
        let mut byte = [0_u8];
        file.read_exact_at(&mut byte, last)?;
        if byte != *b"\n" {
            (&file).write_all(b"\n")?;
            size = size.saturating_add(1);
        }
    }
    Ok(Open { file, size })
}

/// Makes `directory` with mode 0700 when it does not exist, and sets that
/// mode when it does.
fn make_directory(directory: &Path) -> io::Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(DIRECTORY_MODE)
        .create(directory)?;
    fs::set_permissions(directory, Permissions::from_mode(DIRECTORY_MODE))
}
