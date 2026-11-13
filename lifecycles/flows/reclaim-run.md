---
$type: lifecycles/flow/v1
name: reclaim-run
command: the daemon's reclaim thread, started by the sampler
steps:
- {id: dir, transition: state-dir#make-open}
- {id: dry, transition: reclaimer-process#spawn}
- {id: dry-reap, transition: reclaimer-process#reap}
- {id: real, transition: reclaimer-process#spawn}
- {id: real-reap, transition: reclaimer-process#reap}
- {id: report, transition: reclaim#report}
- {id: finish, transition: reclaim#finish}
crash:
- after: real
  leaves: 'the daemon died while a reclaimer runs: the daemon''s reclaim.lock is released with its descriptors,
    and the next daemon''s reclaim mark is idle'
  recovery: under launchd, launchd kills the reclaimer with the daemon's process group, so it stops halfway;
    a daemon started by hand leaves it running, and the next daemon can take reclaim.lock and start a
    second reclaim beside it
- during: real-reap
  leaves: a reclaimer past its timeout, which the log names
  recovery: the reclaim thread keeps waiting until the reclaimer exits, and no other reclaim starts until
    then
  tests:
  - test: crates/deadband/src/cli/tests/reclaim.rs#a_reclaimer_that_runs_past_its_timeout_is_logged_and_admission_continues
    kind: plant
- after: report
  leaves: 'the reclaim thread panicked: the release build''s panic = "abort" ends the daemon, and every
    in-memory state is lost'
  leaves_state:
  - daemon#stale
  - job-entry#gone
  - lease#lost
  recovery: launchd's KeepAlive starts a new daemon, as after any daemon death; a build that unwinds keeps
    the daemon and leaves the reclaim mark set for its life
---
For each reclaimer in turn the thread runs a dry run, logs its plan, and runs
it for real when the plan frees any bytes. It stops when the bytes freed cover
the need. The steps show one reclaimer; the loop repeats dry to report for each
one until the need is met.

The dir step makes the state directory, which already exists on most runs.
The thread holds `reclaim.lock` from before the dry step until after finish.
When another daemon holds it, the thread runs the reclaim-skipped flow
instead.
