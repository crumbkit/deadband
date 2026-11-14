---
$type: lifecycles/entity/v1
name: history-lock
entity: The lock file jobs.lock in the state directory, which the history writer locks for the daemon's
  life.
stores:
- history-lock-file
states:
- {id: absent, kind: absent, initial: true}
- {id: present, kind: stored}
transitions:
- id: make
  from: absent
  to: present
  owner: crates/deadband/src/history/writer.rs#Writer::claim
  locks: []
  effects:
  - {store: history-lock-file, op: create, reads: none}
  unguarded:
    why: take_lock opens jobs.lock with create(true), not create_new, before it takes the flock, so two
      writers on one state directory may both make it; they then open the same file and the flock decides.
      A writer that finds its lock file gone or replaced releases its lock and claims again, so after
      a removal the flock on the new file decides again
    against:
    - history-lock#make
    - history-lock#unlinked
- id: unlinked
  from: present
  to: absent
  owner: outside:person
  locks: []
  effects:
  - {store: history-lock-file, op: delete, reads: none}
---
`Writer::claim` calls `take_lock` (daemon/bind.rs:129-148), which opens
`jobs.lock` with mode 0600, creating it when it is missing, and takes an
exclusive flock without waiting. deadband never removes the file.

Before each line, `claim` checks by device and inode that the file it locked
is still the file at `jobs.lock` (history/writer.rs:264-273). When something
outside deadband has removed or replaced it (`unlinked`), the writer releases
its lock and closes the history file, then takes a lock on the file now at the
path, making it when it is missing (`make`). When a second writer on the same
state directory took the new lock first, this writer records nothing and logs
one line, and the second writer is the only one. A second writer is a daemon
that records history on another socket: one started with
`DEADBAND_SOCKET_DIR` set, or on Linux one whose `TMPDIR` or `XDG_RUNTIME_DIR` differs,
with the same `XDG_STATE_HOME` or `HOME`. A daemon started with
`DEADBAND_SOCKET` set starts no history writer (environment.rs:176-181).

The check and the line's write are separate, so a writer that passed its
check just before the removal writes that one line while the second writer
may already hold the new lock. Finding 5 reports the removal on `unlinked`,
because the file it deletes holds the `history` lock.
