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
| admit | Decides whether a waiting job starts now, and holds the queue. The first rule admits a job when nothing runs, when the probe fails, or when memory pressure is normal and fewer than `--max-jobs` jobs run. Later, live headroom must cover the label's learned cost, and the queue gains priority lanes. |
| daemon | Listens on the socket, samples the probe once a second, and grants waiting jobs as admit allows. |
| client | Asks the daemon to admit a job or to list its jobs, and gives the reason to start at once when the daemon cannot be asked. |
| protocol | The messages over the socket, one JSON object per line. |
| socket | Finds the socket's path from the environment, and lists the variables deadband reads. |
| job | The only module that starts a process: the job that `deadband run` was given. |
| jobserver | Runs one shared jobserver pipe, so all cargo processes together start at most a set number of compiler processes. admit raises and lowers the ceiling. |
| throttle | The only module that pauses, resumes or lowers the priority of a job. Pausing is off by default. |
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

## Later: reclaiming idle workspaces

This section is a draft for after the first modules exist.

When the machine runs short of memory or disk, deadband can ask an external
reclaimer to stop the servers and containers of workspaces nobody is using,
least recently used first. The configuration lists reclaimers as external
commands, so deadband links no workspace tool and does not know what a
workspace is:

```toml
[[reclaim]]
name = "example"
command = ["example-workspace-tool", "reclaim", "--json"]
```

1. deadband runs the command with `--need memory=4G --dry-run`. The reclaimer
   answers with what it would stop, what each item frees, and why each counts
   as idle.
2. deadband shows the plan in `deadband top` and its log, then runs the same
   command without `--dry-run`.
3. The reclaimer stops those workspaces gracefully and reports what it freed.
   deadband measures pressure again before asking for more.

For disk, `--need disk=20G` asks the reclaimer to delete the build output of
idle workspaces. Reclaim runs only under pressure, and an age-based stop is
opt-in. The reclaimer chooses what to stop.
