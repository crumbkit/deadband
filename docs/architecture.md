# Architecture

deadband keeps a busy machine usable while many heavy jobs, such as cargo
builds and test runs, compete for it. This document has two parts. The first
part, "What is built", describes the source as it is now. The second part,
"Planned", holds the agreed design for features that the source does not yet
contain, so nothing in it can be relied on today.

# What is built

## Overview

deadband is one binary with a small per-user daemon. A job asks the daemon for
admission over a Unix socket before it starts. If the daemon is not running,
the job runs at once.

The daemon owns the queue of waiting jobs and the list of running ones. It
samples the machine once a second and grants waiting jobs as the admission
rule allows. The client stays small. It asks for admission, waits, starts the
job with parallelism variables set for the job, and reports the job's exit
status.

```text
          deadband run --label cargo-test -- cargo nextest run
                               |
                               v
 +--------------+  ask/grant  +-----------------------------+
 | client (cli) |------------>| daemon                      |
 +--------------+ Unix socket |                             |
        |                     |  probe --> admit (queue)    |
        | starts the job      |                             |
        v                     |  reclaim --> reclaimers     |
   job process                +-----------------------------+
```

## Modules

| Module | Owns |
| --- | --- |
| probe | Reads CPU load, memory pressure, swap and disk space from macOS every second, and the CPU time and disk bytes of a job's process tree, behind a trait so a Linux probe can be added later. It also reads what the kernel reports of a job's waited-for children. |
| admit | Decides whether a waiting job starts now and how many threads it gets, and holds the queue with its fast and normal lanes. The rule holds a job on the job count or the lane's places even when the probe fails, and on critically low disk even when nothing runs. Past those checks it admits a job when nothing runs or the probe fails, and otherwise holds it on memory pressure or the load. |
| admission | One public call, `admit`, that does what `deadband run` does before it starts a job: it chooses the lane, asks the daemon, waits, and returns the job's lease and variables. It writes no log, so a program that embeds deadband passes a callback. |
| daemon | Listens on the socket, samples the probe once a second, and grants waiting jobs as admit allows. It remembers the last 64 jobs that ended, in memory only, for the timeline request. Its watch thread reads the process tree of each job that `deadband run` started. The daemon hands a line about each job that ends to history. |
| stuck | The pure rule that says when a watched job counts as stuck, the lines that report it, and the `stuck_after`, `stuck_ignore` and `on_stuck` config keys. |
| history | The record types, the rule that turns a command line into a kind, and one writer thread that appends each line to a private file. Nothing in deadband reads the file. |
| serve | One public call, `run_daemon`, that does what `deadband daemon` does before it prints a failure: it binds the socket, reads the reclaim config and serves. |
| client | Asks the daemon to admit a job or to list its jobs, and gives the reason to start at once when the daemon cannot be asked. A granted job's lease has one thread that tells the next daemon the job still runs when the connection closes. |
| protocol | The messages over the socket, one JSON object per line. |
| name | The rule that names a job in `status` and `top` from its workspace, its label and its short command, and leaves the label out when it repeats the workspace or the command's first word. |
| socket | Finds the socket's path from the environment, and lists the variables deadband reads. |
| job | The only module that starts a process: the job that `deadband run` was given, the reclaimers and the `on_stuck` command that the daemon's config file names, `/bin/ps`, which names the busiest process outside the jobs, and `launchctl`, behind a trait so tests use a fake launchd. |
| reclaim | Reads the daemon's config file, decides when to reclaim memory or disk, runs the reclaimers on a thread of their own, and keeps the last reclaim for `deadband status`. |
| environment | Reads the variables that `deadband run` uses, and gives a granted job its parallelism variables. |
| priority | Chooses a job's lane from the priority file. |
| throttle | The only module that changes the priority of a job. `deadband run` lowers its own priority to nice 5 before it starts a normal-lane job, and the job inherits it. |
| cli | The commands `deadband run`, `status`, `daemon`, `service` and `top`, built on crumbkit. Its service module renders the launch agent's plist as a pure function and moves the agent between not installed, installed but not loaded, and loaded. Install and uninstall hold a lock on `service.lock` in the state directory while they read and change the plist and launchd, and the plist's `ThrottleInterval` keeps launchd from restarting a daemon that exits at start more than once a minute. [running.md](running.md) says how to install and run them. |
| tui | `deadband top`, behind the default-on Cargo feature `tui`: the terminal guard, the poll loop, a pure model and a ratatui view. It reads the daemon once a second through a request of its own, and draws its load graph from the local probe. |

