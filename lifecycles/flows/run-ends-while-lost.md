---
$type: lifecycles/flow/v1
name: run-ends-while-lost
command: a granted deadband run whose job ends while no daemon holds its seat
steps:
- {id: end, transition: lease#end-lost}
crash: []
---
The job's thread ends the lease while the watcher looks for a daemon, so
`Seat::end` finds `Lost` and writes no `Done`. When the lease ends while the
watcher waits in `ended_after_wait` (client/lease.rs:330), the watcher sees
the end at once and stops, no daemon hears of the job's end, and no history
line is written for it. When a daemon accepts the watcher's connection first,
the watcher calls `Seat::install`, which holds the seat mutex while it checks
`Ended`, writes `Request::Seat` and sets `Held` (client/lease.rs:248-255). When
the end comes first, `install` refuses, writes no `Seat` and closes the
connection, so no daemon hears of the job. When the end comes after, it is
`end-held` and writes `Done` on the new connection. When the connect or the
write fails, `try_seat` returns `None`, `seat_again` loops back to the check
(client/lease.rs:333-341), finds `Ended`, and no history line is written. No
window leaves a job recorded with its exit lost.
