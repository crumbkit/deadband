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
line is written for it. When the watcher has already passed that check and a
daemon accepts its connection, it writes `Request::Seat`
(client/lease.rs:343-350), and `install` then refuses (client/lease.rs:245-246). The daemon it seated reads
the close with no `Done` and writes a history line for the job with its exit
lost (daemon/seat.rs:171-183); see reseat. That window is about one connect
long, so the second case is rare. When the connect or the write
fails, `try_seat` returns `None`, `seat_again` loops back to the check
(client/lease.rs:333-334), finds `Ended`, and no history line is written.