## Rules that shape the code

- Only the throttle module changes a job's priority. clippy's
  `disallowed-methods` refuses those calls everywhere else.
- deadband never ends a job of its own choosing and sends no signal of its own
  choosing to one. The one exception, decided by the owner on 2026-11-13, is that
  `deadband run` passes a SIGTERM or SIGINT it received on to its job,
  unchanged, and only the job module (`job/forward.rs`) does it. clippy's
  `disallowed-methods` refuses `kill_process` everywhere else.
- When the daemon cannot be reached or does not answer, the job runs at once
  and `deadband run` says why in one line on standard error. When a daemon
  closes the connection of a queued job, `deadband run` asks again for 5
  seconds, and the job runs at once only if no daemon answers in that time. A
  daemon that hangs while it keeps the connection open still keeps a queued
  job waiting, as [running.md](running.md) says.
- When the probe cannot read the machine, or the sampler has stored no reading
  for five sampling intervals (at least 250 ms), the daemon starts each job
  once a place is free, and its log says why in one line.
- The daemon stops serving when the socket path no longer names the socket it
  bound or the lock path no longer names the lock file it holds (both checked
  once every 60 samples), when its sampler panics, or when the operating
  system refuses its sampler, watchdog or a connection's thread. The daemon
  starts every thread through `std::thread::Builder` and handles the refusal,
  so a refused thread never panics. In a
  release build a panic in any thread aborts the process. Launchd then starts
  a new daemon, and clients start their jobs at once while none listens.
- A failure to record a job never delays, holds or ends that job. The daemon
  does no file I/O while it holds the queue lock.
- No source file outside tests is longer than 400 lines.

## Reclaiming memory and disk

When the machine runs short of memory or disk, the daemon can ask external
reclaimers to free some, such as a tool that stops the servers and
containers of workspaces nobody is using. The daemon's config file lists
reclaimers as commands, so deadband links no such tool and does not know
what a reclaimer stops. The reclaimer chooses.

The reclaim module holds the rule that says when to reclaim, as a pure
function of what the daemon reads each second. Memory is short while
memory pressure is above normal or a job waits for memory, and a reclaim
starts once it has been short for `reclaim_after`. Critically low disk
starts a reclaim at once. At most one reclaim runs at a time. Memory and
disk each wait `reclaim_after` after their last reclaim, and each reclaim
in a row that frees nothing doubles that wait, up to 30 minutes.

A reclaim runs on a thread of its own, so admission never waits for it.
For each reclaimer in turn, the daemon runs the command with
`--need memory=4G --dry-run`, logs the plan in one line, and then runs it
without `--dry-run`. It stops once the bytes freed cover the need. A
reclaimer that fails or runs past its timeout is logged in one line and
holds no job. deadband never signals or stops a reclaimer; past its
timeout the daemon stops waiting for it, and starts no other reclaim until
it exits. [running.md](running.md) gives the config and the JSON a
reclaimer prints.

## Detecting stuck jobs

A job can hold a place while it does nothing, for example when its
processes wait in dyld while syspolicyd scans them. The daemon watches the
process tree of each job that `deadband run` started, and says when one has
shown no activity.

