//! How `deadband service` finds the daemon that launchd runs and waits for
//! launchd to unload or start it.

use std::path::{Path, PathBuf};

use super::{Counts, LaunchAgent};

/// Returns the socket of the daemon that launchd runs. That daemon listens
/// under the directory that getconf prints as `temp_dir`, whatever the
/// caller's `TMPDIR` says.
pub(super) fn socket_in(temp_dir: &Path) -> PathBuf {
    crate::socket::path(|name| (name == "TMPDIR").then(|| temp_dir.as_os_str().to_owned()))
}

/// Returns the jobs of the daemon that launchd runs, which `jobs` asks for
/// on that daemon's socket. It returns `None` when there is no agent, when
/// launchd has not loaded it, or when `jobs` returns `None`, so only a loaded
/// agent's daemon is asked.
pub fn held(
    agent: Option<&LaunchAgent>,
    jobs: impl FnOnce(&Path) -> Option<Counts>,
) -> Option<Counts> {
    let agent = agent?;
    if !agent.launchd.is_loaded(&agent.target()).ok()? {
        return None;
    }
    let temp_dir = agent.launchd.temp_dir().ok()?;
    jobs(&socket_in(&temp_dir))
}

/// The most times that install asks a freshly loaded daemon for its jobs,
/// [`PAUSE`](crate::job::launchd::PAUSE) apart.
///
/// Each ask returns at once when nothing listens on the socket. A process
/// that accepts the connection and never replies holds an ask for the
/// client's `ANSWER_WITHIN`, which is two seconds. The polls therefore take
/// about two seconds when nothing listens, and at worst 20 times 2.1 seconds,
/// which is 42 seconds, when a process accepts and never replies.
const ANSWER_POLLS: u32 = 20;

/// Returns whether `answers` becomes true within [`ANSWER_POLLS`] asks, which
/// is whether the daemon that launchd started listens on its socket.
pub(super) fn wait_for_answer(agent: &LaunchAgent, mut answers: impl FnMut() -> bool) -> bool {
    for _ in 0..ANSWER_POLLS {
        if answers() {
            return true;
        }
        agent.launchd.pause();
    }
    false
}

/// The most `launchctl print` polls that install makes after a bootout,
/// [`PAUSE`](crate::job::launchd::PAUSE) apart, about five seconds in all.
const UNLOAD_POLLS: u32 = 50;

/// Waits until launchd no longer lists the agent, which happens once the
/// booted-out daemon has exited, so the bootstrap that follows is not
/// refused. After [`UNLOAD_POLLS`] polls, or when a poll fails, it stops
/// waiting and the bootstrap reports what launchd says.
pub(super) fn wait_until_unloaded(agent: &LaunchAgent) {
    for _ in 0..UNLOAD_POLLS {
        match agent.launchd.is_loaded(&agent.target()) {
            Ok(true) => agent.launchd.pause(),
            Ok(false) | Err(_) => return,
        }
    }
}
