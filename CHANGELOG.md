# Changelog

This file records the notable changes to deadband. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and deadband
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security

- Without `DEADBAND_SOCKET`, `DEADBAND_SOCKET_DIR` or `TMPDIR`, the socket
  moves from `/tmp/deadband-<USER>.sock`, where another user could listen
  first, to `deadband.sock` in `$XDG_RUNTIME_DIR/deadband` or else
  `/tmp/deadband-<uid>`. The daemon makes that directory with mode 0700 and
  refuses to start with `deadband::socket::directory` when it is a symlink,
  belongs to another user or has any group or other permission bit. `USER` is
  no longer read. macOS keeps its per-user temporary directory.
- `run`, `status` and `top` check that the process listening on the socket
  belongs to the same user (`getpeereid` on macOS, `SO_PEERCRED` on Linux)
  before they send anything. When it does not, they print one line and run the
  job at once without sending its command line. The check applies to every
  socket path.
- The daemon, the history writer and the service commands open their lock
  files without following a symlink and refuse a lock file that another user
  owns.

### Changed

- `deadband daemon` now exits 0, where it exited 4, when another daemon
  already serves the socket. It still prints the `deadband::daemon::running`
  refusal. The launchd plist now keeps the daemon alive only when it exits
  with a nonzero status or a signal ends it, so launchd no longer starts its
  own daemon within a minute after a hand-started daemon ends. After a
  hand-started daemon ends, `deadband service install --restart` or the next
  login starts launchd's daemon. An installed agent gets the new plist at
  the next `deadband service install`.

### Fixed

- Exit 4 is listed only for `deadband daemon` in `deadband help exit-codes`
  and `docs/commands.md`, where it repeated in every command's section.
- `deadband status` says, when launchd has loaded the agent and no daemon
  answers, to see the log or run `deadband service install --restart`.
- A history rotation renames `jobs.jsonl` only when the writer's open file
  is still the file there. Another file that replaced it stays in place and
  receives the next line.
- A job that ends while its watcher reconnects to a new daemon no longer
  sends that daemon a `Seat`: the check for the end and the write of the
  `Seat` are one hold of the seat lock.
- `deadband service install` and `uninstall` take `service.lock` in
  `~/.local/state/deadband` whatever `XDG_STATE_HOME` says, so two commands
  that share a plist share the lock.
- A reclaim that makes the state directory now makes it with mode 0700, and
  a reclaim or a service command sets mode 0700 on a state directory that
  exists with a wider mode. Before, a reclaim that ran after the directory
  was removed, or in a daemon whose history thread was refused, left the
  directory with the mode the umask allowed, usually 0755.

## [0.3.1] - 2026-11-13

### Fixed

- A job whose lease the daemon already closed after `Done` is no longer
  seated again and recorded a second time as lost. The job's end now writes
  how the job ended, closes the connection and marks the lease ended in one
  step, so the watcher cannot seat the ended job on the same daemon, or on a
  restarted one, in the gap between the write and the close. A job that ends
  while the watcher is connecting, or after it has written its seat request
  and before the connection is installed, is still recorded by the new
  daemon as lost.
- When deadband cannot start the thread that watches a job's connection to
  the daemon, or cannot copy that connection for it, `deadband run` now
  prints `deadband cannot watch the daemon connection, so a restarted
  daemon will not count this job`. The job still runs as before.
- The daemon keeps recording job history after `jobs.jsonl` is removed or
  replaced. Before each line it checks that its open file is still the file
  at that path, and it opens the path again when it is not. Before, every
  later line went into the removed file and was lost.
- Two daemons no longer write one history directory after `jobs.lock` is
  removed. Before each line the daemon checks that its lock is still the
  file at that path, and it takes the lock again when it is not. When
  another daemon already holds the new lock, this daemon logs once that
  another daemon writes job history and records nothing while that daemon runs. Before, both
  daemons kept writing, and one daemon's rotation could overwrite
  `jobs.1.jsonl` that the other still wrote.

## [0.3.0] - 2026-11-13

### Changed

