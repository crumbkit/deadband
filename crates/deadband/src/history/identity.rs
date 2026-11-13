//! The identity of a file the writer holds open, by device and inode, so
//! the writer can tell when its path names another file or none.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

/// A file named by the device that holds it and its inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Identity {
    /// The device that holds the file.
    device: u64,
    /// The file's inode.
    inode: u64,
}

impl Identity {
    /// Returns the identity of the open file `file`.
    pub(super) fn of(file: &File) -> io::Result<Self> {
        Ok(Self::from(&file.metadata()?))
    }

    /// Reports whether `path` still names this file. A path whose file
    /// cannot be read counts as naming another file, so the caller opens
    /// the path again.
    pub(super) fn still_at(self, path: &Path) -> bool {
        fs::metadata(path).is_ok_and(|metadata| Self::from(&metadata) == self)
    }
}

impl From<&fs::Metadata> for Identity {
    fn from(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}
