---
$type: lifecycles/flow/v1
name: run-abandoned
command: deadband run that is interrupted while its job waits
steps:
- {id: begin, transition: run-process#begin}
- {id: ask, transition: job-entry#ask}
- {id: leave, transition: job-entry#leave}
- {id: exit, transition: run-process#exit}
crash: []
---
A Ctrl-C while the job waits ends `deadband run` before the job starts. The
daemon reads end of file on the connection and removes the waiting entry. The
history records the job as left, with the jobs that were ahead of it.
