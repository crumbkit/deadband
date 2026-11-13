---
$type: lifecycles/entity/v1
name: launch-plist
entity: deadband's launch agent plist, and the LaunchAgents directory that holds it.
stores:
- plist
- launch-agents-dir
states:
- {id: absent, kind: absent, initial: true}
- {id: present, kind: stored}
transitions:
- id: write-plist
  from: absent
  to: present
  owner: crates/deadband/src/cli/service.rs#install
  effects:
  - {store: launch-agents-dir, op: create, reads: none}
  - {store: plist, op: create, reads: under-lock}
  locks:
  - service
- id: rewrite-plist
  from: present
  to: present
  owner: crates/deadband/src/cli/service.rs#install
  effects:
  - {store: launch-agents-dir, op: create, reads: none}
  - {store: plist, op: update, reads: under-lock, if_missing: recreate}
  locks:
  - service
  unguarded:
    why: 'install and uninstall both hold service.lock from before they read the plist until they finish,
      so no delete comes between install''s read and its rename. The lock is under HOME, as the plist is,
      so two commands that share a plist share the lock whatever XDG_STATE_HOME says. The rewrite still
      recreates a missing plist, which keeps the pair an H1 pair in the format'
    against:
    - launch-plist#delete-plist
    test: crates/deadband/src/cli/service/tests.rs#an_install_racing_an_uninstall_leaves_the_plist_only_while_launchd_has_it_loaded
- id: delete-plist
  from: present
  to: absent
  owner: crates/deadband/src/cli/service.rs#uninstall
  effects:
  - {store: plist, op: delete, reads: under-lock}
  locks:
  - service
---
The plist and the launchd service are two independent stores, so they are two
entities: this one and launch-service. Any pair of their states can occur.
`deadband status` reports `not_installed` whenever the plist is absent, even
when launchd still has the service loaded. Uninstall then boots out the
service and reports the plist absent.

Install and uninstall hold `service.lock` in `$HOME/.local/state/deadband`
(whatever `XDG_STATE_HOME` says) from before
they read the plist or launchd until they finish (cli/service.rs:187, 283). A
second command looks again every 100 ms, about ten seconds in all, and then
refuses with `deadband::service::locked`. Install reads the existing plist
under the lock and replaces it by an atomic rename after `create_dir_all` on
`~/Library/LaunchAgents`, so the write effects read under the lock. The
rewrite recreates a missing plist (`if_missing: recreate`), which keeps the
rewrite and the delete an H1 pair in the format. The lock covers it, because
the lock's path and the plist's path both follow `HOME`, so commands with
different `XDG_STATE_HOME` values take the same lock. Install refuses to replace a file that is not an XML
plist. A plist written by an older deadband, whose `KeepAlive` is `true`,
differs from the rendered text, so install rewrites it (`rewrite-plist`) and
reloads the agent; that is how an installed agent takes the `KeepAlive`
dictionary that restarts only an unclean exit.

`create_dir_all` makes the directory when it is missing and does nothing when
it exists. deadband never removes the directory. `reads: none` on that effect
means it writes without reading.