The watch thread wakes every 15 seconds and makes no process call while no
watched job runs. It copies the running jobs under the queue lock, then
reads their trees through `Probe::tree` with the lock released, applies the
rule in `stuck` under the lock, and writes each notice with the lock
released, so admission never waits for a reading. The daemon holds a job's
last reading, its last activity and its last four stuck periods in memory
and keeps those periods with the 64 ended jobs that it remembers.

Activity is any of three things accumulated across the tree since the last
activity: one second of CPU, one mebibyte of disk read or written, or a
change in the tree's set of process IDs. A job with no activity for
`stuck_after`, 5 minutes by default, is stuck. The rule adds only the
positive change of each process, so a child that exits cannot lower the
total. A child owned by another user reads as zero, so its use does not count.

When a job becomes stuck, and at most once a minute while one stays stuck,
the job module runs `/bin/ps` on a thread of its own with a 5-second
deadline. libproc refuses an unprivileged daemon the CPU use of a root
process, and `/bin/ps` is setuid root. At most one runs at a time, and
deadband never signals it. It names the busiest process outside the jobs
when that process uses at least 50% of one core.

deadband never ends, pauses or signals a stuck job, and detection changes
neither admission nor a job's priority or streams. A stuck job keeps its
place. Every failure leaves the job running and writes one log line: a failed
read of the process table leaves the job unflagged, and `deadband status`
then exits 1 with `deadband::job::unreadable`; a platform with no reader
flags nothing and exits 0; a failure of `/bin/ps` names no process and the
job stays flagged.

Only `deadband run` sets `watched` on its job. A program that embeds deadband
holds the lease in its own process, whose tree is not the job's tree, so it
passes no notices and its job is not watched. A daemon restart keeps
`watched`, because `Request::Seat` carries the `Job`.

### The lifecycle of a watched job

| State | Meaning | Enters when | Leaves when |
| --- | --- | --- | --- |
| Watching | The job runs and its tree shows activity within `stuck_after` | The job is granted or seated with `watched` set, or Stuck sees activity | No activity for `stuck_after` goes to Stuck; `Done` or a closed connection ends the record |
| Stuck | The tree has shown no activity for `stuck_after` | Watching passes `stuck_after` | Activity goes to Watching; `Done` or a closed connection ends the record |

A job that a client does not mark `watched`, or whose command matches
`stuck_ignore`, has no record. A waiting job has no record.

| Crash or fault | Result |
| --- | --- |
| The daemon exits | The records are lost. Jobs keep running, because the daemon never touches them. Each lease seats its job on the next daemon, which starts it in Watching with a fresh timer. |
| The client exits | The connection closes and the record ends. |
| The watch thread panics | Detection stops and admission continues. Every job shows as running. |
| `/bin/ps` hangs | The watch thread waits at most 5 seconds for it, then names no process and goes on sampling. |
| The Mac sleeps | The monotonic clock does not advance, so sleep adds no quiet time. |

## The job history

Each daemon appends one line of JSON for every job that ends, and one line
about the machine every 60 seconds, to a private file on that machine.
[running.md](running.md) gives the file's place and format.

The daemon is the only writer. Its writer thread takes an exclusive lock on
`jobs.lock` in the state directory before it opens the file and holds it while
the daemon runs, so one daemon writes each state directory even when two
daemons listen on different sockets. A daemon that finds the lock held records
nothing and logs one line. A daemon started with `DEADBAND_SOCKET` set records
nothing, so test daemons do not write to the file. A daemon started with
`DEADBAND_SOCKET_DIR` set records history like the default daemon, because
only `DEADBAND_SOCKET` turns recording off. The client adds the two things that only it
knows, its working directory on `Ask` and the usage of the job's children on
`Done`. The working directory is shortened to 1 KiB by the client.

The job's connection thread walks up from the working directory to `.git` and
reads the config's `warm_dirs` before it takes the queue lock. Later, while it
holds the lock, the daemon builds the record and hands it to one writer thread
with `try_send` on a channel of 256 entries, so the daemon never waits for the
disk. A line that finds the channel full is dropped, and a counter that each
machine line reports goes up. If the writer thread has stopped, the first line
that cannot reach it is logged once.

