---
$type: lifecycles/entity/v1
name: daemon
entity: The daemon's socket file, and whether a daemon listens on it.
stores:
- socket
states:
- {id: absent, kind: absent, initial: true}
- {id: listening, kind: inferred, inferred_by: crates/deadband/src/client.rs#connect}
- {id: orphaned, kind: stored}
- {id: stale, kind: stored}
transitions:
- id: bind
  from: absent
  to: listening
  owner: crates/deadband/src/daemon/bind.rs#bind_private
  effects:
  - {store: socket, op: create, reads: under-lock}
  locks:
  - socket
- id: rebind
  from: stale
  to: listening
  owner: crates/deadband/src/daemon/bind.rs#bind_private
  effects:
  - {store: socket, op: delete, reads: under-lock}
  - {store: socket, op: create, reads: under-lock}
  locks:
  - socket
- id: stop
  from: listening
  to: stale
  owner: crates/deadband/src/serve.rs#run_daemon
  effects: []
  locks:
  - socket
- id: killed
  from: listening
  to: stale
  owner: outside:signal
  effects: []
  locks: []
- id: unlinked
  from: listening
  to: orphaned
  owner: outside:person
  locks: []
  effects:
  - {store: socket, op: delete, reads: none}
  unguarded:
    why: something outside deadband removed the socket file; clients see no daemon and run every job at
      once until the daemon's next check of its files, within 60 samples, stops it, and the live daemon's
      lock keeps a new daemon from binding until then
    against:
    - daemon#bind
    - daemon#rebind
    - daemon#usurp
    test: crates/deadband/src/daemon/tests/alive.rs#a_daemon_stops_serving_when_its_socket_file_is_removed
- id: usurp
  from: listening
  to: listening
  owner: crates/deadband/src/daemon/bind.rs#bind_private
  locks:
  - socket
  effects:
  - {store: socket, op: delete, reads: under-lock}
  - {store: socket, op: create, reads: under-lock}
- id: end-orphaned
  from: orphaned
  to: absent
  owner: crates/deadband/src/daemon/alive.rs#Ending::check
  locks: []
  effects: []
---
The daemon takes an exclusive `flock` on `<socket>.lock` without waiting, and
only then looks at the socket path. A socket file there is removed and bound
again (`rebind`). Anything else at the path is refused with
`deadband::socket::unusable`, and a held lock is refused with
`deadband::daemon::running`. `deadband daemon` exits with status 0 on that
refusal and nonzero on every other end, so launchd's `KeepAlive`, which
restarts only an unclean exit, leaves a refused daemon stopped. The umask clears every group and other bit while
the socket and the lock file are made.

`stop` is the daemon's own return. `serve` returns, `run_daemon` drops the
`Bound`, which releases the lock, and the process exits. `serve` returns for
any of these reasons (daemon.rs:203-283, daemon/alive.rs):

- accepting fails with an error that another try cannot fix;
- the sampler's check, once every 60 samples, or the watchdog's check while
  the reading is stale, finds the socket file or the lock file gone or
  replaced, compared by device and inode;
- in a build that unwinds, such as a test build, the sampler panics, which
  drops its `Ending` and sends the reason;
- the operating system refuses the sampler, the watchdog or a connection's
  thread. A refused connection thread also drops that connection, so its
  client starts its job.

`killed` is any other end: the SIGTERM that `launchctl bootout` sends,
SIGKILL, or, in a released daemon, a panic in any thread. The release profile sets `panic = "abort"`
(Cargo.toml:13-18), so a released daemon aborts at a panic without returning
from `serve` or dropping `Bound`; the kernel releases the lock when the
process ends. The daemon has no signal handler and never removes its socket file,
so both ends leave the file in place with nothing listening. Neither end
writes the socket store, so both transitions have no effects. The format has
no effect for "the listener closed", which is the change that matters here.

A client treats a socket file with no listener as no daemon
(`ConnectionRefused`) and runs its job at once.

Two changes come from outside deadband:

- `unlinked`: something removes the socket file while the daemon still
  listens, which leaves `orphaned`. Clients then see `NotFound`, count it as
  no daemon and run every job at once. The daemon keeps its lock, so a second
  daemon refuses to start. Within 60 samples, about a minute, the daemon's
  check finds the file gone and stops (`end-orphaned`), and launchd's
  `KeepAlive` starts a daemon that runs `bind`. `unlinked` writes the socket
  store without the socket lock, which is finding 4, and its `unguarded`
  names every partner.
- `usurp`: something removes the lock file while the daemon runs. A second
  daemon then takes a lock on a new file, sees the first daemon's socket,
  removes it and binds its own. Both daemons run, each counting only its own
  jobs, until the first daemon's next check finds its socket file and its
  lock file replaced and stops it, within 60 samples. The removal itself has
  no transition, because the lock file is not a store: the socket's directory
  is `$TMPDIR` or the per-user temporary directory, which deadband does not
  own. The `socket` lock still guards `usurp`, but on a different file from
  the one the first daemon holds.

The daemon names its lock file by a stat of the lock path when `serve`
starts, after `bind` took the lock (daemon/alive.rs:177-193). A lock file
replaced between `bind` and that stat is recorded as the daemon's own, and
the daemon never notices that replacement. deadband keeps this as a soft
limit.

While the sampler stores no reading for five sampling intervals, and at least
250 ms, the watchdog marks the reading stale, and admission treats the probe
as failed, so each job starts once a place is free. The daemon keeps serving
in that case. The format has no state for "serving on a stale reading", and
it is a field of daemon-state, so it is not drawn.
