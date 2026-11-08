# Architecture

deadband keeps a busy machine usable while many heavy jobs, such as cargo
builds and test runs, compete for it. A fixed-slot gate cannot learn what a
job costs, cannot slow a job that is already running, and cannot cap how many
compiler processes one build starts. deadband learns each job's cost, slows
running jobs, and caps compiler processes through a shared jobserver.

## Overview

deadband is one binary with a small per-user daemon. A job asks the daemon for
admission over a Unix socket before it starts. If the daemon is not running,
the job runs at once.

The daemon exists because three things need exactly one owner: the ledger of
past jobs, the shared jobserver, and the signals sent to running jobs. The
daemon is the only writer of the ledger and the only owner of the jobserver
and of signals.

```text
          deadband run --label cargo-test -- cargo nextest run
                               |
                               v
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

The client stays small. It asks for admission, waits, starts the job with the
jobserver's environment, and reports the job's exit status.

## Modules

| Module | Owns |
| --- | --- |
| probe | Reads CPU load, memory pressure and swap from macOS every second, behind a trait so a Linux probe can be added later. |
| ledger | Stores each finished job's label, metadata, duration and measured CPU and memory in SQLite, and gives a learned cost per label. |
| admit | Decides whether a waiting job starts now and how many threads it gets, and holds the queue with its fast and normal lanes. The rule admits a job when nothing runs or the probe fails, and otherwise holds it on the job count, critically low disk, memory pressure or the load. Later, live headroom must cover the label's learned cost. |
| daemon | Listens on the socket, samples the probe once a second, and grants waiting jobs as admit allows. |
| client | Asks the daemon to admit a job or to list its jobs, and gives the reason to start at once when the daemon cannot be asked. |
| protocol | The messages over the socket, one JSON object per line. |
| socket | Finds the socket's path from the environment, and lists the variables deadband reads. |
| job | The only module that starts a process: the job that `deadband run` was given, and the reclaimers that the daemon's config file names. |
| reclaim | Reads the daemon's config file, decides when to reclaim memory or disk, runs the reclaimers on a thread of their own, and keeps the last reclaim for `deadband status`. |
| environment | Reads the variables that `deadband run` uses, and gives a granted job its parallelism variables. |
| priority | Chooses a job's lane from the priority file. |
| jobserver | Runs one shared jobserver pipe, so all cargo processes together start at most a set number of compiler processes. admit raises and lowers the ceiling. |
| throttle | The only module that pauses, resumes or lowers the priority of a job. It lowers a normal-lane job to nice 5 now. Pausing comes later and is off by default. |
| sink | The `HistorySink` trait that receives a record of each finished job. The first sink writes records to a board with `board put`. |
| cli | The commands `deadband run`, `status`, `top` and `daemon`, built on crumbkit. [running.md](running.md) says how to install and run them. |
| tui | `deadband top`: live load, the queue, running jobs and their learned costs, drawn with ratatui. |

## Rules that shape the code

- Only the throttle module sends a signal to a job or changes its priority.
  clippy's `disallowed-methods` refuses those calls everywhere else.
- deadband pauses and resumes jobs and never ends one. When deadband exits for
  any reason, it resumes every job it paused.
- When the probe, the ledger or the daemon fails, the job runs at once and
  deadband says why in one line. A failure in deadband never blocks work.
- No source file outside tests is longer than 400 lines.
- At idle the daemon uses under 1% of one core and under 50 MB of memory, and
  an admission answer takes under 5 ms.

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
