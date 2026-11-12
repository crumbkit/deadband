# deadband

deadband keeps a busy machine usable while many heavy jobs, such as builds
and test runs, compete for it. Each heavy job asks deadband before it
starts. deadband admits the job when the machine has CPU and memory headroom
and holds it in a queue when it does not. It starts fast-lane jobs first and
runs normal-lane jobs at a lower priority. The priority file or `--lane` chooses
the lane. On macOS, `deadband service install` makes launchd start the
daemon at login.

[docs/running.md](docs/running.md) says how to install and run it, and
[docs/architecture.md](docs/architecture.md) describes its modules.

## Licence

deadband is dual-licensed under the Apache License 2.0 and the MIT licence,
at your option.
