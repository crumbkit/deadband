---
$type: lifecycles/entity/v1
name: job-process
entity: The job that deadband run starts as its child.
stores:
- job-process
states:
- {id: absent, kind: absent, initial: true}
- {id: running, kind: stored}
- {id: reaped, kind: absent, terminal: true}
transitions:
- id: start
  from: absent
  to: running
  owner: crates/deadband/src/job/forward.rs#run_forwarding
  effects:
  - {store: job-process, op: create, reads: none}
  locks: []
  unguarded:
    why: each deadband run starts its own job process, and run_forwarding starts the forwarding thread
      only after the spawn returns, so no other transition can reach the process before it exists
    against:
    - job-process#forward
    - job-process#reap
    - job-process#start
- id: forward
  from: running
  to: running
  owner: crates/deadband/src/job/forward.rs#forward
  locks:
  - live
  effects:
  - {store: job-process, op: update, reads: under-lock, if_missing: fail}
- id: reap
  from: running
  to: reaped
  owner: crates/deadband/src/job/forward.rs#run_forwarding
  effects:
  - {store: job-process, op: delete, reads: none}
  locks:
  - live
---
`job::run` calls `forward::run_forwarding`, which starts the job, waits for it
and returns its exit status. The job inherits deadband's environment, its
streams and its nice value, plus each parallelism variable that the caller
did not set. Only a job that the daemon granted also gets `DEADBAND_HELD=1`
(environment.rs:200-202); a job that started open does not, so a deadband run
inside it asks the daemon again.

`forward` passes a SIGTERM or SIGINT that `deadband run` receives on to the
job unchanged, and sends no other signal. The signals are registered before
the job starts. The main thread waits for the job to exit without reaping it
(`waitid` with `NOWAIT`), sets the `live` flag to false under its mutex, and
only then reaps the job (job/forward.rs:66-77). `forward` sends only while it
holds that mutex and finds the flag true (job/forward.rs:81-95). The reap
transition is the hold that clears the flag; the reap itself follows it, and
no signal can reach a reused process ID.

A refused forwarding thread or a failed registration runs the job without
forwarding. A Ctrl-C at the terminal reaches both deadband and the job,
because they share a process group, so the job receives that SIGINT twice.
When `deadband run` itself dies from SIGKILL, the job keeps running with no
parent that reports to the daemon.

Pausing is planned and not built. A paused job would need a region such as
`run-state` with `running` and `stopped`, and the never-kill control needs the
next daemon to resume a job that a crashed daemon paused. That needs a store
that outlives the daemon and names the paused process IDs. Nothing like it
exists yet, and no test shows a resume after a daemon crash.
