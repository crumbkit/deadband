---
$type: lifecycles/entity/v1
name: ps-process
entity: One run of /bin/ps that names the busiest process outside the jobs.
stores:
- ps-process
states:
- {id: absent, kind: absent, initial: true}
- {id: running, kind: stored}
- {id: reaped, kind: absent, terminal: true}
transitions:
- id: spawn
  from: absent
  to: running
  owner: crates/deadband/src/job/busiest.rs#Census::read
  effects:
  - {store: ps-process, op: create, reads: none}
  locks: []
  unguarded:
    why: Census::read refuses a second run while its atomic flag says one runs
    against:
    - ps-process#reap
    - ps-process#spawn
- id: reap
  from: running
  to: reaped
  owner: crates/deadband/src/job/busiest.rs#Census::read
  effects:
  - {store: ps-process, op: delete, reads: none}
  locks: []
---
`Census` holds an atomic flag that refuses a second run while one has not
exited. The watch thread waits at most 5 seconds. A reaper thread of its own
reads the output, reaps `/bin/ps` and clears the flag, even after the watch
thread stopped waiting.

The reaper thread starts before `/bin/ps` and receives the child once it has
started. When the operating system refuses that thread, `Census::read` clears
the flag and returns the error, so no `/bin/ps` starts without a reaper
(job/busiest.rs).
