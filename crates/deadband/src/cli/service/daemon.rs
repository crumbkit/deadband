//! How `deadband service` finds the daemon that launchd runs and waits for
//! launchd to unload or start it.

use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// How long install asks a freshly loaded daemon for its jobs, two
/// seconds, [`PAUSE`](crate::job::launchd::PAUSE) apart.
///
/// Each ask returns at once when nothing listens on the socket, so the
/// asks take two seconds. An ask that a process accepts and never answers
/// lasts the client's [`ANSWER_WITHIN`](crate::client::ANSWER_WITHIN), also
/// two seconds, so an ask that starts just before the deadline ends the wait
/// at about four seconds.
const ANSWER_WITHIN: Duration = Duration::from_secs(2);

/// Returns whether `answers` becomes true within [`ANSWER_WITHIN`], which
/// is whether the daemon that launchd started listens on its socket.
pub(super) fn wait_for_answer(agent: &LaunchAgent, answers: impl FnMut() -> bool) -> bool {
    until(agent, ANSWER_WITHIN, answers)
}

/// How long install polls `launchctl print` after a bootout, five seconds,
/// [`PAUSE`](crate::job::launchd::PAUSE) apart. A poll that starts just
/// before the deadline ends the wait when launchctl returns.
const UNLOAD_WITHIN: Duration = Duration::from_secs(5);

/// Waits until launchd no longer lists the agent, which happens once the
/// booted-out daemon has exited, so the bootstrap that follows is not
/// refused. After [`UNLOAD_WITHIN`], or when a poll fails, it stops waiting
/// and the bootstrap reports what launchd says.
pub(super) fn wait_until_unloaded(agent: &LaunchAgent) {
    let target = agent.target();
    until(agent, UNLOAD_WITHIN, || {
        !matches!(agent.launchd.is_loaded(&target), Ok(true))
    });
}

/// Returns whether `done` becomes true before `within` has passed on the
/// agent's launchd clock, asking it again after each pause.
fn until(agent: &LaunchAgent, within: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = agent.launchd.now() + within;
    loop {
        if done() {
            return true;
        }
        if agent.launchd.now() >= deadline {
            return false;
        }
        agent.launchd.pause();
    }
}
