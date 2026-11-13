---
$type: lifecycles/flow/v1
name: reseat
command: the watcher thread of a granted deadband run, after the daemon's connection closes
steps:
- {id: lose, transition: lease#lose}
- {id: seat, transition: job-entry#seat}
- {id: install, transition: lease#reseat}
crash:
- after: lose
  leaves: a running job that no daemon counts
  recovery: the watcher tries every 100 ms to connect and send Request::Seat, for as long as the job runs
  tests:
  - {test: crates/deadband/src/client/tests.rs#a_lease_tries_again_when_a_daemon_closes_the_seat, kind: kill}
  leaves_state:
  - lease#lost
- after: seat
  leaves: the Seat write failed or timed out inside Seat::install, which closes the connection and leaves
    the seat lost; the new daemon may have counted a job that its close then frees
  recovery: the watcher looks again after 100 ms
  tests:
  - {test: crates/deadband/src/client/lease/tests.rs#a_seat_write_that_fails_leaves_the_seat_lost,
    kind: seam}
  leaves_state:
  - lease#lost
---
The seat step runs in the new daemon. The old connection closes before the new
one opens, so the old daemon has freed the place first. The new daemon gives
the job a fresh stuck timer and no record of how long it waited. Its history
line, when it ends, marks it as seated, counts its run from when it started
on the old daemon (`ran_ms`), counts it as starting when it asked, and has no
warm directories or load at its start.

`Seat::install` holds the seat mutex while it checks for `Ended`, writes
`Request::Seat` and sets `Held`, so the seat step and the install are one hold
of the lock (client/lease.rs:248-255). `Seat::end` runs either before it, and
no `Seat` is sent and no daemon hears of the job, or after it, and `Done`
reaches the new connection, so the new daemon records the real exit. The lock
is held for its `Seat` write, which is bounded by `WRITE_WITHIN` (2 s) for each send, and only during a reseat.
`Seat::end` takes the seat mutex once, so no install can come between its
write and its shutdown.
