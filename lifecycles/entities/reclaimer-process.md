---
$type: lifecycles/entity/v1
name: reclaimer-process
entity: One run of a reclaimer that config.toml names, as a dry run or a real run.
stores:
- reclaimer-process
states:
- {id: absent, kind: absent, initial: true}
- {id: running, kind: stored}
- {id: reaped, kind: absent, terminal: true}
transitions:
- id: spawn
  from: absent
  to: running
  owner: crates/deadband/src/job/reclaimer.rs#run
  effects:
  - {store: reclaimer-process, op: create, reads: none}
  locks:
  - reclaim
  unguarded:
    why: one reclaim thread runs reclaimers one at a time, because Watch keeps one reclaim mark and the
      thread holds reclaim.lock; a daemon restart breaks this (see the reclaim-run flow)
    against:
    - reclaimer-process#reap
    - reclaimer-process#spawn
- id: reap
  from: running
  to: reaped
  owner: crates/deadband/src/job/reclaimer.rs#run
  effects:
  - {store: reclaimer-process, op: delete, reads: none}
  locks:
  - reclaim
---
`run` starts a reading thread and a waiting thread before it starts the
reclaimer, so a refused thread leaves no reclaimer unreaped. The reading
thread keeps the first 1 MiB of the reclaimer's output and reads the rest to
its end. The waiting thread reaps the reclaimer, then waits at most 250 ms for
the output, and the run ends. A background process that still holds the
reclaimer's standard output keeps the reading thread and its pipe until it
closes it, and the reclaim does not wait for it (job/reclaimer.rs:94-170).

Past the config's timeout the log says so in one line, and the reclaim thread
keeps waiting until the reclaimer exits. deadband never signals a reclaimer.
At daemon start the log names each reclaimer whose program cannot start, and
`deadband status` names it after a failed reclaim.

When the daemon ends while a reclaimer runs under launchd, launchd kills the
reclaimer with the rest of the daemon's process group, because the reclaimer
stays in that group (job/reclaimer.rs:146-152) and the plist sets no
`AbandonProcessGroup`. The reclaimer then stops halfway, which deadband itself
never does. When the daemon was started by hand, the reclaimer keeps running
as an orphan, and nothing reaps it for deadband or knows it runs. The launchd
part rests on launchd.plist(5) and is not tested.
