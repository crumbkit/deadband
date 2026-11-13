---
$type: lifecycles/entity/v1
name: state-dir
entity: deadband's state directory, $XDG_STATE_HOME/deadband, and its mode 0700.
stores:
- state-dir
states:
- {id: absent, kind: absent, initial: true}
- {id: present, kind: stored, terminal: true}
transitions:
- id: make
  from: absent
  to: present
  owner: crates/deadband/src/history/writer.rs#make_directory
  effects:
  - {store: state-dir, op: create, reads: none}
  locks: []
  unguarded:
    why: the recursive DirBuilder succeeds when the directory exists, and every call sets mode 0700, so
      two daemons on different sockets end alike
    against:
    - state-dir#make
    - state-dir#make-open
    - state-dir#set-mode
- id: make-open
  from: absent
  to: present
  owner: crates/deadband/src/reclaim/lock.rs#ReclaimLock::take
  locks: []
  effects:
  - {store: state-dir, op: create, reads: none}
  unguarded:
    why: create_dir_all succeeds when the directory exists and sets no mode, so the order with the other
      makes changes only the mode a new directory gets until the next set-mode
    against:
    - state-dir#make
    - state-dir#make-open
    - state-dir#set-mode
- id: set-mode
  from: present
  to: present
  owner: crates/deadband/src/history/writer.rs#make_directory
  effects:
  - {store: state-dir, op: update, reads: none, if_missing: recreate}
  locks: []
  unguarded:
    why: every call sets mode 0700, so the order does not change the result
    against:
    - state-dir#make
    - state-dir#make-open
    - state-dir#set-mode
---
The writer calls `make_directory` before it takes `jobs.lock` (`claim`,
history/writer.rs:279), because the lock file lives in the directory, and
again on every open under the lock (history/writer.rs:348). Each call makes
the directory with mode 0700 when it is missing and sets mode 0700 when it
exists, so a mode the person changed is reset on the next open. The service
commands also make it with mode 0700 before they take `service.lock`
(cli/service/lock.rs:45-52), which is the same `make`.

`make-open` is the reclaim thread's `create_dir_all` before it takes
`reclaim.lock` (reclaim/lock.rs:30). It sets no mode. The history writer makes the
directory with 0700 when its thread starts (history/writer.rs:193-196), before
the first sample (daemon.rs:211-214, 227), so a reclaim makes it first only
after something outside deadband removed the directory, or when the history
thread was refused. After a removal, the writer's next open sets 0700 again.
When the history thread was refused, nothing in that daemon sets 0700, and the
directory keeps the mode the daemon's umask allows, usually 0755, until another daemon's writer, such as the writer of the daemon that launchd
starts next, runs `make_directory` on it. A service command does not change
the mode of a directory that exists (cli/service/lock.rs:45-49); only the
writer's `make_directory` sets 0700 on one (history/writer.rs:378-386). deadband never
removes the directory. The first call takes no lock, so every transition says
`locks: []`, although the writer's later calls run while it holds
`jobs.lock`.
