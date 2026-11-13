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
      so no delete comes between install''s read and its rename. The rewrite still recreates a missing
      plist, so the pair stays unsafe for two commands with different XDG_STATE_HOME values: they take
      different service.lock files while the plist path follows HOME, and can interleave'
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

Install and uninstall hold `service.lock` in the state directory from before
they read the plist or launchd until they finish (cli/service.rs:187, 283). A
second command looks again every 100 ms, about ten seconds in all, and then
refuses with `deadband::service::locked`. Install reads the existing plist
under the lock and replaces it by an atomic rename after `create_dir_all` on
`~/Library/LaunchAgents`, so the write effects read under the lock. The
rewrite recreates a missing plist (`if_missing: recreate`), which keeps the
rewrite and the delete an H1 pair in the format. The lock covers it except
for two commands whose `XDG_STATE_HOME` differs, because the lock's path
follows `XDG_STATE_HOME` and the plist's path follows `HOME`. deadband keeps
that as a soft limit. Install refuses to replace a file that is not an XML
plist.

`create_dir_all` makes the directory when it is missing and does nothing when
it exists. deadband never removes the directory. `reads: none` on that effect
means it writes without reading.
