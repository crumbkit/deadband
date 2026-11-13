---
$type: lifecycles/entity/v1
name: launchd-tool-process
entity: One run of /bin/launchctl by deadband service or deadband status.
stores:
- launchd-tool-process
states:
- {id: absent, kind: absent, initial: true}
- {id: running, kind: stored}
- {id: reaped, kind: absent, terminal: true}
transitions:
- id: spawn
  from: absent
  to: running
  owner: crates/deadband/src/job/launchd.rs#output
  effects:
  - {store: launchd-tool-process, op: create, reads: none}
  locks: []
  unguarded:
    why: each call of output starts its own launchctl process
    against:
    - launchd-tool-process#reap
    - launchd-tool-process#spawn
- id: reap
  from: running
  to: reaped
  owner: crates/deadband/src/job/launchd.rs#output
  effects:
  - {store: launchd-tool-process, op: delete, reads: none}
  locks: []
---
`output` runs the program with `Command::output`, which waits for it and reaps
it, with no deadline. deadband never signals it. A hung `launchctl` holds the
service command until the person interrupts it.

The per-user temporary directory, which the plist passes to the daemon as
`TMPDIR`, is read with `confstr`, which starts no process.
