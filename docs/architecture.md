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
| probe | Reads CPU load, memory pressure, swap and disk space from macOS every second, behind a trait so a Linux probe can be added later. |
| admit | Decides whether a waiting job starts now and how many threads it gets, and holds the queue with its fast and normal lanes. The rule holds a job on the job count or the lane's places even when the probe fails, and on critically low disk even when nothing runs. Past those checks it admits a job when nothing runs or the probe fails, and otherwise holds it on memory pressure or the load. |
| admission | One public call, `admit`, that does what `deadband run` does before it starts a job: it chooses the lane, asks the daemon, waits, and returns the job's lease and variables. It writes no log, so a program that embeds deadband passes a callback. |
| daemon | Listens on the socket, samples the probe once a second, and grants waiting jobs as admit allows. |
| serve | One public call, `run_daemon`, that does what `deadband daemon` does before it prints a failure: it binds the socket, reads the reclaim config and serves. |
| client | Asks the daemon to admit a job or to list its jobs, and gives the reason to start at once when the daemon cannot be asked. |
| protocol | The messages over the socket, one JSON object per line. |
| socket | Finds the socket's path from the environment, and lists the variables deadband reads. |
| job | The only module that starts a process: the job that `deadband run` was given, and the reclaimers that the daemon's config file names. |
| reclaim | Reads the daemon's config file, decides when to reclaim memory or disk, runs the reclaimers on a thread of their own, and keeps the last reclaim for `deadband status`. |
| environment | Reads the variables that `deadband run` uses, and gives a granted job its parallelism variables. |
| priority | Chooses a job's lane from the priority file. |
| throttle | The only module that changes the priority of a job. `deadband run` lowers its own priority to nice 5 before it starts a normal-lane job, and the job inherits it. |
| cli | The commands `deadband run`, `status` and `daemon`, built on crumbkit. [running.md](running.md) says how to install and run them. |

## Rules that shape the code

- Only the throttle module changes a job's priority. clippy's
  `disallowed-methods` refuses those calls everywhere else.
- deadband never signals or ends a job.
- When the daemon cannot be reached, does not answer or closes the connection,
  the job runs at once and `deadband run` says why in one line on standard
  error. A daemon that hangs while it keeps the connection open still keeps a
  queued job waiting, as [running.md](running.md) says.
- When the probe cannot read the machine, the daemon starts each job once a
  place is free, and its log says why in one line.
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

# Planned

None of the features in this part exists in the source. The design records
the agreed plan, and each item here is a proposal until a change builds it
and moves it into the first part.

## The planned design

A fixed-slot gate cannot learn what a job costs, cannot slow a job that is
already running, and cannot cap how many compiler processes one build starts.
deadband is planned to learn each job's cost, slow running jobs, and cap
compiler processes through a shared jobserver.

The daemon would be the only writer of the ledger of past jobs and the only
owner of the jobserver and of signals.

```text
 +--------------+  ask/grant  +----------------------------------------+
 | client (cli) |------------>| daemon                                 |
 +--------------+ Unix socket |                                        |
        |                     |  probe --> admit <-- ledger (SQLite)   |
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
| ledger | Stores each finished job's label, metadata, duration and measured CPU and memory in SQLite, and gives a learned cost per label. |
| jobserver | Runs one shared jobserver pipe, so all cargo processes together start at most a set number of compiler processes. admit raises and lowers the ceiling. |
| sink | The `HistorySink` trait that receives a record of each finished job. The first sink writes records to a board with `board put`. |
| tui | `deadband top`: live load, the queue, running jobs and their learned costs, drawn with ratatui. |

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
