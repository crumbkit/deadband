---
$type: lifecycles/flow/v1
name: run-open
command: deadband run, when no daemon answers within 2 seconds, or no daemon keeps a queued job within
  5 seconds of its connection closing
steps:
- {id: begin, transition: run-process#begin}
- {id: lower, transition: run-process#lower}
- {id: start, transition: job-process#start}
- {id: reap, transition: job-process#reap}
- {id: exit, transition: run-process#exit}
crash:
- {after: start, leaves: deadband run died from SIGKILL and the job keeps running, recovery: none}
---
This is the fail-open path. A missing socket, a socket with no listener, a
daemon that does not answer in 2 seconds, one that closes the connection
before it queues the job, or one whose answer cannot be read all give
`Admission::Open`. A client whose connection closes after `Queued` asks again
every 100 ms, and gives `Admission::Open` only when no daemon has kept the job
for 5 seconds after the first close (client.rs:155-195). The job starts at
once with the cores divided by the default number of jobs, and standard error
says why in one line. The job never gets a lease, so a daemon that starts later
does not count it.

The 2 second limit does not cover sending the request. A daemon that has
stopped reading blocks the client when the command is longer than the
socket's send buffer, about 8 KiB on macOS.

With `DEADBAND_HELD` or `DEADBAND_BYPASS` set, the lower step does not run.
