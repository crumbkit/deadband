---
$type: lifecycles/flow/v1
name: history-unlinked
command: 'outside: something removes jobs.jsonl while the writer holds it open'
steps:
- {id: unlinked, transition: history-file#unlinked}
- {id: open, transition: history-file#open-new}
- {id: append, transition: history-file#append}
crash:
- after: unlinked
  leaves: no jobs.jsonl; a line that passed the writer's identity check just before the removal went to
    the removed file and is lost
  leaves_state:
  - history-file#absent
  recovery: the next line finds the path empty and runs history-file#open-new, so later lines are recorded
    at the path
  tests:
  - test: crates/deadband/src/history/tests.rs#a_writer_whose_file_is_unlinked_opens_a_new_file_at_the_path_and_records_later_lines
    kind: plant
---
The open and append steps are the writer's next line. A file that replaces
`jobs.jsonl`, instead of leaving the path empty, is opened by `reopen` and
appended to (`a_writer_whose_file_is_replaced_appends_to_the_file_now_at_the_path`
in history/tests.rs).
