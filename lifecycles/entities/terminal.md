---
$type: lifecycles/entity/v1
name: terminal
entity: The controlling terminal's raw mode and alternate screen while deadband top runs.
stores:
- terminal-mode
states:
- {id: cooked, kind: stored, initial: true}
- {id: raw, kind: stored}
transitions:
- id: enter
  from: cooked
  to: raw
  owner: crates/deadband/src/tui.rs#Terminal::enter
  effects:
  - {store: terminal-mode, op: update, reads: none, if_missing: fail}
  locks: []
  unguarded:
    why: one deadband top owns its terminal; a second top on the same terminal is not guarded, and the
      first to leave restores the terminal under the other
    against:
    - terminal#enter
    - terminal#leave
- id: leave
  from: raw
  to: cooked
  owner: crates/deadband/src/tui.rs#Terminal::leave
  effects:
  - {store: terminal-mode, op: update, reads: none, if_missing: fail}
  locks: []
  unguarded:
    why: each Guard leaves its modes once, when it drops; a second deadband top on the same terminal is
      not guarded, so one top's leave can restore the terminal while the other has entered raw mode and
      still draws
    against:
    - terminal#enter
    - terminal#leave
---
The terminal's mode is a setting of the terminal device, not a file. `Guard`
leaves the modes when it drops on a normal exit or an error. During a panic,
ratatui's panic hook restores them. SIGTERM and SIGKILL end the process with
no unwind, so the terminal stays raw and the person runs `reset`.
