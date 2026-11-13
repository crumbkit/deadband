---
$type: lifecycles/flow/v1
name: history-lock-unlinked
command: 'outside: something removes jobs.lock while a writer holds it'
steps:
- {id: unlinked, transition: history-lock#unlinked}
- {id: make, transition: history-lock#make}
crash:
- after: unlinked
  leaves: the writer's flock is on the removed file; until the writer's next line, a second writer on
    the same state directory can make and lock a new jobs.lock, and a line the first writer had already
    checked can still be written beside it
  leaves_state:
  - history-lock#absent
  recovery: before its next line the writer finds jobs.lock gone, releases its lock and its history file
    and runs history-lock#make; when another writer holds the new lock, this writer records nothing and
    logs one line
  tests:
  - test: crates/deadband/src/history/tests.rs#a_writer_whose_lock_is_removed_while_another_writer_takes_a_new_one_records_nothing_and_logs_once
    kind: plant
  - test: crates/deadband/src/history/tests.rs#a_writer_whose_lock_is_removed_with_no_other_writer_takes_a_new_lock_and_keeps_recording
    kind: plant
---
The make step is the next `Writer::claim` on that state directory, by this
writer or by a second one.