The same walk runs for every job, including on a daemon that records nothing,
because it also finds the job's workspace. It keeps going past `.git` to the
first directory that holds `.deadband-workspace` as a regular file, and stops
at the filesystem root or after 64 directories. The daemon names the
workspace after that directory, unless the client named one, and computes the
job's short command from its command line with the kind rule below. Both
travel with the job to `status` and `top`, and neither reaches the history
file. The full command line stays in the protocol, because `stuck_ignore`
matches it. `status` prints the full command line only for a job from an
older daemon, which sends no short command, and `top` never prints it.

The kind rule names no tool in code. The kind is the program's file name,
such as `deploy.sh` for `./deploy.sh hunterpass`, so a script's arguments
never reach the file. When the config's `kind_tools` lists the program's file
name, the kind also keeps up to two plain lowercase subcommand words, such as
`cargo nextest run`. The list is empty by default. The rule skips a leading
`env` and the `NAME=value` words after it, and it stops collecting
subcommands at the first word that starts with `-`, because the word after an
option may be its value. The record never holds an option value, an
environment variable or an absolute path.

The writer opens the file when the first line arrives, and each line is one
write. A write that fails loses that line, logs one line when writing starts
to fail and one when it recovers, and the next line tries again. Past 64 MiB,
the writer renames the file to `jobs.1.jsonl`, replacing any earlier one, which
is the only deletion. A crash can tear the last line, so the next open adds a
newline, and a reader skips a line it cannot parse.

# Planned

None of the features in this part exists in the source. The design records
the agreed plan, and each item here is a proposal until a change builds it
and moves it into the first part.

## The planned design

A fixed-slot gate cannot learn what a job costs, cannot slow a job that is
already running, and cannot cap how many compiler processes one build starts.
deadband is planned to learn each job's cost, slow running jobs, and cap
compiler processes through a shared jobserver.

The daemon would be the only owner of the jobserver and of signals. The job
history file exists, as the first part says. A store for fast lookups and any
learned cost depend on what an offline replay of that file shows, so neither
is agreed yet.

```text
 +--------------+  ask/grant  +----------------------------------------+
 | client (cli) |------------>| daemon                                 |
 +--------------+ Unix socket |                                        |
        |                     |  probe --> admit <-- learned cost (?)  |
        | starts the job      |              |          ^              |
        v                     |              v          |              |
   job process <-- signals ---|          throttle   history sink ------+--> history store
        ^                     |              |        (trait)          |
        +-- jobserver tokens -|          jobserver                     |
                              +----------------------------------------+
                                                 ^
                                          deadband top (TUI)
```

## Planned modules

| Module | Would own |
| --- | --- |
| ledger | Would give a learned cost per label from past jobs, if the replay of the job history shows that a prediction is needed. Its store, and whether it uses SQLite, are not decided. |
| jobserver | Runs one shared jobserver pipe, so all cargo processes together start at most a set number of compiler processes. admit raises and lowers the ceiling. |
| sink | The `HistorySink` trait that receives a record of each finished job. The first sink writes records to a board with `board put`. |

Three built modules would also grow. The admit rule would additionally
require that live headroom covers the label's learned cost. The client would
start the job with the jobserver's environment. The throttle module would
pause and resume jobs, with pausing off by default.

## Planned rules

- Only the throttle module would send a signal to a job, and clippy's
  `disallowed-methods` would refuse those calls everywhere else.
- deadband would pause and resume jobs and never end one. When deadband exits
  for any reason, it would resume every job it paused.
- When the ledger fails, the job would run at once and deadband would say
  why in one line.
- At idle the daemon would use under 1% of one core and under 50 MB of
  memory, and an admission answer would take under 5 ms.
