---
$type: lifecycles/entity/v1
name: launch-service
entity: The launchd service gui/<uid>/io.github.crumbkit.deadband, which runs the daemon.
stores:
- launchd-service
states:
- {id: not-loaded, kind: absent, initial: true}
- {id: loaded, kind: inferred, inferred_by: 'crates/deadband/src/job/launchd.rs#System::is_loaded'}
transitions:
- id: bootstrap
  from: not-loaded
  to: loaded
  owner: crates/deadband/src/cli/service.rs#install
  effects:
  - {store: launchd-service, op: create, reads: under-lock}
  locks:
  - service
- id: bootout-for-reload
  from: loaded
  to: not-loaded
  owner: crates/deadband/src/cli/service.rs#install
  effects:
  - {store: launchd-service, op: delete, reads: under-lock}
  locks:
  - service
- id: bootout
  from: loaded
  to: not-loaded
  owner: crates/deadband/src/cli/service.rs#uninstall
  effects:
  - {store: launchd-service, op: delete, reads: under-lock}
  locks:
  - service
  unchecked_reads:
  - the daemon's running and waiting jobs, which uninstall asks for before it takes service.lock and reports
    after the bootout (cli/commands.rs:273-297)
---
launchd holds this store. deadband changes it only through `launchctl
bootstrap` and `launchctl bootout`, and reads it with `launchctl print`, whose
exit code 113 means not loaded. Install and uninstall ask under
`service.lock`; see launch-plist.

The plist sets `RunAtLoad`, `KeepAlive` and a `ThrottleInterval` of 60
seconds, so while the service is loaded, launchd starts the daemon and starts
it again each time it exits, at most once a minute. `KeepAlive` also restarts
a daemon that exits because another daemon holds the socket's lock
(`deadband::daemon::running`), so a daemon started by hand on the same socket
makes launchd restart its own daemon once a minute until the other daemon
ends. Install says when such a daemon holds the socket, and `deadband status`
names `~/Library/Logs/deadband.log` when launchd has the agent loaded and no
daemon answers.

While the agent is loaded and the daemon has running or waiting jobs, or does
not answer, install changes nothing unless `--restart` is given. After
`bootstrap`, install asks the socket up to 20 times, 100 ms apart, and
reports whether the new daemon answers.

The `loaded` state also stands for the daemon process that launchd runs, which
is the daemon entity. The format cannot tie one entity's state to another's,
so the service flows list the daemon's transitions as steps of their own.