- A client that does not read the daemon's reply no longer stops admission.
  The daemon now writes each grant and queued reply after it releases the
  lock that guards the queue, so other threads keep admitting jobs while the
  write waits. A reply that a client has not taken within one second fails,
  and the daemon closes that connection. A `deadband run` that had read no
  queued reply then starts its job at once. One that had read a queued reply
  asks again every 100 ms for up to 5 seconds, is queued again by a daemon
  that is still serving, and starts its job once no daemon has kept it for
  those 5 seconds. Before, the daemon wrote replies while it held the queue
  lock, so each client that did not read stopped all admission for up to one
  second, and a client whose grant failed could wait without end.
- A daemon whose lock file (the socket's path with `.lock` added) is removed
  or replaced now stops serving within about a minute, and `deadband daemon`
  prints `the lock file is gone or replaced, so the daemon stops serving`.
  Before, a removed lock file let a second daemon start, remove the first
  one's socket and bind its own, so two daemons each counted only their own
  jobs.
- When the operating system refuses a thread, for example at the user's
  limit on threads or processes, no daemon thread panics. A refused sampler,
  watchdog or connection thread stops the daemon with the reason
  `the <name> thread could not start: <error>, so the daemon stops serving`,
  and launchd starts a new one while jobs start at once. A refused thread
  for a reclaim, an `on_stuck` command or a `/bin/ps` run is one log line,
  and the next attempt tries again. Before, a refused `/bin/ps` thread
  switched stuck detection off until the daemon restarted.
- `deadband service install` and `deadband service uninstall` hold a lock on
  `service.lock` in the state directory while they read and change the plist
  and launchd. A second command waits up to about ten seconds and then
  refuses with `deadband::service::locked`, which names the lock file, and
  changes nothing. Before, an install racing an uninstall could load an
  agent whose plist the uninstall then deleted.
- The launch agent's plist sets `ThrottleInterval` to 60 seconds, so a
  daemon that exits at start, because a daemon started by hand holds the
  socket, starts again once a minute instead of every ten seconds. Running
  `deadband service install` again writes the new plist. When a daemon that
  launchd did not start already answers on the socket, install says so.
- `deadband run` passes a SIGTERM or SIGINT that it receives on to its job,
  unchanged, keeps waiting, and exits with the job's status (143 or 130 when
  the signal ended it), so a `kill` of `deadband run` ends the job and the
  daemon records it as ended, not lost. A Ctrl-C at the terminal now reaches
  the job twice, and `deadband run` replaces an ignored SIGINT or SIGTERM
  that its caller passed down.
- A waiting `deadband run` whose daemon closes the connection now tries to
  reach a daemon every 100 ms for 5 seconds and asks again, so a restarted
  daemon queues the job instead of every waiting job starting at once. The
  job starts at once only when no daemon answers within the 5 seconds.
- A reclaim ends when its reclaimer exits, even when a background process
  of the reclaimer still holds its standard output open. Before, such a
  process kept the reclaim running and the daemon started no later reclaim.
- `deadband status` prints `reclaiming: <need>, started <age> ago` while a
  reclaim runs, and the status JSON gains `reclaiming` with the need and the
  start time.

### Added

- A daemon started with `DEADBAND_SOCKET` set ignores `config.toml`, logs one
  line saying so, and runs no reclaimer and no `on_stuck` command. A reclaim
  holds a lock on `reclaim.lock` in the state directory while it runs, so
  daemons of one user reclaim one at a time.
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
- `deadband service install` reports `daemon answers` or `daemon does not
  answer; see ~/Library/Logs/deadband.log` after it loads the agent, having
  asked the daemon up to 20 times, 100 milliseconds apart.
- `deadband service uninstall` and `install --restart` print how many jobs
  the daemon runs and how many wait before they unload the agent, and the
  uninstall question names them. They ask the daemon that launchd runs, and
  uninstall asks only when launchd has loaded the agent. Uninstall ends by
  listing the socket, lock, log, history and configuration that stay.
- When no daemon answers on macOS, `deadband status` says whether the agent
  is not installed, installed but not loaded, loaded but not answering, or
  could not be checked, and names `deadband service install` or the log.
  `deadband status --json` gives the state in `daemon_state`: `running`,
  `not_loaded`, `not_installed`, `loaded_silent` or `launchd_unasked`.
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
