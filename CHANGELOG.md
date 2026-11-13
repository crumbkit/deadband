# Changelog

This file records the notable changes to deadband. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and deadband
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `deadband service install` writes and loads the launchd agent that runs
  `deadband daemon` at login and restarts it when it stops, with `TMPDIR`
  and `PATH` set for launchd. It leaves an identical, loaded plist alone. It
  reloads a changed one only when the daemon under the per-user temporary
  directory answers with no jobs; otherwise it keeps the old plist, so
  running install again when no job runs reloads it. `--restart` reloads the
  agent even while the daemon has jobs. `deadband service uninstall` unloads
  the agent and deletes the plist after it asks, or at once with `--yes`.
  Both take `--dry-run`, and on platforms other than macOS both exit with
  `deadband::service::unsupported`.
- When no daemon answers on macOS, `deadband status` says whether the agent
  is not installed or not loaded and names `deadband service install`.
  `deadband status --json` gives the state in `daemon_state`.
- Every command accepts `--no-input`, which makes a command that would ask
  a question refuse instead.
- `DEADBAND_SOCKET_DIR` names the directory that holds `deadband.sock`. It
  is read after `DEADBAND_SOCKET` and before the per-user directory.

### Fixed

- On macOS, `deadband run` and `deadband status` now find the launchd
  daemon's socket whatever `TMPDIR` is. They use the directory that
  `getconf DARWIN_USER_TEMP_DIR` prints, where that daemon listens, so a
  caller with another `TMPDIR` no longer runs its jobs uncounted. They fall
  back to `TMPDIR` when the system has no value. They read it with
  `confstr`, so they start no process.

## [0.2.1] - 2026-11-11

### Fixed

- The release binaries are now published. Version 0.2.0 was tagged, but its
  release did not complete, so it has no binaries.

## [0.2.0] - 2026-11-11

### Added

- Each version tag gets a GitHub release with `deadband` binaries for macOS
  on Apple silicon and for Linux on x86_64 and aarch64, as tar.gz archives
  with a SHA256SUMS file.
- `deadband skills install` writes deadband's agent skill into each agent
  skills directory that exists, `~/.claude/skills`, `~/.agents/skills` and
  `$CODEX_HOME/skills`, or into the directory that `--dir` names.
  `deadband skills check` exits with 1 when an installed copy is missing or
  differs from the installed deadband's, and `deadband skills print` prints
  the skill. The skill is crates/deadband/skill/SKILL.md, embedded in the
  binary.
- `deadband completion install` writes deadband's shell completion where the
  shell loads it, for bash, zsh and fish, with `--shell` naming the shell when
  `SHELL` does not. `deadband completion check` exits with 1 when the
  installed file is missing or differs, and `deadband completion print`
  prints the registration script. deadband also answers a shell's completion
  request, which a shell makes by running deadband with `COMPLETE` set to its
  name. deadband now builds on crumbkit 0.3.1.
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
  that show a warm build, and `kind_tools`, the programs whose kind in the job
  history keeps up to two subcommand words. The kind of any other job is its
  program's file name alone. Both keys are empty by default. An older binary
  refuses these keys and runs no reclaimer, so add them only after installing
  this release.
- `deadband status` and `deadband top` show each job's workspace and short
  command. The workspace is the nearest directory at or above the job's
  working directory that holds a `.deadband-workspace` file, or the name that
  a program passes in the new `Asked::workspace` field. The short command is
  the job-history kind of the job's command line, such as `cargo nextest
  run`. `deadband top` gains `workspace` and `job` columns.

### Changed

- `deadband status` prints `running: NAME (pid N): SHORT`, where NAME joins
  the workspace and the label, in place of the label and the full command
  line. `deadband status --json` prints the `deadband/status/v2` document,
  whose jobs carry `workspace` and `short` and no longer carry `command`.
- `Asked` gains a `workspace` field, so a program that builds one must set it.

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
