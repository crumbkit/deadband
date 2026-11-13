---
$type: lifecycles/entity/v1
name: skill
entity: deadband's agent skill, a copy in each agent skills directory that exists.
stores:
- skill-files
states:
- {id: absent, kind: absent, initial: true}
- {id: installed, kind: stored, terminal: true}
transitions:
- id: install-new
  from: absent
  to: installed
  owner: crates/deadband/src/cli/setup.rs#skills
  effects:
  - {store: skill-files, op: create, reads: before-lock}
  locks: []
  unguarded:
    why: nothing locks two skills installs; each writes the same embedded skill, and the later write wins
    against:
    - skill#install-new
    - skill#install-over
- id: install-over
  from: installed
  to: installed
  owner: crates/deadband/src/cli/setup.rs#skills
  effects:
  - {store: skill-files, op: update, reads: before-lock, if_missing: recreate}
  locks: []
  unguarded:
    why: nothing locks two skills installs; the later write wins
    against:
    - skill#install-new
    - skill#install-over
---
`deadband skills install` hands the embedded skill to `crumbkit_skills::install`,
which chooses the directories and how it writes them. deadband has no command
that removes the skill, so `installed` is terminal. The registry cannot see
into the library, so the effects describe only what deadband asks for.
