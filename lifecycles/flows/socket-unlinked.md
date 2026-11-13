---
$type: lifecycles/flow/v1
name: socket-unlinked
command: 'outside: something removes the socket file while the daemon listens'
steps:
- {id: unlinked, transition: daemon#unlinked}
- {id: end, transition: daemon#end-orphaned}
crash:
- after: unlinked
  leaves: a daemon that no client can reach, for up to 60 samples; clients start every job at once
  leaves_state:
  - daemon#orphaned
  recovery: the daemon's next check of its files runs daemon#end-orphaned, and launchd's KeepAlive starts
    a daemon that runs daemon#bind
  tests:
  - test: crates/deadband/src/daemon/tests/alive.rs#a_daemon_that_no_client_reaches_stops_serving_when_its_socket_file_is_removed
    kind: plant
---
The daemon notices within 60 samples, about a minute, or sooner through the
watchdog when its reading is stale. A socket file that another socket
replaced is noticed the same way (daemon/alive.rs:198-225). The end step is
the daemon's own stop; then the path holds no socket at all, so the next
daemon runs daemon#bind.
