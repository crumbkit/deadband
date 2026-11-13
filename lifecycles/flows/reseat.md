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
  leaves: the job ended between the seat and the install, so the new daemon counts a job that has ended
  recovery: Seat::install refuses and closes the connection, and the new daemon runs job-entry#end with
    the exit lost
  tests:
  - {test: crates/deadband/src/client/tests.rs#a_connection_that_opens_after_the_job_ended_is_closed,
    kind: seam}
---
The seat step runs in the new daemon. The old connection closes before the new
one opens, so the old daemon has freed the place first. The new daemon gives
the job a fresh stuck timer and no record of how long it waited. Its history
line, when it ends, marks it as seated, counts its run from when it started
on the old daemon (`ran_ms`), counts it as starting when it asked, and has no
warm directories or load at its start.

The `after: seat` crash point is the order where the job ends before the
install. When the install lands first, `Seat::end` writes `Done` on the new
connection and the new daemon records the real exit. `Seat::end` takes the
seat mutex once, so no install can come between its write and its shutdown.
