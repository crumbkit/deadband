---
$type: lifecycles/flow/v1
name: history-append
command: the daemon's history writer thread, for a line while the file is open
steps:
- {id: append, transition: history-file#append}
crash:
- during: append
  leaves: a torn last line; the writer closes the file after the failed write
  leaves_state:
  - history-file#torn
  recovery: the next line runs the history-reopen flow, whose open is history-file#repair
---
Most lines take this flow. A write past 64 MiB is followed by the
history-rotate flow.
