# Changelog

This file records the notable changes to deadband. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and deadband
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `starmap.toml` declares the environment variables that deadband reads, and
  `mxv.toml` declares the daemon as the process `deadband`, on a socket of its
  own in the checkout, with tasks for the commands of the pull-request lane.
  A test checks that `starmap.toml` names exactly the variables that the
  code lists.

### Changed

- deadband takes its four lock files, the daemon's `<socket>.lock`,
  `service.lock`, `reclaim.lock` and `jobs.lock`, through crumbkit-durable
  0.6.1's `PathLock`. The files keep their names, places and mode 0600, and
  every refusal keeps its code and exit status. A lock file that another user
  owns is still refused; the reason now reads `<path> is owned by user <uid>,
  not user <uid>`.

### Fixed

- A daemon whose lock file was replaced after it locked the file and before
  it started serving now stops at its next alive check. Before, it recorded
  the new file as its own and kept serving while a second daemon could lock
  the new file.
- When another program replaces a lock file while deadband is locking it,
  deadband now opens and locks the new file, up to three times, instead of
  holding a lock on a file that is no longer at the path.

## [0.4.0] - 2026-11-17

### Added

- `deadband top` shows reclaims. A `reclaims` section holds one timeline row
  for each configured reclaimer, the line above the rule gives the plan of a
  running reclaim or the reason the last one failed, the header adds
  `RECLAIMING` or `RECLAIM FAILED`, and the line that says why jobs wait
  says when the next reclaim starts. The daemon keeps the runs of the last
  ten minutes in memory. A reclaimer's report may mark an item `state:
  "kept"` with a `reason`, and `top` lists it in the plan.
- `deadband status` prints the free space of the disk that holds the home
  directory on the line after the load, such as `disk: 212 GiB free of 994
  GiB (21%)`, or `disk: could not be read (<reason>)` when the disk cannot be
  read. `deadband status --json` gives it in a `disk` object with
  `free_bytes` and `total_bytes`, or `null` when it could not be read.
- `deadband top` shows a `disk` row under the `load` and `cores` rows with
  the free space in whole GiB, read at each poll. The value is red while the
  disk is under the level at which the daemon holds jobs, and `-` when the
  disk cannot be read.

### Changed

- In the library, `probe::Sample` no longer implements `Display`;
  `cli::Status` writes the status text and has a `disk` field.
  `probe::Disk` derives `Serialize` and `JsonSchema` and gains
  `free_percent`.
- In the library, a command is a `job::CommandLine` with a `program` and
  `args`. `Reclaimer::command` and `Watching::census` are `CommandLine`,
  `stuck::Config::on_stuck` is `Option<CommandLine>`, and `job::run`,
  `reclaimer::run`, `on_stuck::run` and `Census::read` take one, so they no
  longer return `InvalidInput` for an empty command. `busiest::PS_COMMAND`
  is replaced by `busiest::ps_command()`. The config file is unchanged: an
  empty `command` or `on_stuck` is still refused, and a config without
  `on_stuck` still runs none.
- In the library, `reclaim::Reclaimer` no longer derives `Deserialize` or
  `JsonSchema`. The config file's own reclaimer form is private.
- When the operating system refuses the thread that reads a reclaimer's
  output, the reclaimer does not start and the reclaim fails with the
  operating system's error, as it already did when the waiting thread was
  refused. The waiting thread no longer reads the output itself in that case.
- A SIGTERM, SIGINT, SIGHUP, SIGQUIT, SIGUSR1 or SIGUSR2 that `deadband run`
  inherited as ignored is no longer registered or passed on, so it stays
  ignored and the job inherits the ignore. Under `nohup deadband run --
  <job>`, the job survives a hangup. Before, `deadband run` replaced an
  ignored SIGINT or SIGTERM with its own handler, so a background job of a
  non-interactive shell script ended on a SIGINT that it would otherwise have
  ignored; now a `kill -INT` of a `deadband run` that a script started in the
  background no longer ends it.
- In the library, `protocol::Job` has a `beats` field, `Reply::Queued` has a
  `beats` field, `Reply` has a `Beat` variant and `client::Unasked` has a
  `Hung` variant. `admission::admit` always asks for beats. On the wire both
  fields are left out when false, so an older peer reads the same lines.
- deadband reads and prints ages with jiff in place of humantime, which
  RUSTSEC-2026-0542 marks unmaintained. Every age that the docs use, such as
  `"60s"`, `"5m"`, `"1h 30m"`, `"2 hours"` and `"1d"`, reads as before, and
  log and status text prints ages as before, such as `1m`, `50m`, `1h 30m`
  and `1day 2h`. Config ages now also accept a comma between units such as
  `"1h, 30m"` and a leading `+` such as `"+5m"`. Config ages in months or
  years, such as `"1M"` or `"1y"`, are now refused, and so are ages with
  units out of order or repeated, such as `"30m 1h"` or `"1m 1m"`. Ages of
  31 days or more print in days, for example `31days`, where they printed
  months. A refused age keeps its message's start and gives jiff's reason.
- `deadband skills install`, `check` and `print` name deadband in their help
  and examples, such as "Install deadband's skills into each agent skills
  directory that exists", in place of "this tool's skills".
- The failure in the JSON documents of `deadband status`, `service install`
  and `service uninstall` may hold crumbkit 0.6.0's new recovery steps, a
  `program` step and an `after` step, and a `causes` list. Their schemas
  gain these as options and drop nothing.

### Fixed

- A reclaimer that never exits no longer stops every reclaim for good.
  After 10 times `reclaim_timeout`, its ceiling, the daemon logs one line
  that names the reclaimer and its process ID, leaves it running, and lets
  the next reclaim start; `status` and `top` show the reclaim as failed with
  `ran past its ceiling of <ceiling>` instead of a reclaim in progress. If
  the reclaimer exits later, its output is discarded and its exit is logged
  once. In the library, `job::reclaimer::run` takes the ceiling and a
  callback for such a late exit, and `Ended` has an `Abandoned` variant.
- The daemon writes no log line and checks no file while it holds its queue
  lock. The watchdog used to check the socket file and write the line about
  a stale reading with the lock held, and a line that found the history
  thread stopped was written the same way. When the daemon's standard error
  blocked, as a stopped terminal or an unread pipe does, no job was granted
  or queued and `deadband status` got no reply. The watchdog now writes both
  lines after it releases the lock, and it may now grant the jobs that a
  stale reading lets start in the same pass that finds the socket file gone.
  In the library, `history::Recorder::send` no longer writes a log line, and
  the new `Recorder::say_stopped` writes the line that the writer has
  stopped, once, when a line has found it stopped.
- The daemon keeps its log file under 8 MiB. When its standard error is a
  regular file opened for appending, as launchd opens
  `~/Library/Logs/deadband.log`, and that file is longer than 8 MiB at start
  or at the check once a minute, the daemon empties it and logs one line
  saying so. The log used to grow without limit.
- The daemon keeps a reclaimer's whole report when the machine runs the
  thread that reads it late. Once the reclaimer exits, the daemon reads what
  its standard output already holds, where it used to stop reading 250 ms
  after the exit and count a cut-off report as failed.
- `deadband run` passes a SIGHUP, SIGQUIT, SIGUSR1 or SIGUSR2 that it
  receives on to its job, as it already did for SIGTERM and SIGINT, and exits
  with the job. A SIGHUP sent to the process of `deadband run` alone, such as
  a `kill -HUP` from a harness, used to end `deadband run` and leave the job
  running, so the daemon freed the job's place while the job ran uncounted.
- A queued `deadband run` starts its job when its daemon stops sending
  beats. The daemon sends a `beat` line every 5 seconds to each waiting job
  whose `Ask` set `beats`, and says so in its `Queued` reply. When no line
  arrives for 16 seconds, `deadband run` says that the daemon queued the job
  and then sent nothing, and runs the job. A daemon that is stopped, or whose
  threads block while they hold its queue, used to keep every queued run
  waiting for ever. A daemon of an earlier release sends no beats, and a new
  client waits on it with no deadline, as before.
- The `Ask` line and the daemon's first answer share the 2-second deadline.
  A daemon that stopped reading used to block a run whose command line was
  longer than the socket's buffer, and a daemon that sent its answer a byte
  at a time could keep a run waiting for minutes.
- Each loss of a daemon that queued a job and sent it a beat gets its own
  5-second window to ask again. A second daemon restart more than 5 seconds
  after the first used to start every queued job at once.
- `commands list` and `docs/commands.md` give `deadband status` its exit 1,
  with `deadband::job::stuck` and `deadband::job::unreadable`, so an agent
  can tell a stuck job from a crash. They list exit 3, which says deadband
  could not read the machine, only for `status`, the one command that exits
  with it, where they used to list it for every command.
- The waits of `deadband service install` and `uninstall` end at a time:
  two seconds for the new daemon to answer, five seconds for launchd to
  unload the old one and ten seconds for another service command's lock.
  They used to count polls, so a slow `launchctl print` or a daemon that
  accepted the connection and never replied stretched a wait, the answer
  wait to as much as 42 seconds.

## [0.4.0-alpha.1] - 2026-11-14

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
- Before connecting, `run`, `status` and `top` refuse a socket whose directory
  belongs to another user or that others can write to (unless it is sticky),
  and the connection no longer waits for a listener whose queue is full, so a
  listener planted by another user cannot hold a job back.
- The daemon, the history writer and the service commands open their lock
  files without following a symlink and refuse a lock file that another user
  owns.
- The daemon refuses a job whose label holds a control character, such as a
  line feed, and closes its connection, so the client runs the job at once.
  `deadband run` and `admit` replace each control character in a label with
  a space before they ask, so their jobs are still admitted. `status` prints
  the command line that an older daemon sends with each control character
  escaped, so a label or a command line can no longer add a forged row to
  `status`. The daemon now keeps the first 64 bytes of a label, as `top` and
  the history file already showed it, so `status` and the stuck hook see the
  same label.

### Changed

- deadband depends on crumbkit, crumbkit-skills and crumbkit-test 0.5.0, and
  CI installs cargo-crumbkit 0.5.0 and runs `tool-skill check` and
  `tool-skill draft --check`. The `skills install`, `skills check` and
  `skills print` commands come from crumbkit-skills, so their help text is
  crumbkit-skills' generic wording, and the rows of `skills install --json`
  and `skills check --json` gain a `skill` field. Their flags, effects and
  exit codes are unchanged. In the library, `deadband::cli::SkillsCommand` is
  crumbkit-skills' type, and its `Print` variant takes `SkillsPrint`.

- In the library, `Job::label` and `StuckJob::label` are `Label`,
  `StuckJob::quiet` is a `Duration`, `Reply::Granted`, `Request::Seat`,
  `Limits::parallelism` and `Lease::threads` (which was `Lease::jobs`) use
  `Threads`, `JobId` is its own type, and `name` takes `Label` and `Short`.
  `Running::recent_jobs` is renamed `recent_threads`, and `Queue::remove`
  returns a running job's threads, which replaces `Queue::threads_of`.
  `Arrival::read` takes the seated job's age as an `Ago`. The JSON on the
  socket and in `status --json` is unchanged.

- `deadband daemon` now exits 0, where it exited 4, when another daemon
  already serves the socket. It still prints the `deadband::daemon::running`
  refusal. The launchd plist now keeps the daemon alive only when it exits
  with a nonzero status or a signal ends it, so launchd no longer starts its
  own daemon within a minute after a hand-started daemon ends. After a
  hand-started daemon ends, `deadband service install --restart` or the next
  login starts launchd's daemon. An installed agent gets the new plist at
  the next `deadband service install`.

### Fixed

- Two watched jobs that report one process ID each get their own stuck
  notice and stuck periods, where the first job's connection received both
  notices. A waiting job that leaves no longer ends the stuck record of a
  running job with the same process ID. `deadband status` names each stuck
  job by its own running line. The daemon's list of jobs, and the running,
  waiting and stuck rows of `status --json`, gain an `id` field with the
  job's number in the daemon's queue. `status` matches a stuck row to its job
  by that number, and by process ID for an older daemon that sends none.
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
