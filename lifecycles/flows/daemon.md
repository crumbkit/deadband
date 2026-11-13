---
$type: lifecycles/flow/v1
name: daemon
command: deadband daemon
steps:
- id: bind
  one_of:
  - {transition: daemon#bind}
  - {transition: daemon#rebind}
  - {transition: daemon#usurp}
- id: end
  one_of:
  - {transition: daemon#stop}
  - {transition: daemon#killed}
crash:
- during: bind
  leaves: the lock file, and a stale socket file when the crash came after UnixListener::bind made it
    (daemon/bind.rs:199)
  leaves_state:
  - daemon#stale
  recovery: the kernel releases the flock when the process ends; the next start runs daemon#rebind, or
    daemon#bind when no socket file was made
- after: bind
  leaves: 'the daemon died: the socket file with nothing listening; every job-entry, stuck record and
    reclaim mark is lost; up to 256 history lines that wait in the writer''s channel are lost; running
    jobs keep running'
  leaves_state:
  - daemon#stale
  - job-entry#gone
  - lease#lost
  recovery: launchd's KeepAlive starts a daemon, at most once a minute (ThrottleInterval), which runs
    daemon#rebind; each lease runs the reseat flow; a queued client asks again for 5 seconds, so the new
    daemon queues it again when it binds in that time, and otherwise the client runs the job at once with
    no lease; under launchd, launchd kills a reclaimer, /bin/ps or on_stuck command in flight with the
    daemon's process group, and a daemon started by hand leaves them running as orphans
  tests:
  - test: crates/deadband/src/client/tests.rs#a_lease_seats_again_with_its_job_and_threads_after_the_connection_closes
    kind: kill
  - test: crates/deadband/src/client/tests.rs#a_queued_client_asks_a_restarted_daemon_again_before_it_starts_the_job
    kind: kill
- after: bind
  leaves: after daemon#rebind, a new daemon that admits jobs before the old daemon's running jobs have
    seated themselves, for up to about 100 ms
  recovery: none
- after: bind
  leaves: the operating system refused the sampler, the watchdog or a connection's thread
  leaves_state:
  - daemon#stale
  recovery: serve returns and the process ends (daemon#stop), so launchd starts a new daemon and each
    client goes on as after any daemon end
  tests:
  - {test: crates/deadband/src/daemon/tests/alive.rs#a_daemon_that_cannot_start_its_sampler_stops_serving,
    kind: plant}
  - test: crates/deadband/src/daemon/tests/alive.rs#a_daemon_that_cannot_start_a_connections_thread_stops_serving_and_closes_it
    kind: plant
- after: bind
  leaves: 'a thread of the daemon panicked: the release build''s panic = "abort" ends the process at once
    (daemon#killed), and every in-memory state is lost'
  leaves_state:
  - daemon#stale
  - job-entry#gone
  - lease#lost
  recovery: launchd's KeepAlive starts a new daemon, as after any daemon death; in a build that unwinds,
    a sampler panic instead makes serve return (daemon#stop)
  tests:
  - {test: crates/deadband/src/daemon/tests/alive.rs#a_daemon_stops_serving_when_its_sampler_panics, kind: plant}
- after: bind
  leaves: 'the lock file was removed and a second daemon ran daemon#usurp: two daemons each count only
    their own jobs'
  recovery: within 60 samples the first daemon finds its socket file and lock file replaced and stops;
    a lock file replaced between bind and serve's stat of it is never noticed
  tests:
  - {test: crates/deadband/src/daemon/tests/alive.rs#a_daemon_stops_serving_when_its_lock_file_is_removed,
    kind: plant}
  - test: crates/deadband/src/daemon/tests/alive.rs#a_daemon_stops_serving_when_another_file_replaces_its_lock_file
    kind: plant
---
The daemon binds, reads config.toml once, starts the sampler, the watchdog,
the watch thread and the history writer, and serves until one of the reasons
that daemon names makes `serve` return. Between bind and end the daemon runs
the sample, watch, reclaim-run and history flows.

The bind step is `bind` on a path with no socket file, `rebind` over a socket
file that an ended daemon left (launchd's KeepAlive takes this path after every
daemon end, because no daemon removes its socket file), and `usurp` when the
lock file was removed under a live daemon.

A start that another daemon's lock refuses takes no step: the process prints
`deadband::daemon::running` and exits with status 0, so launchd does not
start it again. Every other end, including `daemon#stop` and the
`deadband::socket::unusable` refusals of bind, exits nonzero, and a signal
ends `daemon#killed`, so launchd's `KeepAlive` restarts both.

The first crash after bind is any end of the daemon process. With `deadband
daemon` run by hand and no launch agent, nothing restarts it, and every later
job runs at once. The `after: bind` point with no test about a new daemon is
the soft limit that client/lease.rs names; it is a window, not a crash, and
the format has no other place for it. No test shows a crash inside bind or
rebind.

A daemon started with `DEADBAND_SOCKET` set reads no config.toml, so it runs
no reclaimer and no `on_stuck` command, and records no history
(environment.rs:176-193). A daemon started with `DEADBAND_SOCKET_DIR` set
does all three, like the default daemon.

The test on the panic crash point runs in a build that unwinds, so it shows
the `daemon#stop` path, not the abort that a released daemon takes.
