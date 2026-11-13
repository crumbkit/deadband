---
$type: lifecycles/entity/v1
name: job-entry
entity: One job in the daemon's queue, from asking to its place in the ring of the last 64 ended jobs.
stores:
- daemon-state
states:
- {id: absent, kind: absent, initial: true}
- {id: waiting, kind: stored}
- {id: running, kind: stored}
- {id: ended, kind: stored}
- {id: gone, kind: absent, terminal: true}
transitions:
- id: ask
  from: absent
  to: waiting
  owner: crates/deadband/src/daemon/seat.rs#ask
  effects:
  - {store: daemon-state, op: create, reads: under-lock}
  locks:
  - queue
  unchecked_reads:
  - whether the daemon records, and the config's warm_dirs and kind_tools, read under an earlier hold
    of the queue mutex; they are fixed at daemon start
- id: grant
  from: waiting
  to: running
  owner: crates/deadband/src/daemon/reply.rs#State::grant
  effects:
  - {store: daemon-state, op: update, reads: under-lock, if_missing: fail}
  locks:
  - queue
  unchecked_reads:
  - the machine reading, taken with the queue mutex released and stored under it before admission decides
    (daemon.rs:304-311)
- id: seat
  from: absent
  to: running
  owner: crates/deadband/src/daemon/seat.rs#State::seat
  effects:
  - {store: daemon-state, op: create, reads: under-lock}
  locks:
  - queue
  unchecked_reads:
  - whether the daemon records, and the config's warm_dirs and kind_tools, read under an earlier hold
    of the queue mutex; they are fixed at daemon start
- id: undelivered
  from: running
  to: ended
  owner: crates/deadband/src/daemon/reply.rs#deliver
  effects:
  - {store: daemon-state, op: update, reads: under-lock, if_missing: fail}
  locks:
  - queue
  unchecked_reads:
  - whether the grant could be written, which deliver learns with the queue mutex released (daemon/reply.rs:104);
    State::record then finds nothing to remove when the client's close already ran job-entry#end
- id: leave
  from: waiting
  to: ended
  owner: crates/deadband/src/daemon/seat.rs#finish
  effects:
  - {store: daemon-state, op: update, reads: under-lock, if_missing: fail}
  locks:
  - queue
- id: end
  from: running
  to: ended
  owner: crates/deadband/src/daemon/seat.rs#finish
  effects:
  - {store: daemon-state, op: update, reads: under-lock, if_missing: fail}
  locks:
  - queue
- id: evict
  from: ended
  to: gone
  owner: crates/deadband/src/daemon/timeline.rs#State::record
  effects:
  - {store: daemon-state, op: delete, reads: under-lock}
  locks:
  - queue
regions:
- id: stuck
  exists_in:
  - running
  states:
  - {id: untracked, kind: absent, initial: true}
  - {id: watching, kind: stored}
  - {id: stuck, kind: stored}
  transitions:
  - id: track
    from: untracked
    to: watching
    owner: crates/deadband/src/daemon/watch.rs#State::apply
    effects:
    - {store: daemon-state, op: create, reads: under-lock}
    locks:
    - queue
    unchecked_reads:
    - each job's process tree, read through the probe with the queue mutex released
  - id: become-stuck
    from: watching
    to: stuck
    owner: crates/deadband/src/daemon/watch.rs#State::apply
    effects:
    - {store: daemon-state, op: update, reads: under-lock, if_missing: fail}
    locks:
    - queue
    unchecked_reads:
    - each job's process tree, read through the probe with the queue mutex released
  - id: become-active
    from: stuck
    to: watching
    owner: crates/deadband/src/daemon/watch.rs#State::apply
    effects:
    - {store: daemon-state, op: update, reads: under-lock, if_missing: fail}
    locks:
    - queue
    unchecked_reads:
    - each job's process tree, read through the probe with the queue mutex released
---
The entry lives only in the daemon's memory, in the `Queue` and `ended` ring of
`State`, behind one `Mutex`. Every transition holds that mutex. A daemon that
ends loses every entry in every state at once; the flows record that as a
crash, because no code runs it.

`ask` and `seat` first take the mutex to copy whether the daemon records and
the config's `warm_dirs` and `kind_tools`, release it, read the job's working
directory from the disk, and take the mutex again to push the entry
(daemon/seat.rs:70-89). The second hold uses values read under the first.
Those values are fixed when the daemon starts, so the gap changes nothing. The
effect says `under-lock`, because the queue itself is read under the hold that
writes it, and the earlier read is listed in `unchecked_reads`.

`grant` decides each grant under the mutex (`State::grant`,
daemon/reply.rs:34-57). The thread then releases the mutex and writes its
replies one after another with a 1 second write timeout (`deliver`,
daemon/reply.rs:102-120). When a grant is not written in that second, the
thread takes the mutex again, removes the job (`undelivered`), which records
it once as left, grants again, and shuts the connection down after it has
released the mutex. `ask` decides `Queued` under the mutex in the same way
and writes it with `deliver`. A client that does not read never holds the
mutex. It delays only the later replies of the same delivery, by up to a
second each.

`undelivered` decides from a failed write that `deliver` saw with the mutex
released. When the client's close reaches `finish` first, `end` has already
removed the job, and `State::record` finds nothing to remove
(daemon/timeline.rs:121-125).

`leave` and `end` both run in `finish` after the connection's read ends:
`Done` from the client, end of file, or a line the daemon cannot parse. `end`
records the exit as lost when no `Done` arrived. Both hand a history line to
the writer with `try_send`, which never waits.

`evict` is the drop of the oldest ended entry when a 65th job ends. It acts on
another entry than the one that just ended. The format's steps cannot say
which instance a transition acts on.

`seat` takes a job that an earlier daemon granted. It runs even past
`max_jobs`, and it counts at most one grant's threads.

The `stuck` region exists only for a running job whose client set `watched`
(only `deadband run` does) and whose command does not match `stuck_ignore`.
`exists_in` can name a state but not a condition, so the region reads as if
every running job had it. This is a bend in the index. The watch thread reads the process tree
with the mutex released and applies the rule under it. The first reading of a
job creates its track (`track`, the `or_insert_with` at
daemon/watch.rs:126-130). A job that ended during the read gets no record. The
track ends with the entry: `State::record` removes it when the job ends, and
keeps its last four stuck periods with the ended entry.
