---
$type: lifecycles/index/v1
stores:
- id: socket
  where: $DEADBAND_SOCKET, else $DEADBAND_SOCKET_DIR/deadband.sock, else on macOS deadband.sock in the
    per-user temporary directory that confstr _CS_DARWIN_USER_TEMP_DIR names, else $TMPDIR/deadband.sock,
    else /tmp/deadband-<USER>.sock
  kind: socket
  write: in-place
  lock: socket
- {id: history-file, where: $XDG_STATE_HOME/deadband/jobs.jsonl (default ~/.local/state/deadband/jobs.jsonl),
  kind: file, write: append, lock: history}
- {id: rotated-history, where: $XDG_STATE_HOME/deadband/jobs.1.jsonl, kind: file, write: atomic-replace,
  lock: history}
- id: history-lock-file
  where: $XDG_STATE_HOME/deadband/jobs.lock
  kind: file
  write: in-place
  lock: none
  holds_lock_files:
  - history
- id: daemon-state
  where: 'daemon process memory: the State behind Arc<Mutex<State>> in crates/deadband/src/daemon.rs'
  kind: process
  write: in-place
  lock: queue
- id: lease
  where: 'client process memory: SeatState behind the Seat mutex in crates/deadband/src/client/lease.rs'
  kind: process
  write: in-place
  lock: seat
- {id: run-process, where: 'the deadband run process in the kernel''s process table, and its nice value',
  kind: process, write: external, lock: none}
- {id: job-process, where: 'the job''s process, a child of deadband run, in the kernel''s process table',
  kind: process, write: external, lock: live}
- id: reclaimer-process
  where: a reclaimer named in $XDG_CONFIG_HOME/deadband/config.toml, a child of the daemon
  kind: process
  write: external
  lock: none
- {id: ps-process, where: '/bin/ps, a child of the daemon''s watch thread', kind: process, write: external,
  lock: none}
- {id: on-stuck-process, where: 'the on_stuck command of config.toml, a child of the daemon', kind: process,
  write: external, lock: none}
- {id: plist, where: ~/Library/LaunchAgents/io.github.crumbkit.deadband.plist, kind: file, write: atomic-replace,
  lock: service}
- {id: launchd-service, where: launchd's service gui/<uid>/io.github.crumbkit.deadband, kind: external,
  write: external, lock: service}
- {id: launch-agents-dir, where: ~/Library/LaunchAgents, kind: dir, write: in-place, lock: service}
- {id: launchd-tool-process, where: '/bin/launchctl, a child of deadband service or deadband status',
  kind: process, write: external, lock: none}
- {id: state-dir, where: '$XDG_STATE_HOME/deadband, with mode 0700', kind: dir, write: in-place, lock: none}
- id: skill-files
  where: deadband's skill in each agent skills directory that exists, as crumbkit_skills chooses
  kind: file
  write: external
  lock: none
- {id: completion-file, where: 'deadband''s shell completion file, as crumbkit''s completion module chooses',
  kind: file, write: external, lock: none}
- {id: terminal-mode, where: the controlling terminal's raw mode and alternate screen, kind: terminal,
  write: external, lock: none}
locks:
- {id: socket, kind: flock, where: '<socket path>.lock, made with mode 0600 and never removed', wait: 0s}
- id: history
  kind: flock
  where: $XDG_STATE_HOME/deadband/jobs.lock, held by the writer thread, which checks before each line
    that the file it locked is still at the path and takes the lock again when it is not; never removed
  wait: 0s
- {id: queue, kind: mutex, where: 'the std::sync::Mutex around State (Shared in crates/deadband/src/daemon.rs)',
  wait: unbounded}
- {id: seat, kind: mutex, where: 'the std::sync::Mutex in each Seat (crates/deadband/src/client/lease.rs)',
  wait: unbounded}
- id: service
  kind: flock
  where: $XDG_STATE_HOME/deadband/service.lock (default ~/.local/state/deadband/service.lock), held by
    deadband service install and uninstall for the whole command and never removed; the plist's path follows
    HOME instead
  wait: 10s
