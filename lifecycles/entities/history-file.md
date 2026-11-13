---
$type: lifecycles/entity/v1
name: history-file
entity: The job history file jobs.jsonl in the state directory.
stores:
- history-file
states:
- {id: absent, kind: absent, initial: true}
- {id: present, kind: stored}
- {id: torn, kind: stored}
transitions:
- id: open-new
  from: absent
  to: present
  owner: crates/deadband/src/history/writer.rs#open_file
  effects:
  - {store: history-file, op: create, reads: none}
  locks:
  - history
- id: append
  from: present
  to: present
  owner: crates/deadband/src/history/writer.rs#Writer::write
  effects:
  - {store: history-file, op: update, reads: under-lock, if_missing: fail}
  locks:
  - history
- id: repair
  from: torn
  to: present
  owner: crates/deadband/src/history/writer.rs#open_file
  effects:
  - {store: history-file, op: update, reads: under-lock, if_missing: fail}
  locks:
  - history
- id: rotate
  from: present
  to: absent
  owner: crates/deadband/src/history/writer.rs#Writer::rotate_past_limit
  effects:
  - {store: history-file, op: delete, reads: under-lock}
  locks:
  - history
- id: reopen
  from: present
  to: present
  owner: crates/deadband/src/history/writer.rs#open_file
  locks:
  - history
  effects:
  - {store: history-file, op: update, reads: under-lock, if_missing: fail}
- id: unlinked
  from: present
  to: absent
  owner: outside:person
  locks: []
  effects:
  - {store: history-file, op: delete, reads: none}
  unguarded:
    why: something outside deadband removed or replaced jobs.jsonl while the writer holds it open; before
      each line the writer compares the open file with the file at the path by device and inode and opens
      the path again when they differ, so only a line that passed that check just before the removal goes
      to the removed file
    against:
    - history-file#append
    - history-file#open-new
    - history-file#reopen
    - history-file#repair
    - history-file#rotate
    test: crates/deadband/src/history/tests.rs#a_writer_whose_file_is_unlinked_opens_a_new_file_at_the_path_and_records_later_lines
---
One writer thread per daemon owns the file. It takes the `jobs.lock` flock
before it opens the file. A writer that finds the lock held records nothing,
logs one line, and tries the lock again on each later line. A daemon started
with `DEADBAND_SOCKET` set starts no writer.

`torn` is entered only by a crash or a failed write in the middle of a line.
No transition goes there; the history flows' crash points name it in
`leaves_state`. The next open adds a newline (`repair`). An open of a whole
file (`reopen`) sets the file's mode to 0600 and writes nothing else.

Before each line the writer checks, by device and inode, that its open file is
still the file at `jobs.jsonl` (history/writer.rs:243-249). When the path
names no file or another file, the writer closes its file and opens the path
again, which is `open-new` for a removed file and `reopen` for a replacement.
`append` itself writes to the open file descriptor, not to the path, so it
says `if_missing: fail`. The check and the write are two calls, so a removal
between them sends that one line to the removed file
(history/writer.rs:243-254). The format has no value for "writes to the
removed file"; the bend in the index says so.

When something outside deadband removes `jobs.jsonl` (`unlinked`), the next
line therefore makes a new file at the path, and later lines are recorded
there.

`rotate` is a `rename` of `jobs.jsonl` to `jobs.1.jsonl` past 64 MiB. It is one
atomic call that changes two stores. An effect writes only its own entity's
stores, so the rename is split into this transition and rotated-history's,
and the history-rotate flow lists them as two steps that cannot happen apart.
This is a bend in the index.
