---
$type: lifecycles/flow/v1
name: service-uninstall
command: deadband service uninstall
steps:
- {id: tools, transition: launchd-tool-process#spawn}
- {id: tools-reap, transition: launchd-tool-process#reap}
- {id: bootout, transition: launch-service#bootout}
- {id: killed, transition: daemon#killed}
- {id: delete, transition: launch-plist#delete-plist}
crash:
- during: bootout
  leaves: the agent loaded and the plist kept
  recovery: a rerun of uninstall
  tests:
  - {test: crates/deadband/src/cli/service/tests.rs#a_failed_bootout_keeps_the_plist, kind: plant}
- during: delete
  leaves: 'the delete failed: the agent not loaded, the plist kept and no daemon'
  leaves_state:
  - launch-plist#present
  - launch-service#not-loaded
  - daemon#stale
  recovery: a rerun of uninstall deletes the plist
  tests:
  - test: crates/deadband/src/cli/service/tests.rs#a_delete_that_fails_after_uninstalls_bootout_keeps_the_plist_and_a_rerun_deletes_it
    kind: plant
---
Uninstall asks first unless `--yes` is given, and its question and report
give the running and waiting jobs the daemon held. It then holds
`service.lock` until it finishes. When the agent is not loaded, the
bootout and killed steps do not run. Running jobs keep running; their leases
look for a daemon every 100 ms for as long as they run. The report names the
files that stay, such as the daemon's socket file, lock file and log.
