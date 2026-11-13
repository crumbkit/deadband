//! The identity of a file, by the device that holds it and its inode, so a
//! holder can tell when a path names another file or none.

use std::fs::{File, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt as _;

/// A file named by the device that holds it and its inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileId {
    /// The device that holds the file.
    device: u64,
    /// The file's inode.
    inode: u64,
}

impl FileId {
    /// Returns the identity of the open file `file`.
    pub(crate) fn of(file: &File) -> io::Result<Self> {
        Ok(Self::from(&file.metadata()?))
    }

    /// Reports whether `metadata` describes this file.
    pub(crate) fn is(self, metadata: &Metadata) -> bool {
        Self::from(metadata) == self
    }
}

impl From<&Metadata> for FileId {
    fn from(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FileId;

    #[test]
    fn a_file_id_is_the_open_files_and_its_paths_but_not_another_files() {
        let directory = crate::testing::TestHome::new();
        let first = directory.join("first");
        let second = directory.join("second");
        crate::history::tests::write_file(&first, "a").expect("the file is made");
        crate::history::tests::write_file(&second, "b").expect("the file is made");
        let file = std::fs::File::open(&first).expect("the file opens");

        let id = FileId::of(&file).expect("the open file has metadata");

        assert!(id.is(&std::fs::metadata(&first).expect("the path is readable")));
        assert!(!id.is(&std::fs::metadata(&second).expect("the path is readable")));
    }
}
