---
name: deadband
description: Run builds, tests and other heavy jobs through deadband so a busy machine stays usable. Use when starting a build, a test suite or any job that uses many cores.
---

# Running heavy jobs through deadband

deadband starts a heavy job when the machine has CPU and memory headroom and
queues it when it does not.

Start every build, test suite and other heavy job as
`deadband run --label <label> -- <command>`. The label names the job in
`deadband status`.

When deadband holds a job, wait for it, or push and let CI test. Never bypass
the hold.
