---
$type: lifecycles/entity/v1
name: on-stuck-process
entity: One run of the on_stuck command that config.toml names, for a job that became stuck.
stores:
- on-stuck-process
states:
- {id: absent, kind: absent, initial: true}
- {id: running, kind: stored}
- {id: reaped, kind: absent, terminal: true}
transitions:
- id: spawn
  from: absent
  to: running
  owner: crates/deadband/src/job/on_stuck.rs#run
  effects:
  - {store: on-stuck-process, op: create, reads: none}
  locks: []
  unguarded:
    why: up to 8 on_stuck commands run at once by design, each its own process; an atomic counter caps
      them
    against:
    - on-stuck-process#reap
    - on-stuck-process#spawn
- id: reap
  from: running
  to: reaped
  owner: crates/deadband/src/job/on_stuck.rs#run
  effects:
  - {store: on-stuck-process, op: delete, reads: none}
  locks: []
---
At most 8 run at once; an atomic counter in `Hooks` refuses a ninth and the log
says so. The command reads one JSON object on standard input. Past its
deadline the log says so, and deadband keeps waiting and never signals it.

When the operating system refuses the thread that runs the command, the log
says so in one line and the counter gives the place back
(daemon/watch/hook.rs).
