---
$type: lifecycles/flow/v1
name: sample
command: the daemon's sampler thread, once a second
steps:
- {id: check, transition: daemon#stop}
- {id: grant, transition: job-entry#grant}
- {id: undelivered, transition: job-entry#undelivered}
- {id: reclaim, transition: reclaim#start}
crash:
- during: reclaim
  leaves: the operating system refused the reclaim thread after reclaim#start set the mark
  recovery: start_reclaim runs reclaim#skip, so the mark clears and the resource rests reclaim_after (the
    reclaim-skipped flow)
- after: grant
  leaves: 'the sampler stalled: the reading it stored ages past five sampling intervals'
  recovery: the watchdog counts the reading as stale, so admission treats the probe as failed and each
    job starts once a place is free, and the watchdog keeps checking the socket and lock files
  tests:
  - test: crates/deadband/src/daemon/tests/alive.rs#a_job_starts_when_the_sampler_stalls_and_its_last_reading_holds_the_job
    kind: seam
---
The sampler first counts one sample, and on every 60th checks the socket file
and the lock file; when either is gone or replaced it stops the daemon (the
check step, daemon#stop or daemon#end-orphaned). It then reads the probe and
the priority file with the queue mutex released, takes the mutex to store
the reading, decides every grant that admission now allows, hands a machine
line to the history writer, and decides whether to reclaim. It writes the
grants after it releases the mutex, and a grant that cannot be written runs
undelivered. The connection threads also run grant and undelivered whenever a
job asks or ends. The steps are the transitions the sampler may run in this
order; most ticks run none of them.
