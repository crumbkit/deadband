//! The rule that says when the daemon starts a reclaim. It takes its
//! monotonic time from the caller, so the tests check each case directly,
//! and reads the wall clock only to note when a reclaim starts.

use std::fmt;
use std::time::{Duration, Instant, SystemTime};

use crumbkit::Age;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Config, Resource, ended_at};

/// A reclaim that runs now, as `deadband status` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Reclaiming {
    /// The `--need` that the daemon asked for, such as `memory=4G`.
    pub need: String,
    /// When the reclaim started, which JSON holds in RFC 3339 and text
    /// shows as how long ago that was.
    #[serde(deserialize_with = "ended_at")]
    pub since: Age,
}

impl fmt::Display for Reclaiming {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "reclaiming: {}, started {} ago", self.need, self.since)
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
    /// Whether the last reclaim of the resource did not run because another
    /// reclaim held the lock, so the rest after it is `after` alone.
    skipped: bool,
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
            .saturating_mul(2_u32.saturating_pow(if self.skipped { 0 } else { self.fruitless }))
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
    /// The resource of the running reclaim and when it started.
    running: Option<(Resource, SystemTime)>,
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
        self.running = Some((resource, SystemTime::now()));
        Some(resource)
    }

    /// Records that the running reclaim ended at `now` after it freed
    /// `freed_bytes`. A reclaim that freed nothing lengthens the next rest
    /// of its resource.
    pub fn ended(&mut self, freed_bytes: u64, now: Instant) {
        let Some((resource, _)) = self.running.take() else {
            return;
        };
        let track = self.track(resource);
        track.ended = Some(now);
        track.short_since = None;
        track.skipped = false;
        track.fruitless = if freed_bytes == 0 {
            track.fruitless.saturating_add(1)
        } else {
            0
        };
    }

    /// Records that the running reclaim did not run at `now` because
    /// another reclaim held the lock. The resource rests for
    /// exactly `config.after`, and its count of fruitless reclaims stays as
    /// it was.
    pub fn skipped(&mut self, now: Instant) {
        let Some((resource, _)) = self.running.take() else {
            return;
        };
        let track = self.track(resource);
        track.skipped = true;
        track.ended = Some(now);
        track.short_since = None;
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

    /// Returns the running reclaim as of `now`, or `None` when none runs.
    #[must_use]
    pub fn reclaiming(&self, config: &Config, now: SystemTime) -> Option<Reclaiming> {
        let (resource, started) = self.running?;
        Some(Reclaiming {
            need: resource.need(config),
            since: Age::since(started, now),
        })
    }

    /// Returns what the rule remembers of `resource`.
    const fn track(&mut self, resource: Resource) -> &mut Track {
        match resource {
            Resource::Memory => &mut self.memory,
            Resource::Disk => &mut self.disk,
        }
    }
}
