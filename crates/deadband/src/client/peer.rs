//! Reads the uid of the user who owns the process on the other end of a
//! connection, so the client sends its command line only to its own daemon.

use std::io;
use std::os::unix::net::UnixStream;

/// Returns the effective uid of the process that listens on the other end
/// of `stream`: `SO_PEERCRED` on Linux and `getpeereid` on macOS.
///
/// # Errors
///
/// Returns the error of the call, or `Unsupported` on a platform that
/// deadband has no reader for.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    rustix::net::sockopt::socket_peercred(stream)
        .map(|credentials| credentials.uid.as_raw())
        .map_err(io::Error::from)
}

/// Returns the effective uid of the process that listens on the other end
/// of `stream`.
///
/// # Errors
///
/// Returns the error of `getpeereid`.
#[cfg(target_os = "macos")]
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    crate::probe::peer_uid(stream)
}

/// Says that this platform has no reader, so the client starts its job at
/// once and says why.
///
/// # Errors
///
/// Always returns `Unsupported`.
#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
pub(crate) fn peer_uid(_: &UnixStream) -> io::Result<u32> {
    Err(io::ErrorKind::Unsupported.into())
}
