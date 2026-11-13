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
  owner: crates/deadband/src/daemon.rs#make_state_dir
  effects:
  - {store: state-dir, op: create, reads: none}
  locks: []
  unguarded:
    why: the recursive DirBuilder succeeds when the directory exists, and every call sets mode 0700, so
      two daemons on different sockets, a reclaim and a service command end alike
    against:
    - state-dir#make
    - state-dir#set-mode
    test: crates/deadband/src/reclaim/lock.rs#a_reclaim_that_makes_the_state_directory_makes_it_private
- id: set-mode
  from: present
  to: present
  owner: crates/deadband/src/daemon.rs#make_state_dir
  effects:
  - {store: state-dir, op: update, reads: none, if_missing: recreate}
  locks: []
  unguarded:
    why: every call sets mode 0700, so the order does not change the result
    against:
    - state-dir#make
    - state-dir#set-mode
    test: crates/deadband/src/reclaim/lock.rs#a_reclaim_makes_an_open_state_directory_private
---
One function, `make_state_dir` (daemon.rs:389-397), makes the directory with
mode 0700 when it is missing and sets mode 0700 when it exists. Every maker in
deadband calls it, so whichever maker runs first leaves the directory at 0700
and a mode the person changed is reset by the next call. The makers are these:

- The history writer calls it before it takes `jobs.lock` (`claim`,
  history/writer.rs:274), because the lock file lives in the directory, and
  again on every open under the lock (history/writer.rs:354).
- The reclaim thread calls it before it takes `reclaim.lock`
  (reclaim/lock.rs:32). When the call fails, the reclaim runs without the
  lock, as it does when the lock file cannot be made.
- The service commands call it, on `$HOME/.local/state/deadband` whatever
  `XDG_STATE_HOME` says, before they take `service.lock`
  (cli/service/lock.rs:40). That is the directory of the daemon that launchd
  starts, since the plist sets no `XDG_STATE_HOME`.

The history writer makes the directory when its thread starts
(history/writer.rs:188-191), before the first sample (daemon.rs:211-214, 227),
so a reclaim makes it first only after something outside deadband removed the
directory, or when the history thread was refused. In both cases the reclaim
leaves it at 0700. deadband never removes the directory. The first call takes
no lock, so every transition says `locks: []`, although the writer's later
calls run while it holds `jobs.lock`.
