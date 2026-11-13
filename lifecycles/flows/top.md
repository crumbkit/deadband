---
$type: lifecycles/flow/v1
name: top
command: deadband top
steps:
- {id: enter, transition: terminal#enter}
- {id: leave, transition: terminal#leave}
crash:
- after: enter
  leaves: 'SIGTERM or SIGKILL ended deadband top: the terminal stays in raw mode on the alternate screen'
  leaves_state:
  - terminal#raw
  recovery: none; the person runs reset
---
`top` polls the daemon once a second with a Timeline request and changes no
daemon state. A panic restores the terminal through ratatui's panic hook.