- id: reclaim
  kind: flock
  where: $XDG_STATE_HOME/deadband/reclaim.lock, held by a daemon's reclaim thread while it runs and never
    removed; a reclaimer does not inherit it
  wait: 0s
- id: live
  kind: mutex
  where: the Mutex<bool> live in crates/deadband/src/job/forward.rs, which says the job has not yet been
    reaped
  wait: unbounded
timeouts:
- id: first-line-within
  value: 2s
  where: daemon.rs FIRST_LINE_WITHIN
  note: the daemon closes a connection that sent no first line, before any job-entry exists
- id: write-timeout
  value: 1s
  fires: job-entry#undelivered
  where: daemon.rs WRITE_TIMEOUT
  note: a grant the client does not take; deliver writes it after the queue mutex is released. A failed
    Queued reply has its connection shut down, and the client's read then ends in job-entry#leave
- id: answer-within
  value: 2s
  fires: job-process#start
  where: client.rs ANSWER_WITHIN
  note: a client with no first answer starts its job at once (the run-open flow)
- id: reask-for
  value: 5s
  fires: job-process#start
  where: client.rs REASK_FOR
  note: a queued client whose connection closed starts its job at once when no daemon has kept it this
    long after the first close (the run-open flow)
- id: reask-every
  value: 100ms
  fires: job-entry#ask
  where: client.rs REASK_EVERY
  note: the pause between a queued client's tries to reach a daemon after its connection closed
- id: retry-every
  value: 100ms
  fires: job-entry#seat
  where: client/lease.rs RETRY_EVERY
  note: a lost lease tries to seat on the next daemon; a lease that ends while lost is also noticed within
    this time
- id: write-within
  value: 2s
  where: client/lease.rs WRITE_WITHIN
  note: a seat the daemon does not take counts as failed, and the lease tries again after retry-every
- id: sample-every
  value: 1s
  fires: job-entry#grant
  where: daemon.rs SAMPLE_EVERY
  note: the sample flow, which may also run job-entry#undelivered and reclaim#start
- id: alive-check
  value: 60s
  fires: daemon#end-orphaned
  where: daemon/alive.rs CHECK_EVERY_SAMPLES x sample-every
  note: the sampler checks the socket file and the lock file every 60 samples; a gone or replaced lock
    file fires daemon#stop instead
- id: stale-after
  value: 5s
  where: daemon/alive.rs STALE_AFTER_SAMPLES and STALE_FLOOR
  note: five sampling intervals, and at least 250 ms, with no new reading; admission then treats the probe
    as failed, and the watchdog checks the socket and lock files
- {id: machine-every, value: 60s, fires: history-file#append, where: history.rs MACHINE_EVERY, note: a
    machine line}
- id: watch-every
  value: 15s
  fires: job-entry#track
  where: daemon/settings.rs
  note: the watch flow, which may also run become-stuck and become-active
