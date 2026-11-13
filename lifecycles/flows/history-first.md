---
$type: lifecycles/flow/v1
name: history-first
command: the daemon's history writer thread, when it starts and for its first line
steps:
- id: claim-dir
  one_of:
  - {transition: state-dir#make}
  - {transition: state-dir#set-mode}
- {id: lock, transition: history-lock#make}
- {id: open-dir, transition: state-dir#set-mode}
- id: open
  one_of:
  - {transition: history-file#open-new}
  - {transition: history-file#repair}
  - {transition: history-file#reopen}
- {id: append, transition: history-file#append}
crash:
- during: append
  leaves: a torn last line in jobs.jsonl
  leaves_state:
  - history-file#torn
  recovery: the next open runs history-file#repair
  tests:
  - test: crates/deadband/src/history/tests.rs#opening_a_file_with_a_torn_last_line_adds_a_newline_so_later_lines_stay_whole
    kind: plant
- {after: append, leaves: 'the daemon died with lines still in the writer''s channel: up to 256 job and
    machine lines are lost', recovery: none}
---
The writer thread takes the lock in `claim` when it starts, before any line
arrives (history/writer.rs:188-191). `claim` makes the state directory before
it locks `jobs.lock` (history/writer.rs:274); the lock step
runs only when `jobs.lock` does not exist yet (daemon/bind.rs:137). The
writer's first line then opens `jobs.jsonl`, which calls `make_state_dir`
(daemon.rs) again (history/writer.rs:250-253, 354). The open
step is `open-new` when no file exists, `repair` when the last line is torn,
and `reopen` for a whole file.

When another daemon holds `jobs.lock`, the line stops after the claim-dir
step: this writer records nothing, logs one line, and tries the lock again on
each later line (history/writer.rs:275-297). The format has no step that
refuses and stops, so the prose says it.

The daemon hands each line to the writer with `try_send` on a channel of 256
lines (history/writer.rs:73), so it never waits for the disk. Lines are lost
in these cases: a full channel, a stopped writer, another daemon holding
`jobs.lock`, a failed write, a line written in the moment a `jobs.jsonl` is removed
(history-unlinked), and a daemon end with lines in the channel. A job that ends while no daemon holds
its seat has no line (run-ends-while-lost). A daemon started with
`DEADBAND_SOCKET` set writes no line at all.
