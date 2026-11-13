---
$type: lifecycles/flow/v1
name: service-install-load
command: deadband service install, over an agent that launchd has not loaded
steps:
- {id: tools, transition: launchd-tool-process#spawn}
- {id: tools-reap, transition: launchd-tool-process#reap}
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
- {during: plist, leaves: 'the write failed: the agent not loaded, and the plist as it was', recovery: a
    rerun of install}
- during: bootstrap
  leaves: 'the bootstrap failed: the plist written and the agent not loaded'
  leaves_state:
  - launch-plist#present
  - launch-service#not-loaded
  recovery: a rerun of install loads it
  tests:
  - {test: crates/deadband/src/cli/service/tests.rs#a_failed_bootstrap_leaves_the_plist_installed_but_not_loaded,
    kind: plant}
- after: bind
  leaves: launchd loaded the agent and its daemon exits at start, for example because a daemon that launchd
    did not start holds the socket's lock
  leaves_state:
  - launch-service#loaded
  recovery: install reports that the daemon does not answer or that another daemon holds the socket, and
    names the log; launchd starts the daemon again at most once a minute until the other daemon ends
  tests:
  - test: crates/deadband/src/cli/service/tests.rs#an_install_says_when_a_daemon_that_launchd_did_not_start_holds_the_socket
    kind: seam
  - {test: crates/deadband/src/cli/service/tests.rs#an_install_whose_daemon_never_answers_says_so_and_names_the_log,
    kind: seam}
---
This covers a fresh install, with no plist (`write-plist`), and an installed
agent that launchd has not loaded (`rewrite-plist` when the plist changed; with
an unchanged plist the plist step does not run). The bind step is the daemon's
own, started by `RunAtLoad`.

The whole command holds `service.lock`; a second service command waits for it
for about ten seconds. After the bootstrap, install asks the socket up to 20
times and reports whether the new daemon answers.
