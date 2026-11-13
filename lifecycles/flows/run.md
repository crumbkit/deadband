---
$type: lifecycles/flow/v1
name: run
command: deadband run --label <label> -- <command>, with a daemon that answers
steps:
- {id: begin, transition: run-process#begin}
- {id: ask, transition: job-entry#ask}
- {id: grant, transition: job-entry#grant}
- {id: lease, transition: lease#grant}
- {id: lower, transition: run-process#lower}
- {id: start, transition: job-process#start}
- {id: forward, transition: job-process#forward}
- {id: reap, transition: job-process#reap}
- {id: done, transition: lease#end-held}
- {id: end, transition: job-entry#end}
- {id: evict, transition: job-entry#evict}
- {id: exit, transition: run-process#exit}
crash:
- after: ask
  leaves: 'the client died while its job waited: a waiting job-entry whose connection closed'
  recovery: the daemon's connection thread reads end of file and runs job-entry#leave
  tests:
  - test: crates/deadband/src/daemon/tests.rs#a_waiting_client_that_disconnects_leaves_the_queue_and_the_next_moves_up
    kind: kill
  leaves_state:
  - job-entry#waiting
- after: ask
  leaves: 'the daemon died while the job waited: the client reads end of file or a reset after it had
    read Queued'
  leaves_state:
  - job-entry#gone
  recovery: the client asks again every 100 ms for up to 5 seconds, so a daemon that comes back in that
    time runs job-entry#ask for it again; after 5 seconds with no daemon that keeps it, the client starts
    the job at once with no lease (the run-open flow)
  tests:
  - test: crates/deadband/src/client/tests.rs#a_queued_client_asks_a_restarted_daemon_again_before_it_starts_the_job
    kind: kill
  - test: crates/deadband/src/client/tests.rs#a_queued_client_starts_the_job_when_no_daemon_returns_within_the_bound
    kind: kill
- during: grant
  leaves: a running job-entry whose client cannot take the grant within the 1 second write timeout
  leaves_state:
  - job-entry#running
  recovery: deliver runs job-entry#undelivered under the queue mutex, records the job once as left, grants
    again, and then shuts the connection down; the client starts the job at once if it had read no Queued
    line, and asks again if it had
  tests:
  - test: crates/deadband/src/daemon/tests/reply.rs#a_grant_that_cannot_be_written_frees_its_place_once_and_closes_the_connection
    kind: seam
- during: lease
  leaves: 'try_clone or the watcher thread failed: a held lease that nothing watches (client/lease.rs:99-119)'
  leaves_state:
  - lease#held
  recovery: none; the lease tells its notices once that a restarted daemon will not count the job, the
    job runs, and if the daemon goes it is never seated on the next daemon and no stuck notice reaches
    it
  tests:
  - test: crates/deadband/src/client/lease/tests.rs#a_lease_that_cannot_be_watched_says_so_and_the_job_still_ends_with_the_daemon
    kind: seam
- after: start
  leaves: 'deadband run died from SIGKILL while its job runs: the job keeps running with no lease'
  recovery: none; the daemon reads end of file and runs job-entry#end with the exit lost, so the job runs
    uncounted
  tests:
  - test: crates/deadband/src/daemon/tests.rs#a_job_whose_client_closes_while_it_runs_is_recorded_as_lost_with_no_usage
    kind: kill
  leaves_state:
  - job-entry#running
- after: start
  leaves: 'the daemon died while the job runs: the lease''s connection closed'
  recovery: the lease's watcher runs the reseat flow
  tests:
  - test: crates/deadband/src/client/tests.rs#a_lease_seats_again_with_its_job_and_threads_after_the_connection_closes
    kind: kill
  leaves_state:
  - lease#lost
---
`admission::admit` asks the daemon, waits up to 2 seconds for the first
answer, and then waits for the grant with no deadline while the daemon keeps
the connection open. The flow is also the library call `admission::admit`
followed by the caller's own start. A program that cannot start still sends
`Done` with no exit code, so the flow skips start and reap.

The daemon decides `Queued` and `Granted` under the queue mutex and writes
them after it releases the mutex (daemon/reply.rs). A `Granted` reply can
therefore reach the client before its `Queued` reply, when another thread
grants the job between the asking thread's release and its write. The client
then holds its lease, and the lease's watcher ignores the late `Queued` line.

The evict step drops the oldest ended job when this one is the 65th, not this
job's own entry.

The first two crash points are both `after: ask`, because one is the client
dying and the other the daemon dying. A queued client whose connection closes
asks again for 5 seconds (client.rs `REASK_FOR`). After a daemon restart,
such as `deadband service install --restart`, the new daemon therefore queues
the waiting jobs again. Only a queue that finds no daemon for those 5 seconds
starts together.

`deadband run` passes a SIGTERM or SIGINT that its caller sends to the job
unchanged and keeps waiting (job/forward.rs, the forward step, which runs
only when such a signal arrives), so the job decides how to end,
the done step runs, and the daemon records the job's real exit. Only a
SIGKILL of `deadband run` alone leaves the job running uncounted, which is
the `after: start` crash point.
