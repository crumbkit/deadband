# Changelog

This file records the notable changes to deadband. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and deadband
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- The daemon detects a job that has shown no activity for 5 minutes and says
  so in `deadband status`, which then exits 1, in `deadband top`, and on the
  standard error of the waiting `deadband run`. The `stuck_after`,
  `stuck_ignore` and `on_stuck` config keys control it. deadband never ends,
  pauses or signals a stuck job.
- Each daemon appends one line of JSON for every job that ends, and one line
  about the machine every 60 seconds, to `deadband/jobs.jsonl` in
  `$XDG_STATE_HOME`, or in `~/.local/state`. A recording failure never holds a
  job. A daemon started with `DEADBAND_SOCKET` set records nothing, and a
  second daemon on the same state directory records nothing and logs one line.
- `deadband status --json` shows each job's working directory in an optional
  `cwd` field.
- The config file accepts `warm_dirs`, the directories under a job's checkout
  that show a warm build. An older binary refuses this key and runs no
  reclaimer, so add it only after installing this release.

### Changed

- `DEADBAND_HELD=0` no longer marks a job as held, and `DEADBAND_HELD` and
  `DEADBAND_BYPASS` now read their values by the same rule: a variable takes
  effect when it is set to a value that is neither empty nor `0`.
- A `deadband run` whose daemon goes away connects again every 100 ms and
  tells the next daemon that its job is already running. The new daemon
  counts that job at once, so a restart no longer admits a second full set
  of jobs on top of the ones that run. The new daemon does not count jobs that
  were waiting, jobs that started while no daemon listened, or jobs of clients
  from an older binary, so the first restart onto this release still counts
  nothing from those clients.
