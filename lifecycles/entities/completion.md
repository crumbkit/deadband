---
$type: lifecycles/entity/v1
name: completion
entity: deadband's shell completion file, where the shell loads it.
stores:
- completion-file
states:
- {id: absent, kind: absent, initial: true}
- {id: installed, kind: stored, terminal: true}
transitions:
- id: install-new
  from: absent
  to: installed
  owner: crates/deadband/src/cli/setup.rs#completion
  effects:
  - {store: completion-file, op: create, reads: before-lock}
  locks: []
  unguarded:
    why: nothing locks two completion installs; each writes the same text for one deadband version, and
      the later write wins
    against:
    - completion#install-new
    - completion#install-over
- id: install-over
  from: installed
  to: installed
  owner: crates/deadband/src/cli/setup.rs#completion
  effects:
  - {store: completion-file, op: update, reads: before-lock, if_missing: recreate}
  locks: []
  unguarded:
    why: nothing locks two completion installs; the later write wins
    against:
    - completion#install-new
    - completion#install-over
---
`deadband completion install` hands deadband's command to
`crumbkit::completion::install`, which chooses the file and how it writes it.
deadband has no command that removes it, so `installed` is terminal.
