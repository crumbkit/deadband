---
$type: lifecycles/entity/v1
name: reclaim
entity: The daemon's mark that a reclaim runs, and the last reclaim that deadband status reports.
stores:
- daemon-state
states:
- {id: idle, kind: stored, initial: true}
- {id: running, kind: stored}
transitions:
- id: start
  from: idle
  to: running
  owner: crates/deadband/src/reclaim/watch.rs#Watch::observe
  effects:
  - {store: daemon-state, op: update, reads: under-lock, if_missing: fail}
  locks:
  - queue
  unchecked_reads:
  - the machine reading, taken with the queue mutex released and stored under it before the decision (daemon.rs:304-311)
- id: report
  from: running
  to: running
  owner: crates/deadband/src/daemon.rs#start_reclaim
  effects:
  - {store: daemon-state, op: update, reads: under-lock, if_missing: fail}
  locks:
  - queue
  - reclaim
- id: skip
  from: running
  to: idle
  owner: crates/deadband/src/reclaim/watch.rs#Watch::skipped
  locks:
  - queue
  effects:
  - {store: daemon-state, op: update, reads: under-lock, if_missing: fail}
- id: finish
  from: running
  to: idle
  owner: crates/deadband/src/reclaim/watch.rs#Watch::ended
  effects:
  - {store: daemon-state, op: update, reads: under-lock, if_missing: fail}
  locks:
  - queue
  - reclaim
---
`Watch` keeps one `running` mark, so at most one reclaim runs in one daemon.
The sampler sets it in `observe` under the queue mutex and then starts the
reclaim thread. That thread records each reclaimer's outcome (`report`) and
clears the mark (`finish`) when the reclaim ends. `deadband status` shows the
mark as `reclaiming: <need>, started <age> ago`.

The reclaim thread first takes `reclaim.lock` in the state directory without
waiting (reclaim/lock.rs), so two daemons of one user never reclaim at once.
When another daemon holds it, or when the operating system refuses the
reclaim thread, the mark is cleared at once (`skip`), and that resource rests
exactly `reclaim_after`, with its count of fruitless reclaims unchanged
(daemon.rs:342-376). A lock file that cannot be made or locked, or a state directory that cannot
be made or whose mode cannot be set, fails open, and the reclaim runs without
the lock. A daemon started with `DEADBAND_SOCKET` set has
no reclaimers, so it never sets the mark.

The mark is in memory, and the lock is held by the daemon's own descriptor,
which a reclaimer does not inherit. A daemon that ends while a reclaimer runs
loses both. Under launchd, launchd then kills the reclaimer with the rest of
the daemon's process group, because the plist sets no `AbandonProcessGroup`,
so the reclaimer stops halfway. A daemon started by hand leaves the reclaimer
running, and the next daemon can take the lock and start a second reclaim
beside it. The launchd part rests on launchd.plist(5) and is not tested.

The release profile sets `panic = "abort"` (Cargo.toml:13-18), so a panic in
the reclaim thread ends the whole daemon, as any other daemon death does. Only
a build that unwinds, such as a test or debug build, leaves the mark set after
a panic, because `finish` never runs; the daemon then reclaims nothing for
the rest of its life.

Each resource also keeps its own back-off (`short_since`, `ended`,
`fruitless`, `skipped`) in the same `Watch`. Those fields change on every
sample and are not drawn as states.