- {id: stuck-after, value: 5m, fires: job-entry#become-stuck, where: stuck.rs DEFAULT_AFTER, note: 'config
    key stuck_after, at least 1 minute'}
- id: census-within
  value: 5s
  where: daemon/settings.rs
  note: the watch thread stops waiting for /bin/ps; ps-process#reap runs later on the reaper thread
- {id: census-every, value: 60s, fires: ps-process#spawn, where: daemon/settings.rs, note: another /bin/ps
    while a job stays stuck}
- {id: on-stuck-within, value: 60s, where: daemon/settings.rs, note: one log line; on-stuck-process#reap
    still waits for the command}
- id: reclaim-timeout
  value: 5m
  where: reclaim.rs DEFAULT_TIMEOUT
  note: one log line; reclaimer-process#reap and reclaim#finish still wait for the reclaimer
- id: reclaim-after
  value: 60s
  fires: reclaim#start
  where: reclaim.rs DEFAULT_AFTER
  note: config key reclaim_after, at least 10 seconds; memory must be short this long, and each resource
    rests this long after a reclaim, doubled after each reclaim in a row that freed nothing, up to the
    longer of MAX_REST (30 minutes) and reclaim_after (reclaim/watch.rs:34, 75)
- id: drain-grace
  value: 250ms
  where: job/reclaimer.rs DRAIN_GRACE
  note: how long a reclaimer's run waits for its output after the reclaimer exits; a background process
    that still holds the pipe is not waited for
- id: unload-wait
  value: 5s
  fires: launch-service#bootstrap
  where: cli/service/daemon.rs UNLOAD_POLLS x job/launchd.rs PAUSE
  note: 50 polls 100 ms apart, after which install bootstraps whatever launchd says
- id: service-lock-wait
  value: 10s
  where: cli/service/lock.rs LOCK_POLLS x job/launchd.rs PAUSE
  note: a service command looks for service.lock 100 times, 100 ms apart, and then refuses with deadband::service::locked
- id: answer-wait
  value: 2s
  where: cli/service/daemon.rs ANSWER_POLLS x job/launchd.rs PAUSE
  note: install asks the socket up to 20 times after bootstrap and reports whether the new daemon answers
- id: throttle-interval
  value: 60s
  where: cli/service/plist.rs THROTTLE_SECONDS, read by launchd
  note: launchd starts the daemon at most once in this time, so a daemon that exits at start is restarted
    once a minute
bends:
- daemon-state and lease are memory inside a process. Store kinds are file, dir, process, socket, terminal
  and external, so both are kind process, because the memory lives and dies with its process.
- A process store names a kind of process, and each run makes a new one. Their unguarded reasons say why
  two different processes do not conflict; the format's seams assume one shared record.
- State kinds are absent, stored and inferred, so a live process, a terminal mode and an in-memory value
  are marked stored.
- An atomic rename changes two stores, jobs.jsonl and jobs.1.jsonl. An effect writes only its entity's
  stores, so the rename is history-file#rotate plus a rotated-history transition, listed as two steps
  that cannot happen apart. Draft 4 lists this as still open.
- Steps cannot be optional. A step that runs only sometimes (repair, the bootout when the agent is not
  loaded, the plist write with --restart and an unchanged plist, rotation after an append, the sample
  flow's check step, which stops the daemon only when a file is gone, history-first's lock step, which
  runs only when jobs.lock is missing (daemon/bind.rs:137), each step of sample and watch) is listed as
  if it always runs, and the flow's prose says when it does not.
- Steps cannot loop. reclaim-run shows one reclaimer, and the service flows show one launchctl
  run, though each repeats.
- 'Crash points cannot say whose process crashed. The run flow has two after: ask points, one for the
  client and one for the daemon, and the leaves text says which.'
- Daemon death ends every in-memory entry in every state at once and no code runs it, so it is a crash
  point of the daemon flow whose leaves_state names job-entry#gone, not a transition from each state.
- 'A transition cannot say which instance it acts on: job-entry#evict drops the oldest ended job, not
  the job of the run flow that runs it.'
- 'exists_in names a state but not a condition: the stuck region exists only for running jobs that deadband
  run marked watched and that stuck_ignore does not match.'
- One entity cannot copy its state into another. The job inherits deadband run's nice value at fork, which
  run-process says in prose.
- lease#grant builds the seat before any other thread can see it, so it takes no lock on a locked store.
  It is finding 4, and its unguarded says why it is safe.
- 'A step cannot refuse and stop: a history line whose writer finds jobs.lock held by another daemon stops
  after the claim-dir step and is lost, and a reclaim that finds reclaim.lock held is a flow of its own
  (reclaim-skipped).'
- A timeout that fires no transition, such as one that only logs, leaves fires out, and one that starts
  a flow names that flow's first transition.
- 'The socket''s lock file is not a store, because its directory is not deadband''s, so its removal by
  something outside deadband has no transition; daemon#usurp is the bind that follows it, and the first
  daemon''s check stops it within 60 samples. service.lock and reclaim.lock are not stores either: deadband
  makes them on first use and never removes them, and nothing in deadband depends on their removal.'
- 'if_missing has no value for a write that goes to a removed file. history-file#append says if_missing:
  fail, because it writes to the open descriptor; the writer checks the path before each line, so only
  a line written between that check and a removal goes to the removed file (history/writer.rs:248-259).'
---
# deadband's lifecycle registry

This registry lists the state that deadband creates, changes or removes. It
follows draft 4.1 of the lifecycle registry format. The files are checked and
drawn by `lifecycle-view`, a script kept outside this repository in
`scripts/lifecycle-view` of the owner's board. Run it from the repository root
as `uv run --script <board>/scripts/lifecycle-view/lifecycle-view . -o <file>.html`;
it exits 0 when it accepts the files and names each refusal otherwise.

Most of deadband's state is not a file. The daemon keeps its queue, its stuck
records and its reclaim mark in memory. A client keeps its lease in memory.
The jobs, reclaimers, `/bin/ps`, `on_stuck` and `launchctl` are processes. The nice value of `deadband run` is a process attribute, and the
raw mode that `deadband top` sets is a terminal setting. Each of these is a
store above, with a `kind` that says which. The `timeouts` above list each
timeout with the transition it fires, and the `bends` list each place where
this registry stretches the format.

deadband never pauses or resumes a job yet. Pausing is planned in
docs/architecture.md, so no entity has a paused state.

deadband reads these files and never writes them, so they are not stores:
`$XDG_CONFIG_HOME/deadband/config.toml`, which the daemon reads once at
start, and the priority file in the same directory, which the daemon reads
every second and `deadband run` reads once. launchd writes the daemon's
standard error to `~/Library/Logs/deadband.log`. deadband makes four lock
files on first use and never removes them: the socket's lock file, and
`jobs.lock`, `service.lock` and `reclaim.lock` in the state directory.
`jobs.lock` is the store history-lock-file; the other three are a bend.

The daemon also changes fields of its own each tick that are not drawn as
states: the latest reading, whether the priority file holds an entry, each
running job's tally, the time of the last machine line, the count of dropped
history lines, and each resource's reclaim back-off.

## Protocol messages

| Message | Sent by | Transition it drives |
| --- | --- | --- |
| `Ask` | client | `job-entry#ask` |
| `Queued` | daemon | none; the client waits, and asks again for 5 seconds if the connection then closes |
| `Granted` | daemon | `job-entry#grant` before it is sent, `lease#grant` when it arrives |
| `Seat` | lease watcher | `job-entry#seat` |
| `Done`, or the connection closing | client | `job-entry#end`, or `job-entry#leave` for a waiting job |
| `Stuck`, `Active` | daemon | none; a notice line at the client, after `job-entry#become-stuck` or `become-active` |
| `Status`, `Timeline` | `status`, `top` | none |

## Entities

- [completion](lifecycles/entities/completion.md): deadband's shell completion file, where the shell loads it.
- [daemon](lifecycles/entities/daemon.md): The daemon's socket file, and whether a daemon listens on it.
- [history-lock](lifecycles/entities/history-lock.md): The lock file jobs.lock in the state directory.
- [history-file](lifecycles/entities/history-file.md): The job history file jobs.jsonl in the state directory.
- [job-entry](lifecycles/entities/job-entry.md): One job in the daemon's queue, from asking to its place in the ring of the last 64 ended jobs.
- [job-process](lifecycles/entities/job-process.md): The job that deadband run starts as its child.
- [launch-plist](lifecycles/entities/launch-plist.md): deadband's launch agent plist, and the LaunchAgents directory that holds it.
- [launch-service](lifecycles/entities/launch-service.md): The launchd service gui/<uid>/io.github.crumbkit.deadband, which runs the daemon.
- [launchd-tool-process](lifecycles/entities/launchd-tool-process.md): One run of /bin/launchctl by deadband service or deadband status.
- [lease](lifecycles/entities/lease.md): A granted client's connection to the daemon, which its watcher thread seats again on the next daemon.
- [on-stuck-process](lifecycles/entities/on-stuck-process.md): One run of the on_stuck command that config.toml names, for a job that became stuck.
- [ps-process](lifecycles/entities/ps-process.md): One run of /bin/ps that names the busiest process outside the jobs.
- [reclaim](lifecycles/entities/reclaim.md): The daemon's mark that a reclaim runs, and the last reclaim that deadband status reports.
- [reclaimer-process](lifecycles/entities/reclaimer-process.md): One run of a reclaimer that config.toml names, as a dry run or a real run.
- [rotated-history](lifecycles/entities/rotated-history.md): The rotated job history file jobs.1.jsonl.
- [run-process](lifecycles/entities/run-process.md): The deadband run process, and the nice value that its job inherits.
- [skill](lifecycles/entities/skill.md): deadband's agent skill, a copy in each agent skills directory that exists.
- [state-dir](lifecycles/entities/state-dir.md): deadband's state directory, $XDG_STATE_HOME/deadband, and its mode 0700.
- [terminal](lifecycles/entities/terminal.md): The controlling terminal's raw mode and alternate screen while deadband top runs.

## Flows

- [daemon](lifecycles/flows/daemon.md): deadband daemon
- [socket-unlinked](lifecycles/flows/socket-unlinked.md): outside: something removes the socket file while the daemon listens
- [run](lifecycles/flows/run.md): deadband run --label <label> -- <command>, with a daemon that answers
- [run-open](lifecycles/flows/run-open.md): deadband run, when no daemon answers within 2 seconds, or no daemon keeps a queued job within 5 seconds of its connection closing
- [run-abandoned](lifecycles/flows/run-abandoned.md): deadband run that is interrupted while its job waits
- [reseat](lifecycles/flows/reseat.md): the watcher thread of a granted deadband run, after the daemon's connection closes
- [run-ends-while-lost](lifecycles/flows/run-ends-while-lost.md): a granted deadband run whose job ends while no daemon holds its seat
- [sample](lifecycles/flows/sample.md): the daemon's sampler thread, once a second
- [reclaim-run](lifecycles/flows/reclaim-run.md): the daemon's reclaim thread, started by the sampler
- [reclaim-skipped](lifecycles/flows/reclaim-skipped.md): the daemon's reclaim thread when another daemon holds reclaim.lock, or the sampler when the reclaim thread is refused
- [watch](lifecycles/flows/watch.md): the daemon's watch thread, every 15 seconds while a watched job runs
- [history-first](lifecycles/flows/history-first.md): the daemon's history writer thread, for its first line
- [history-reopen](lifecycles/flows/history-reopen.md): the daemon's history writer thread, for a line after a failed write or a rotation closed the file, or after the writer found jobs.jsonl or jobs.lock removed or replaced
- [history-append](lifecycles/flows/history-append.md): the daemon's history writer thread, for a line while the file is open
- [history-rotate](lifecycles/flows/history-rotate.md): the daemon's history writer thread, after a write took jobs.jsonl past 64 MiB
- [history-lock-unlinked](lifecycles/flows/history-lock-unlinked.md): outside: something removes jobs.lock while a writer holds it
- [history-unlinked](lifecycles/flows/history-unlinked.md): outside: something removes jobs.jsonl while the writer holds it open
- [service-install-reload](lifecycles/flows/service-install-reload.md): deadband service install, over a loaded agent whose plist changed or is missing, or with --restart
- [service-install-load](lifecycles/flows/service-install-load.md): deadband service install, over an agent that launchd has not loaded
- [service-uninstall](lifecycles/flows/service-uninstall.md): deadband service uninstall
- [top](lifecycles/flows/top.md): deadband top
- [skills-install](lifecycles/flows/skills-install.md): deadband skills install
- [completion-install](lifecycles/flows/completion-install.md): deadband completion install
