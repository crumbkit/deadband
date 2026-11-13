---
$type: lifecycles/entity/v1
name: run-process
entity: The deadband run process, and the nice value that its job inherits.
stores:
- run-process
states:
- {id: absent, kind: absent, initial: true}
- {id: running, kind: stored}
- {id: gone, kind: absent, terminal: true}
transitions:
- id: begin
  from: absent
  to: running
  owner: crates/deadband/src/main.rs#main
  effects: []
  locks: []
- id: exit
  from: running
  to: gone
  owner: crates/deadband/src/cli/commands.rs#run
  effects: []
  locks: []
regions:
- id: priority
  exists_in:
  - running
  states:
  - {id: caller-nice, kind: stored, initial: true}
  - {id: lowered, kind: stored, terminal: true}
  transitions:
  - id: lower
    from: caller-nice
    to: lowered
    owner: crates/deadband/src/throttle.rs#lower_own_priority
    effects:
    - {store: run-process, op: update, reads: before-lock, if_missing: fail}
    locks: []
    unguarded:
      why: a deadband run process lowers its own nice value once, on its main thread
      against:
      - run-process#lower
---
The store's kind is `process`. The `running` state is marked `stored`,
because state kinds are absent, stored and inferred; the process exists while
the kernel holds it.

`lower` sets this process's own nice value to 5 for a normal-lane job. When
the value is already 5 or higher, it leaves it unchanged (throttle.rs:31), so
it never raises the priority. The state `lowered` therefore means "nice 5 or
higher", and the transition may change nothing. It runs whether or not
the daemon granted the job, and not at all when `DEADBAND_HELD` or
`DEADBAND_BYPASS` is set. Nothing ever restores the value: it ends with the
process. The job process inherits it at `fork`. The format cannot say that one
entity's state is copied into another's when the second is created, so
job-process has no priority region of its own. This is a bend in the index.

`lower` reads the current value with `getpriority` and then sets it, with no
lock, which is `reads: before-lock`.

throttle is the only module that changes a priority, and clippy's
`disallowed-methods` refuses `setpriority` elsewhere. job/forward.rs is the
only module that sends a signal, and it sends only a SIGTERM or SIGINT that
`deadband run` received (see job-process).
