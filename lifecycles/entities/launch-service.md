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
    after the bootout (cli/commands.rs:281-305)
---
launchd holds this store. deadband changes it only through `launchctl
bootstrap` and `launchctl bootout`, and reads it with `launchctl print`, whose
exit code 113 means not loaded. Install and uninstall ask under
`service.lock`; see launch-plist.

The plist sets `RunAtLoad`, a `KeepAlive` dictionary with `SuccessfulExit`
false, and a `ThrottleInterval` of 60 seconds. While the service is loaded,
launchd starts the daemon and starts it again each time it exits with a
nonzero status or by a signal, at most once a minute, and leaves it stopped
when it exits with status 0. Only one exit has status 0: a daemon that
another daemon's lock on the socket refuses (`deadband::daemon::running`,
cli/commands.rs#daemon). So when a daemon started by hand holds the socket,
launchd starts its own daemon once, sees the clean exit and stops. When the
hand-started daemon ends, launchd does not take over; its daemon starts again
at `deadband service install --restart` or at the next login (`RunAtLoad`).
Install says when such a daemon holds the socket, and `deadband status`
names `~/Library/Logs/deadband.log` when launchd has the agent loaded and no
daemon answers.

While the agent is loaded and the daemon has running or waiting jobs, or does
not answer, install changes nothing unless `--restart` is given. After
`bootstrap`, install asks the socket up to 20 times, 100 ms apart, and
reports whether the new daemon answers.

The `loaded` state also stands for the daemon process that launchd runs, which
is the daemon entity. The format cannot tie one entity's state to another's,
so the service flows list the daemon's transitions as steps of their own.
