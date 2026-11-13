---
$type: lifecycles/flow/v1
name: watch
command: the daemon's watch thread, every 15 seconds while a watched job runs
steps:
- {id: track, transition: job-entry#track}
- {id: stuck, transition: job-entry#become-stuck}
- {id: ps, transition: ps-process#spawn}
- {id: ps-reap, transition: ps-process#reap}
- {id: hook, transition: on-stuck-process#spawn}
- {id: hook-reap, transition: on-stuck-process#reap}
- {id: active, transition: job-entry#become-active}
crash:
- during: ps-reap
  leaves: /bin/ps still runs after the 5 second wait, with the census flag set
  recovery: the reaper thread clears the flag when /bin/ps exits; until then each census is refused
- during: ps
  leaves: the operating system refused the thread that waits for /bin/ps
  recovery: Census::read starts that thread before /bin/ps and clears the census flag when it is refused,
    so no /bin/ps runs unreaped and the next census can run
  tests:
  - test: crates/deadband/src/job/tests.rs#a_census_whose_thread_cannot_start_names_no_process_and_the_next_run_reads
    kind: seam
- after: hook
  leaves: the daemon died while /bin/ps or an on_stuck command runs
  recovery: under launchd, launchd kills the process with the daemon's process group; a daemon started
    by hand leaves it running as an orphan that nothing waits for; the next daemon's census flag and on_stuck
    count start at zero
---
The thread copies the watched jobs under the queue mutex, reads their process
trees through the probe with the mutex released, applies the stuck rule under
the mutex, and writes each notice to the job's connection with the mutex
released. The ps step runs when a job becomes stuck and then at most once a
minute while one stays stuck. The track step runs on a job's first reading. The hook step runs when the config names
`on_stuck`. The thread never signals or changes a job.

All stuck records are in memory. A daemon end loses them, and the reseat flow
starts each watched job again in watching.

A refused watch thread is one log line, and the daemon then flags no job as
stuck. A refused `on_stuck` thread is one log line and frees its place among
the 8 (daemon/watch/hook.rs).
