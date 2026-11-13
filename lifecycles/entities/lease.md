---
$type: lifecycles/entity/v1
name: lease
entity: A granted client's connection to the daemon, which its watcher thread seats again on the next
  daemon.
stores:
- lease
states:
- {id: absent, kind: absent, initial: true}
- {id: held, kind: stored}
- {id: lost, kind: stored}
- {id: ended, kind: stored, terminal: true}
transitions:
- id: grant
  from: absent
  to: held
  owner: crates/deadband/src/client/lease.rs#Lease::watched
  effects:
  - {store: lease, op: create, reads: none}
  locks: []
  unguarded:
    why: Lease::watched builds the seat before any other thread can see it, so no other transition on
      this lease can run yet
    against:
    - lease#end-held
    - lease#end-lost
    - lease#grant
    - lease#lose
    - lease#reseat
- id: lose
  from: held
  to: lost
  owner: crates/deadband/src/client/lease.rs#Seat::lose
  effects:
  - {store: lease, op: update, reads: under-lock, if_missing: fail}
  locks:
  - seat
- id: reseat
  from: lost
  to: held
  owner: crates/deadband/src/client/lease.rs#Seat::install
  effects:
  - {store: lease, op: update, reads: under-lock, if_missing: fail}
  locks:
  - seat
- id: end-held
  from: held
  to: ended
  owner: crates/deadband/src/client/lease.rs#Seat::end
  effects:
  - {store: lease, op: update, reads: under-lock, if_missing: fail}
  locks:
  - seat
- id: end-lost
  from: lost
  to: ended
  owner: crates/deadband/src/client/lease.rs#Seat::end
  effects:
  - {store: lease, op: update, reads: under-lock, if_missing: fail}
  locks:
  - seat
---
The lease is `SeatState` in the client's memory: `Held(stream)`, `Lost` or
`Ended`. The state is in memory, but its meaning is a socket connection to the
daemon. While the connection is open, the daemon counts the job. When it
closes, from either end, the daemon frees the job's place.

`grant` builds the seat without its mutex, because no other thread can see it
yet. That is no lock on a locked store, which is finding 4, and its
`unguarded` names every other transition of the lease.

`Lease::watched` starts the watcher thread only when `try_clone` of the
connection succeeds and the thread starts (client/lease.rs:99-119). When
either fails, the lease is held with no watcher, and the lease passes one line
to its notices saying that a restarted daemon will not count the job
(client/lease.rs:113-118). That lease never moves to `lost`: when the daemon
goes, nothing seats the job on the next daemon, and no stuck notice reaches
the client. The format has no state for "held with no watcher", so the run
flow records it as a failure during the lease step.

`Lease::end` takes the seat mutex once. `Seat::end` writes `Done` when the
state is `Held`, shuts the connection down, and sets `Ended`, all in one hold
(client/lease.rs:209-219). The watcher therefore cannot install a connection
between the write and the shutdown, and both end transitions read the state
under the lock that writes it. `Drop` calls `Seat::end` with no `Done`, which
finds `Ended` and changes nothing after a `Lease::end`. A lease that is
dropped without `end` shuts the connection down with no `Done`, and the daemon
records the job as lost.

`end-lost` still leaves a lost exit when the watcher has already sent
`Request::Seat` to a daemon: `install` then refuses, and that daemon reads the
close with no `Done` (see reseat and run-ends-while-lost).

Shutting the connection down also ends the watcher's blocked read.
