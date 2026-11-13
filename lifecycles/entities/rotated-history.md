---
$type: lifecycles/entity/v1
name: rotated-history
entity: The rotated job history file jobs.1.jsonl.
stores:
- rotated-history
states:
- {id: absent, kind: absent, initial: true}
- {id: present, kind: stored, terminal: true}
transitions:
- id: rotate-in
  from: absent
  to: present
  owner: crates/deadband/src/history/writer.rs#Writer::rotate_past_limit
  effects:
  - {store: rotated-history, op: create, reads: none}
  locks:
  - history
- id: rotate-over
  from: present
  to: present
  owner: crates/deadband/src/history/writer.rs#Writer::rotate_past_limit
  effects:
  - {store: rotated-history, op: update, reads: none, if_missing: recreate}
  locks:
  - history
---
The rename replaces an earlier `jobs.1.jsonl`, which is the only deletion of
history. deadband never removes the rotated file, so `present` is terminal.
