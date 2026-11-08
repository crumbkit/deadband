//! Helpers that the unit tests of several modules share.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Returns a socket path in the temporary directory that no other test of
/// this process uses.
pub(crate) fn socket_path() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("db-{}-{n}.sock", std::process::id()))
}
