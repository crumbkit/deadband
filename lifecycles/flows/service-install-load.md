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
  leaves: launchd loaded the agent and its daemon exits at start with status 0, because a daemon that launchd
    did not start holds the socket's lock
  leaves_state:
  - launch-service#loaded
  recovery: install says that another daemon holds the socket; launchd does not start its daemon again, so
    after the other daemon ends, deadband service install --restart or the next login starts it
  tests:
  - test: crates/deadband/src/cli/service/tests.rs#an_install_says_when_a_daemon_that_launchd_did_not_start_holds_the_socket
    kind: seam
  - {test: crates/deadband/src/cli/tests.rs#daemon_exits_0_with_daemon_running_while_another_daemon_answers_on_its_socket,
    kind: plant}
  - test: crates/deadband/src/cli/service/tests.rs#the_plist_makes_launchd_restart_only_a_daemon_that_exits_nonzero_or_by_a_signal
    kind: seam
- after: bind
  leaves: launchd loaded the agent and its daemon fails at start with a nonzero status, for example because
    something other than a socket is at the socket's path
  leaves_state:
  - launch-service#loaded
  recovery: install reports that the daemon does not answer and names the log; launchd starts the daemon
    again at most once a minute until the cause is fixed
  tests:
  - {test: crates/deadband/src/cli/service/tests.rs#an_install_whose_daemon_never_answers_says_so_and_names_the_log,
    kind: seam}
  - test: crates/deadband/src/cli/tests.rs#daemon_exits_4_with_socket_unusable_when_a_file_that_is_not_a_socket_holds_its_path
    kind: plant
---
This covers a fresh install, with no plist (`write-plist`), and an installed
agent that launchd has not loaded (`rewrite-plist` when the plist changed; with
an unchanged plist the plist step does not run). The bind step is the daemon's
own, started by `RunAtLoad`.

The whole command holds `service.lock`; a second service command waits for it
for about ten seconds. After the bootstrap, install asks the socket up to 20
times and reports whether the new daemon answers.
