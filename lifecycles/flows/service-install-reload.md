---
$type: lifecycles/flow/v1
name: service-install-reload
command: deadband service install, over a loaded agent whose plist changed or is missing, or with --restart
steps:
- {id: tools, transition: launchd-tool-process#spawn}
- {id: tools-reap, transition: launchd-tool-process#reap}
- {id: bootout, transition: launch-service#bootout-for-reload}
- {id: killed, transition: daemon#killed}
- id: plist
  one_of:
  - {transition: launch-plist#write-plist}
  - {transition: launch-plist#rewrite-plist}
- {id: bootstrap, transition: launch-service#bootstrap}
- id: bind
  one_of:
  - {transition: daemon#bind}
  - {transition: daemon#rebind}
crash:
- during: bootout
  leaves: the agent loaded with the old plist
  recovery: a rerun of install reloads it
  tests:
  - test: crates/deadband/src/cli/service/tests.rs#a_failed_bootout_during_install_keeps_the_old_plist_so_a_rerun_reloads
    kind: plant
- during: plist
  leaves: 'the write failed: the old plist, the agent not loaded and no daemon, so every job runs at once'
  leaves_state:
  - launch-service#not-loaded
  - daemon#stale
  recovery: a rerun of install writes the plist and loads it
  tests:
  - test: crates/deadband/src/cli/service/tests.rs#a_write_that_fails_after_bootout_leaves_the_old_plist_not_loaded_and_a_rerun_loads_the_new_one
    kind: plant
- during: bootstrap
  leaves: 'the bootstrap failed: the new plist, the agent not loaded and no daemon'
  leaves_state:
  - launch-service#not-loaded
  - daemon#stale
  recovery: a rerun of install loads it
  tests:
  - test: crates/deadband/src/cli/service/tests.rs#a_failed_bootstrap_after_bootout_leaves_the_agent_not_loaded_and_a_rerun_loads_it
    kind: plant
---
Every install holds `service.lock` from its start to its end. It reads the
per-user temporary directory with `confstr`, runs `launchctl print`, and after
bootout polls `launchctl print` for about 5 seconds until the old daemon has
exited; the tools steps stand for all of these, each a
launchd-tool-process from spawn to reap. The format has no loop, so they are
listed once.

Without `--restart`, install asks the daemon for its jobs first and changes
nothing while the daemon has running or waiting jobs or does not answer. With
`--restart`, the killed step ends the daemon while it holds jobs: running jobs
reseat on the new daemon, and waiting clients ask again for 5 seconds, so the new
daemon queues them again when it binds in that time, as the run flow says.
The report gives the running and waiting jobs the old daemon held. The plist step is `write-plist` when launchd has the agent loaded
with no plist, and `rewrite-plist` when the plist changed. With `--restart`
and an unchanged plist, the plist step does not run; draft 4 has no optional
step. The killed and bind steps are the daemon's, which launchd causes.
