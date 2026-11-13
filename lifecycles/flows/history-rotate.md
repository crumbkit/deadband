---
$type: lifecycles/flow/v1
name: history-rotate
command: the daemon's history writer thread, after a write took jobs.jsonl past 64 MiB
steps:
- {id: rotate, transition: history-file#rotate}
- id: rotated
  one_of:
  - {transition: rotated-history#rotate-in}
  - {transition: rotated-history#rotate-over}
crash:
- {during: rotate, leaves: 'either the old names or the new, because rename is atomic; no line is lost
    or doubled', recovery: none}
- after: rotated
  leaves: jobs.1.jsonl and no jobs.jsonl
  leaves_state:
  - history-file#absent
  - rotated-history#present
  recovery: the next line runs the history-reopen flow, whose open is history-file#open-new
  tests:
  - test: crates/deadband/src/history/tests.rs#a_fresh_writer_on_a_directory_that_holds_only_the_rotated_file_starts_a_new_file
    kind: plant
---
The two steps are one `rename`; see history-file. `rotate-in` runs when no
`jobs.1.jsonl` exists and `rotate-over` replaces the earlier one, which is the
only deletion of history. A failed rename logs once, the file stays open and
keeps growing, and the next write tries again. The writer checks the path
before each line and opens a removed `jobs.jsonl` again, so a later rotation
renames the new file.
