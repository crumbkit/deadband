//! Puts the terminal into raw mode and the alternate screen, and restores
//! it on every exit path.

use std::io;

/// The terminal modes that the view changes.
pub(super) trait Modes {
    /// What the modes give the caller to draw on.
    type Screen;

    /// Enters the modes.
    ///
    /// # Errors
    ///
    /// Returns the error of a mode that could not be entered.
    fn enter(&mut self) -> io::Result<Self::Screen>;

    /// Leaves the modes. A terminal that cannot be restored stays as it is.
    fn leave(&mut self);
}

/// Holds the modes entered, and leaves them when it drops on a normal exit
/// and on an error. During a panic it does nothing, because ratatui's panic
/// hook has already restored the terminal and printed the message, and a
/// second restore would move the cursor back over that message.
///
/// A SIGTERM or SIGKILL ends the process with no unwind, so the terminal
/// stays raw and the person runs `reset`. This is a deliberate limit: nothing
/// can catch SIGKILL, and SIGTERM goes unhandled here because only an outside
/// kill reaches it, since Ctrl-C arrives as a key. A handler through
/// signal-hook, which crossterm already brings in, that sets a flag the poll
/// loop checks, would replace the limit for SIGTERM.
pub(super) struct Guard<M: Modes>(M);

impl<M: Modes> Guard<M> {
    /// Enters `modes` and returns the guard with what the modes give to
    /// draw on. Modes that fail half way are left again.
    ///
    /// # Errors
    ///
    /// Returns the error of a mode that could not be entered.
    pub(super) fn enter(modes: M) -> io::Result<(Self, M::Screen)> {
        let mut guard = Self(modes);
        let screen = guard.0.enter()?;
        Ok((guard, screen))
    }
}

impl<M: Modes> Drop for Guard<M> {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.0.leave();
        }
    }
}
