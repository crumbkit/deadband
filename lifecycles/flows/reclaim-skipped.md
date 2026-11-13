---
$type: lifecycles/flow/v1
name: reclaim-skipped
command: the daemon's reclaim thread when another daemon holds reclaim.lock, or the sampler when the operating
  system refuses the reclaim thread
steps:
- {id: start, transition: reclaim#start}
- {id: skip, transition: reclaim#skip}
crash:
- after: skip
  leaves: no reclaim now, and one log line that says why
  leaves_state:
  - reclaim#idle
  recovery: the resource rests exactly reclaim_after, and the next reclaim that the rule starts tries
    the lock again
  tests:
  - test: crates/deadband/src/reclaim/tests.rs#a_reclaim_skipped_for_a_held_lock_rests_reclaim_after_and_is_not_counted_as_fruitless
    kind: seam
---
No test refuses the reclaim thread, because the sampler thread starts it and
a test's refusal reaches only the threads that the test's own thread starts.
