---
$type: lifecycles/flow/v1
name: history-reopen
command: the daemon's history writer thread, for a line after a failed write or a rotation closed the
  file, or after the writer found jobs.jsonl or jobs.lock removed or replaced
steps:
- {id: dir, transition: state-dir#set-mode}
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
---
A failed write closes the file, and so do a rotation and a removed or
replaced `jobs.jsonl` or `jobs.lock`. The next line opens it again:
`make_state_dir` (daemon.rs) sets the directory's mode, and the open step is one of the
three opens.
