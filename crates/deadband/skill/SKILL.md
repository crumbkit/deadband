---
name: deadband
description: Run builds, tests and other heavy jobs through deadband so a busy machine stays usable. Use when starting a build, a test suite or any job that uses many cores.
---

# Running heavy jobs through deadband

deadband admits heavy jobs one at a time when the machine has room, so many
agents can build at once without freezing it.

Start every build, test suite and other heavy job as
`deadband run --label <label> -- <command>`. The label names the job in
`deadband status`.

When deadband holds a job, wait for it, or push and let CI test. Never bypass
the hold.

`deadband completion install` writes the shell completion for deadband's
commands where your shell loads it, with `--shell bash|zsh|fish` when `SHELL`
does not name it.
