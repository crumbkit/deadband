//! The rule that says when the daemon starts a reclaim. It reads no clock,
//! so the tests check each case directly.

use std::fs::{File, TryLockError};
use std::path::Path;
use std::time::{Duration, Instant};

use super::{Config, Resource};
use crate::daemon::take_lock;

/// The name of the lock file in the state directory that one reclaim holds.
const LOCK_FILE: &str = "reclaim.lock";

/// The lock that lets one reclaim at a time run for a user, whichever
/// daemons are up. It holds until the value is dropped.
#[derive(Debug)]
pub struct ReclaimLock {
    /// The open lock file, which holds the lock until it is closed.
    _file: Option<File>,
}

impl ReclaimLock {
    /// Takes the lock in `directory` without waiting, and returns `None`
    /// when another reclaim holds it. A lock file that cannot be made or
    /// locked holds nothing, and the reclaim runs without a lock.
    pub fn take(directory: &Path) -> Option<Self> {
        if std::fs::create_dir_all(directory).is_err() {
            return Some(Self { _file: None });
        }
        match take_lock(&directory.join(LOCK_FILE)) {
            Ok(file) => Some(Self { _file: Some(file) }),
            Err(TryLockError::WouldBlock) => None,
            Err(TryLockError::Error(_)) => Some(Self { _file: None }),
        }
    }
}

/// The longest that the daemon waits between reclaims of one resource
/// after reclaims that freed nothing. Each such reclaim doubles the wait,
/// from `reclaim_after` up to this.
pub const MAX_REST: Duration = Duration::from_secs(30 * 60);

/// What the rule remembers of one resource.
#[derive(Debug, Default, Clone, Copy)]
struct Track {
    /// Since when the resource has been short without a break, counted
    /// from the end of the last reclaim of it.
    short_since: Option<Instant>,
    /// When the last reclaim of the resource ended.
    ended: Option<Instant>,
    /// How many reclaims of the resource in a row freed nothing, while it
    /// stayed short.
    fruitless: u32,
}

impl Track {
    /// Records whether the resource is `short` at `now`. A resource that
    /// is not short ends its back-off.
    fn see(&mut self, short: bool, now: Instant) {
        self.short_since = short.then(|| self.short_since.unwrap_or(now));
        if !short {
            self.fruitless = 0;
        }
    }

    /// Reports whether the resource has been short for at least `lasted`
    /// at `now`.
    fn lasted(&self, lasted: Duration, now: Instant) -> bool {
        self.short_since
            .is_some_and(|since| now.saturating_duration_since(since) >= lasted)
    }

    /// Reports whether the rest after the last reclaim has passed at
    /// `now`: `after`, doubled for each reclaim in a row that freed
    /// nothing, up to [`MAX_REST`] or `after` when that is longer.
    fn rested(&self, after: Duration, now: Instant) -> bool {
        let rest = after
            .saturating_mul(2_u32.saturating_pow(self.fruitless))
            .min(MAX_REST.max(after));
        self.ended
            .is_none_or(|ended| now.saturating_duration_since(ended) >= rest)
    }
}

/// The rule that says when the daemon starts a reclaim, fed with what the
/// daemon reads each second. Each resource keeps its own rest, so a disk
/// need that no reclaimer meets never holds back a memory reclaim.
#[derive(Debug, Default, Clone, Copy)]
pub struct Watch {
    /// What the rule remembers of memory.
    memory: Track,
    /// What the rule remembers of disk.
    disk: Track,
    /// The resource of the running reclaim.
    running: Option<Resource>,
}

impl Watch {
    /// Records that at `now` memory was short, or not, and free disk was
    /// critically low, or not, and returns what to reclaim now. A returned
    /// resource counts as a running reclaim until [`Watch::ended`].
    ///
    /// Nothing is reclaimed without a reclaimer in `config`, or while a
    /// reclaim runs. Critically low disk is reclaimed at once, and before
    /// memory. Short memory is reclaimed once it has lasted
    /// `config.after`. After a reclaim, the same resource rests for
    /// `config.after`, doubled for each reclaim in a row that freed
    /// nothing, up to [`MAX_REST`], and short memory must last
    /// `config.after` again.
    pub fn observe(
        &mut self,
        config: &Config,
        memory_short: bool,
        disk_critical: bool,
        now: Instant,
    ) -> Option<Resource> {
        self.memory.see(memory_short, now);
        self.disk.see(disk_critical, now);
        if config.reclaimers.is_empty() || self.running.is_some() {
            return None;
        }
        let after = config.after;
        let resource = if self.disk.lasted(Duration::ZERO, now) && self.disk.rested(after, now) {
            Resource::Disk
        } else if self.memory.lasted(after, now) && self.memory.rested(after, now) {
            Resource::Memory
        } else {
            return None;
        };
        self.running = Some(resource);
        Some(resource)
    }

    /// Records that the running reclaim ended at `now` after it freed
    /// `freed_bytes`. A reclaim that freed nothing lengthens the next rest
    /// of its resource.
    pub fn ended(&mut self, freed_bytes: u64, now: Instant) {
        let Some(resource) = self.running.take() else {
            return;
        };
        let track = self.track(resource);
        track.ended = Some(now);
        track.short_since = None;
        track.fruitless = if freed_bytes == 0 {
            track.fruitless.saturating_add(1)
        } else {
            0
        };
    }

    /// Reports whether the last reclaim of `resource` freed nothing while
    /// it stayed short, so a plan of nothing need not be logged again.
    #[must_use]
    pub const fn quiet(&self, resource: Resource) -> bool {
        match resource {
            Resource::Memory => self.memory.fruitless > 0,
            Resource::Disk => self.disk.fruitless > 0,
        }
    }

    /// Returns what the rule remembers of `resource`.
    const fn track(&mut self, resource: Resource) -> &mut Track {
        match resource {
            Resource::Memory => &mut self.memory,
            Resource::Disk => &mut self.disk,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ReclaimLock;

    #[test]
    fn a_second_reclaim_finds_the_lock_held_until_the_first_ends() {
        let directory = std::env::temp_dir().join(format!("db-rl-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("the directory is made");
        let first = ReclaimLock::take(&directory).expect("the first reclaim takes the lock");
        assert!(ReclaimLock::take(&directory).is_none());
        drop(first);
        assert!(ReclaimLock::take(&directory).is_some());
        let _ = std::fs::remove_dir_all(&directory);
    }
}
