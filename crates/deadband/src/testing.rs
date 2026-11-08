//! Helpers that the unit tests of several modules share.

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::daemon::lock_path;

/// A socket path in the temporary directory that no other test of this
/// process uses. Dropping it removes the socket and its lock file.
#[derive(Debug)]
pub(crate) struct TestSocket(PathBuf);

impl Deref for TestSocket {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TestSocket {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(lock_path(&self.0));
    }
}

/// Returns a new [`TestSocket`].
pub(crate) fn socket_path() -> TestSocket {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    TestSocket(std::env::temp_dir().join(format!("db-{}-{n}.sock", std::process::id())))
}
