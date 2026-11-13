//! Starts deadband's threads. The operating system can refuse a new thread,
//! for example when the user has reached the limit on threads or processes.
//! [`thread`] hands that refusal back as an error, so a caller decides what
//! a missing thread means and no thread panics.

use std::io;
use std::thread::{Builder, JoinHandle};

/// Starts `run` on a new thread named `name`.
///
/// # Errors
///
/// Returns the operating system's error when it refuses the thread.
pub(crate) fn thread<T: Send + 'static>(
    name: &str,
    run: impl FnOnce() -> T + Send + 'static,
) -> io::Result<JoinHandle<T>> {
    if refused(name) {
        return Err(io::Error::from(io::ErrorKind::WouldBlock));
    }
    Builder::new().name(name.to_owned()).spawn(run)
}

/// Outside the tests, deadband refuses no thread of its own.
#[cfg(not(test))]
const fn refused(_name: &str) -> bool {
    false
}

#[cfg(test)]
thread_local! {
    /// The names of the threads that a test refuses on its own thread.
    static REFUSED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Reports whether the test running on this thread refused threads named
/// `name`.
#[cfg(test)]
fn refused(name: &str) -> bool {
    REFUSED.with(|refused| refused.borrow().iter().any(|each| each == name))
}

/// Makes [`thread`] fail for every thread named `name` that the calling
/// thread starts, as the operating system does at its thread limit, until
/// [`allow`] is called. The refusal is thread-local: it reaches only threads
/// that the calling thread itself starts, never threads started by a thread
/// it started. A test therefore cannot refuse the reclaim thread, which the
/// sampler thread starts.
#[cfg(test)]
pub(crate) fn refuse(name: &str) {
    REFUSED.with(|refused| refused.borrow_mut().push(name.to_owned()));
}

/// Lets the calling thread start threads of every name again.
#[cfg(test)]
pub(crate) fn allow() {
    REFUSED.with(|refused| refused.borrow_mut().clear());
}
