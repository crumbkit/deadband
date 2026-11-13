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
| socket | Finds the socket's path from the environment, and lists the variables deadband reads. On macOS the default socket is in the directory that `getconf DARWIN_USER_TEMP_DIR` names, so `run`, `status` and the launchd daemon find the same socket whatever `TMPDIR` each caller has. |
| spawn | Starts every thread of the daemon and hands back the operating system's refusal of a thread as an error, so the caller decides what a missing thread means. Its tests can refuse a named thread. |
| job | The only module that starts a process: the job that `deadband run` was given, the reclaimers and the `on_stuck` command that the daemon's config file names, `/bin/ps`, which names the busiest process outside the jobs, and `launchctl`, behind a trait so tests use a fake launchd. |
| reclaim | Reads the daemon's config file, decides when to reclaim memory or disk, runs the reclaimers on a thread of their own, holds the per-user `reclaim.lock` while a reclaim runs, and keeps the running and the last reclaim for `deadband status`. |
| environment | Reads the variables that `deadband run` uses, and gives a granted job its parallelism variables. |
| priority | Chooses a job's lane from the priority file. |
| throttle | The only module that changes the priority of a job. `deadband run` lowers its own priority to nice 5 before it starts a normal-lane job, and the job inherits it. |
| cli | The commands `deadband run`, `status`, `daemon`, `service` and `top`, built on crumbkit. Its service module renders the launch agent's plist as a pure function and moves the agent between not installed, installed but not loaded, and loaded. Install and uninstall hold a lock on `service.lock` in the state directory while they read and change the plist and launchd. The plist's `KeepAlive` restarts only a daemon that exits nonzero or by a signal, and its `ThrottleInterval` keeps launchd from restarting a daemon that fails at start more than once a minute. [running.md](running.md) says how to install and run them. |
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
  starts every thread through the spawn module (`spawn.rs`) and handles the
  refusal, so a refused thread never panics. The client's lease watcher starts through the same module, and the signal
  forwarder starts through `std::thread::Builder`; both handle a refusal
  without failing the job. A lease whose watcher cannot start tells the job's
  notices that a restarted daemon will not count the job. In a
  release build a panic in any thread aborts the process. Launchd then starts
  a new daemon, and clients start their jobs at once while none listens.
- A failure to record a job never delays, holds or ends that job. The daemon
  does no file I/O while it holds the queue lock.
- The daemon writes no reply to a client while it holds the queue lock. A
  thread decides each grant and queued reply under the lock, releases it, and
  then writes the replies one after another (`daemon/reply.rs`). Each write
  waits at most the connection's one-second write timeout. A client that does
  not read therefore never holds the lock, and delays the other replies that
  the same thread writes by at most one second. A grant that is not written
  in that second frees the job's place, and the daemon then shuts that
  connection down. A client that had read no queued reply, or that reads a
  partial line, then starts its job at once. A client that had read a queued
  reply asks again every 100 ms for up to 5 seconds, so a daemon that is
  still serving queues it again, and it starts its job once no daemon has
  kept it for those 5 seconds.
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
starts a reclaim at once. At most one reclaim runs at a time in a daemon,
and the reclaim thread holds `reclaim.lock` in the state directory while it
runs, so at most one runs at a time for the user whichever daemons are up. A
daemon that finds the lock held logs one line, starts no reclaim, and waits
`reclaim_after` before it tries again. A daemon started with
`DEADBAND_SOCKET` set reads no `config.toml`, so it runs no reclaimer and no
`on_stuck` command. When the daemon reads its config, it logs one line for
each reclaimer, and for `on_stuck`, whose program does not exist, cannot be
read, is not an executable file, or is not an absolute path, and keeps the
reclaimer, so a program installed later works at the next reclaim. A reclaim
whose program cannot start records that failure with the program's name for
`deadband status`, and waits at least `reclaim_after` before it tries again. Memory and
disk each wait `reclaim_after` after their last reclaim, and each reclaim
in a row that frees nothing doubles that wait, up to 30 minutes.

A reclaim runs on a thread of its own, so admission never waits for it.
For each reclaimer in turn, the daemon runs the command with
`--need memory=4G --dry-run`, logs the plan in one line, and then runs it
without `--dry-run`. It stops once the bytes freed cover the need. A
reclaimer that fails or runs past its timeout is logged in one line and
holds no job. deadband never signals or stops a reclaimer. Past its
timeout the daemon logs that and starts no other reclaim until the
reclaimer exits. A reclaimer's run ends when the reclaimer process exits:
the daemon then waits at most 250 ms for the rest of its standard output, so
a background process that the reclaimer left holding standard output does
not hold the reclaim. While a reclaim runs, `deadband status` shows its need
and when it started. [running.md](running.md) gives the config and the
JSON a reclaimer prints.

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
nothing and logs one line. Before each line the writer checks, by device and
inode, that its lock and its open file are still the files at their paths. It
takes a removed or replaced lock again and reopens a removed or replaced
history file. A daemon started with `DEADBAND_SOCKET` set records
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

## Lifecycles and crash points

The tables below give the states of a job, the daemon, the launch agent and
a reclaim, as the source implements them, and what each crash or fault
leaves behind. deadband stores none of these states on disk. Job states
live in the client and in the daemon's memory, the launch agent's state is
read from launchd and the plist each time, and the reclaim state is one
field of the daemon. Each test named below lives under `crates/deadband/`.

### The lifecycle of a job

A job is one `deadband run`, or one call of `admission::admit` by a program
that embeds deadband.

| State | Meaning | Enters when | Leaves when |
| --- | --- | --- | --- |
| Asked | The client has sent `Ask` and waits up to 2 seconds for the first answer | The client connects to the socket | `Granted` goes to Running; `Queued` goes to Waiting; no daemon on the socket, a connection that fails, no answer within 2 seconds, a closed connection or an unreadable answer goes to Open |
| Waiting | The daemon holds the job in its queue and the client waits with no deadline | The daemon replies `Queued` | `Granted` goes to Running; a closed connection goes to Asking again |
| Asking again | The client asks again every 100 ms, so a restarted daemon queues or grants it | The connection of a queued job closes | A daemon grants the job, which goes to Running, or queues it, which goes to Waiting; 5 seconds after the first loss with no grant, the job goes to Open |
| Running | The job runs and the daemon counts its place; the lease's watcher thread reads the connection | The daemon grants the job | The job ends and the client sends `Done`, which goes to Ended; the connection closes while the client lives, which goes to Reseating |
| Reseating | The lease has lost its daemon and tries to connect and send `Seat` every 100 ms | The daemon stops or closes the connection while the job runs | A daemon accepts the seat and counts the job at once, which goes to Running; the job ends first, and no daemon is told |
| Open | The job starts at once without a lease, and `deadband run` says why in one line | The daemon could not be asked or did not keep the job, or `DEADBAND_HELD` or `DEADBAND_BYPASS` is set, in which case nothing is asked and nothing is said | The job starts, which goes to Uncounted |
| Uncounted | The job runs and no daemon counts it, for the rest of its run, because it holds no lease to seat | Open | The job ends |
| Ended | The daemon records the exit code or signal and frees the place | The client sends `Done` | — |
| Lost | The daemon records the end as lost and frees the place | The connection closes, or sends an unreadable line, without `Done` | — |
| Left | The daemon records once that the job left before it ran, and frees any place it was granted | The client of a waiting job closes its connection, or the daemon cannot write the job's grant within 1 second | — |

The daemon writes no reply while it holds the queue lock. A thread decides
each `Queued` and `Granted` reply under the lock, releases the lock, and then
writes its replies one after another (`daemon/reply.rs`), each within the
connection's 1-second write timeout. When a grant is not written in that
second, the daemon records the job as Left, grants again under the lock, and
shuts that connection down after it has released the lock. The client then
goes to Open if it had read no `Queued` line, or only part of one, and to
Asking again if it had read `Queued`.

The tests are `a_queued_client_asks_a_restarted_daemon_again_before_it_starts_the_job`,
`a_queued_client_starts_the_job_when_no_daemon_returns_within_the_bound` and
`a_queued_client_stops_asking_at_the_bound_when_every_daemon_queues_it_and_closes`
in `src/client/tests.rs`; `admit_without_a_daemon_starts_the_job_with_capped_threads_and_says_why`
and `admit_with_bypass_asks_nothing` in `src/admission/tests.rs`;
`a_job_that_runs_when_its_daemon_is_killed_counts_on_the_next_daemon` and
`a_run_inside_the_job_that_holds_the_only_place_runs_at_once` in
`tests/run.rs`; and `a_running_job_whose_client_is_killed_is_lost` and
`a_waiting_job_whose_client_is_killed_has_left` in `src/daemon/tests/timeline.rs`;
and `a_grant_that_cannot_be_written_frees_its_place_once_and_closes_the_connection`
and `a_client_that_reads_gets_its_queued_reply_and_then_its_grant` in
`src/daemon/tests/reply.rs`.

### The lifecycle of the daemon

| State | Meaning | Enters when | Leaves when |
| --- | --- | --- | --- |
| Binding | `deadband daemon` takes the kernel lock on `<socket>.lock`, removes an old socket at the path, and binds a new one | The process starts | A bound socket goes to Serving. A held lock refuses with `deadband::daemon::running` and exits with status 0. Something other than a socket at the path, or a socket or lock that cannot be made, refuses with `deadband::socket::unusable` |
| Serving | The sampler reads the machine every second and grants jobs, a watchdog thread watches the reading's age, and the accept loop starts one thread per connection | The socket is bound and the sampler and watchdog threads started | A reason to stop goes to Stopped |
| Serving on a stale reading | The sampler has stored no reading for five sampling intervals, and at least 250 ms. Admission treats the probe as failed, so each job starts once a place is free | The watchdog finds the reading stale | The sampler stores a new reading |
| Stopped | `serve` returns its reason and the process ends | The socket file or the lock file is gone or replaced, checked every 60 samples and by the watchdog while the reading is stale; the sampler panics; the operating system refuses the sampler, the watchdog or a connection's thread; or accepting fails in a way that another try cannot fix | — The process exits nonzero, so under launchd `KeepAlive` starts a new daemon |

### The lifecycle of the launch agent

`deadband status` names these states when no daemon answers, and
`--json` gives them as `daemon_state`.

| State | Meaning | Enters when | Leaves when |
| --- | --- | --- | --- |
| Not installed | No plist exists | Uninstall deletes the plist, or none was written | Install writes the plist |
| Not loaded | The plist exists and launchd has not loaded it | Uninstall's `bootout` succeeded and its delete failed, or install's `bootstrap` failed | Install bootstraps the plist |
| Loaded, answering | launchd has loaded the agent and its daemon answers on the socket | `bootstrap` succeeded and the daemon bound the socket | Uninstall, or install with a changed plist or `--restart`, boots the agent out |
| Loaded, silent | launchd has loaded the agent and no daemon answers, for example because a daemon that launchd did not start holds the lock; status names `~/Library/Logs/deadband.log` | The daemon exits at start | The other daemon ends, and launchd's next start binds the socket |
| launchd not asked | `launchctl print` failed, so deadband cannot say whether the agent is loaded | launchctl fails | launchctl answers again |

Install and uninstall hold `service.lock` in the state directory from before
they read the plist or launchd until they finish. A second command looks
again every 100 ms, about ten seconds in all, and then refuses with
`deadband::service::locked`. When the plist changed, install keeps a loaded daemon that has running
or waiting jobs, or that does not answer, unless `--restart` is given. With
`--restart`, and on uninstall, the report gives the running and waiting jobs
the daemon held, or says that it did not answer. After `bootstrap`, install
asks the socket up to 20 times and reports whether the new daemon answers.
It says so when a daemon that launchd did not start already holds the
socket. The plist's `KeepAlive` sets `SuccessfulExit` to false, so launchd
restarts a daemon that exits nonzero or by a signal and leaves one that
exits with status 0 stopped. A daemon refused because another daemon holds
the socket exits with 0, so launchd starts it once and then waits for
`deadband service install --restart` or the next login. The plist sets
`ThrottleInterval` to 60 seconds, so launchd starts a daemon that fails at
start at most once a minute.

The tests are in `src/cli/service/tests.rs`:
`state_names_a_missing_plist_and_a_plist_that_launchd_has_not_loaded`,
`an_install_whose_daemon_never_answers_says_so_and_names_the_log`,
`an_install_says_when_a_daemon_that_launchd_did_not_start_holds_the_socket`,
`a_loaded_agent_whose_daemon_does_not_answer_holds_the_reload_and_keeps_the_old_plist`,
`uninstall_of_a_loaded_agent_prints_the_jobs_the_stopped_daemon_held` and
`the_plist_makes_launchd_wait_between_restarts_of_a_daemon_that_exits_at_start`.
`status_names_the_log_when_launchd_has_loaded_the_agent_and_no_daemon_answers`
is in `src/cli/tests/service.rs`.

### The lifecycle of a reclaim

| State | Meaning | Enters when | Leaves when |
| --- | --- | --- | --- |
| Idle | No reclaim runs | The daemon starts, or a rest ends | Disk is critically low, or memory has been short for `reclaim_after`, and the resource has rested; this goes to Locking. A daemon with no reclaimer stays Idle |
| Locking | The reclaim thread tries `reclaim.lock` without waiting | The rule starts a reclaim | The lock is taken, which goes to Dry run; the lock is held by another daemon, or the thread is refused, which goes to Resting for exactly `reclaim_after` |
| Dry run | A reclaimer runs with `--dry-run`, and the log shows its plan | The lock is taken, or the previous reclaimer did not cover the need | A plan to free bytes goes to Real run; a plan of nothing or a failure moves to the next reclaimer, or to Resting after the last |
| Real run | A reclaimer runs without `--dry-run` | The dry run planned to free bytes | The bytes freed cover the need, or the last reclaimer has run, which goes to Resting; otherwise the next reclaimer's dry run |
| Resting | The resource waits `reclaim_after`, doubled for each reclaim in a row that freed nothing, up to 30 minutes | The reclaim ended | The rest passes, which goes to Idle |

Each run of a reclaimer ends when the reclaimer process exits. A run past its
timeout is logged and waited for, and no other reclaim starts meanwhile.
`deadband status` shows the reclaim from Locking until Resting as
`reclaiming: <need>, started <age> ago`.

The tests are `no_reclaim_starts_while_one_runs_or_within_reclaim_after_of_its_end`,
`a_reclaim_skipped_for_a_held_lock_rests_reclaim_after_and_is_not_counted_as_fruitless`,
`a_reclaim_that_frees_nothing_doubles_the_wait_for_that_resource_up_to_30_minutes`,
`a_dry_run_that_plans_nothing_skips_the_real_run`,
`the_reclaim_stops_once_the_bytes_freed_cover_the_need` and
`a_running_reclaim_names_its_need_and_when_it_started_until_it_ends` in
`src/reclaim/tests.rs`, and
`under_warn_pressure_the_daemon_runs_the_reclaimer_dry_then_for_real_and_status_shows_it`
in `src/cli/tests/reclaim.rs`.

### Crashes and faults

| Crash or fault | Result | Test |
| --- | --- | --- |
| The sampler panics | The sampler's ending sends its reason, the daemon stops serving and the process ends. Waiting clients ask again for 5 seconds and then start, and running jobs reseat on the next daemon | `a_daemon_stops_serving_when_its_sampler_panics` in `src/daemon/tests/alive.rs` |
| The sampler blocks | After five sampling intervals the watchdog counts the reading as stale, so the jobs it held start once a place is free, and it keeps checking the socket and lock files | `a_job_starts_when_the_sampler_stalls_and_its_last_reading_holds_the_job` and `a_daemon_whose_sampler_is_stalled_stops_serving_when_its_socket_file_is_removed` in `src/daemon/tests/alive.rs` |
| The daemon hangs after it sent `Queued`, with the connection open | The client keeps waiting with no deadline, because fail-open applies only to a closed connection or a missing first answer. A stalled sampler is the case above; a daemon whose every thread hangs keeps its waiting jobs waiting until it is stopped | No test injects a whole-daemon hang |
| A client never reads its reply | The daemon writes the reply after it releases the queue lock, so the client never holds the lock. The write fails after 1 second. A grant that fails frees the job's place and is recorded once as Left, and the daemon shuts that connection down | `a_client_that_never_reads_its_grant_does_not_delay_the_next_grant`, `a_grant_that_cannot_be_written_frees_its_place_once_and_closes_the_connection` and `a_queued_reply_that_waits_on_its_client_leaves_the_queue_lock_free` in `src/daemon/tests/reply.rs` |
| The daemon stops or is killed | Waiting clients ask again for 5 seconds, so a restarted daemon queues them again, and start only if no daemon answers. Running jobs keep running, and each lease seats its job on the next daemon | `a_queued_client_asks_a_restarted_daemon_again_before_it_starts_the_job` in `src/client/tests.rs`; `a_job_that_runs_when_its_daemon_is_killed_counts_on_the_next_daemon` in `tests/run.rs` |
| A reclaimer hangs | The daemon logs that it ran past its timeout, keeps waiting for it, and starts no other reclaim until it exits; admission continues, and `deadband status` shows the reclaim in progress | `a_reclaimer_that_runs_past_its_timeout_is_logged_and_admission_continues` in `src/cli/tests/reclaim.rs`; `status_prints_a_reclaim_in_progress_after_the_jobs` in `src/cli/tests.rs` |
| A reclaimer exits and leaves a background process holding its standard output | The run ends when the reclaimer exits, after at most 250 ms for its output | `a_background_child_that_holds_stdout_does_not_hold_the_reclaim_after_the_reclaimer_exits` in `src/job/reclaimer.rs` |
| A reclaimer names a program that is missing | The daemon logs the program at start, the reclaim records the failure, and `deadband status` names the program | `the_daemon_says_at_start_that_a_reclaimer_names_a_program_that_does_not_exist` and `a_reclaimer_whose_program_is_missing_fails_and_status_names_the_program` in `src/cli/tests/reclaim.rs` |
| The socket file is deleted or replaced | The daemon stops serving within 60 samples, and under launchd a new daemon binds a new socket | `a_daemon_stops_serving_when_its_socket_file_is_removed` and `a_daemon_stops_serving_when_another_socket_replaces_its_file` in `src/daemon/tests/alive.rs` |
| The lock file is deleted or replaced | The daemon stops serving within 60 samples, because a second daemon could take a new lock, remove its socket and bind its own | `a_daemon_stops_serving_when_its_lock_file_is_removed` and `a_daemon_stops_serving_when_another_file_replaces_its_lock_file` in `src/daemon/tests/alive.rs` |
| A second daemon starts on the same socket | It finds the lock held, refuses with `deadband::daemon::running` and exits with status 0, so launchd does not start it again | `bind_refuses_while_another_daemon_holds_the_lock_even_without_its_socket` in `src/daemon/tests.rs`; `daemon_exits_0_with_daemon_running_while_another_daemon_answers_on_its_socket` in `src/cli/tests.rs` |
| `jobs.jsonl` is deleted or replaced | The writer opens the file at the path again before the next line, so later lines are recorded there | `a_writer_whose_file_is_unlinked_opens_a_new_file_at_the_path_and_records_later_lines` and `a_writer_whose_file_is_replaced_appends_to_the_file_now_at_the_path` in `src/history/tests.rs` |
| `jobs.lock` is deleted or replaced | The writer releases its lock and takes it again before the next line. When another daemon took the new lock first, the writer logs one line and records nothing while that daemon holds the lock | `a_writer_whose_lock_is_removed_while_another_writer_takes_a_new_one_records_nothing_and_logs_once` and `a_writer_whose_lock_is_removed_with_no_other_writer_takes_a_new_lock_and_keeps_recording` in `src/history/tests.rs` |
| Two daemons on different sockets want to reclaim | A daemon on its own `DEADBAND_SOCKET` runs no reclaimer. Two daemons that both read the config share `reclaim.lock`, so the second starts no reclaim while the first runs | `a_daemon_started_with_its_own_socket_runs_no_reclaimer_from_the_config_file` in `tests/run.rs`; `a_second_reclaim_finds_the_lock_held_until_the_first_ends` in `src/reclaim/lock.rs` |
| `deadband run` receives SIGTERM or SIGINT alone | It passes the signal on to its job unchanged and keeps waiting, so the job decides how to end and the daemon receives `Done` with the signal | `a_signal_the_caller_sent_ends_a_sleeping_job_and_the_exit_code_follows_it` and `the_job_receives_the_signal_unchanged_and_decides_its_own_exit_code` in `src/job/tests.rs` |
| `deadband run` is killed alone with SIGKILL | The connection closes, the daemon records the job as lost and frees its place, and the job keeps running uncounted. A signal to the process group reaches the job too | `a_running_job_whose_client_is_killed_is_lost` in `src/daemon/tests/timeline.rs` covers the daemon's side; no test covers the job that keeps running |
| The operating system refuses a daemon thread | A refused sampler, watchdog or connection thread stops the daemon; a refused connection thread also closes that connection, so its client starts its job. A refused reclaim thread starts no reclaim now. A refused watch, history or `on_stuck` thread is one log line | `a_daemon_that_cannot_start_its_sampler_stops_serving`, `a_daemon_that_cannot_start_its_watchdog_stops_serving` and `a_daemon_that_cannot_start_a_connections_thread_stops_serving_and_closes_it` in `src/daemon/tests/alive.rs`; `an_on_stuck_whose_thread_cannot_start_is_one_log_line_and_frees_its_place` in `src/daemon/watch/hook.rs`; the reclaim thread is not tested, as the soft limits below say |
| `service install` races `service uninstall` | The second command waits for `service.lock`, so the plist exists only while launchd has the agent loaded | `an_install_racing_an_uninstall_leaves_the_plist_only_while_launchd_has_it_loaded` and `two_concurrent_installs_load_the_agent_once_and_leave_the_rendered_plist` in `src/cli/service/tests.rs` |

### Known limits left as they are

These limits are kept on purpose. The first three were found in the
lifecycle review of 2026-11-12, and the rest are soft limits of the fixes
that followed it.

- The daemon's log, `~/Library/Logs/deadband.log`, is never rotated. The
  plist sends the daemon's standard error to it, and nothing truncates it.
- A daemon that is killed loses the history lines still waiting for the
  writer, up to the 256 that the channel holds. A rotation past 64 MiB
  replaces the earlier `jobs.1.jsonl`.
- The daemon keys each watched job's stuck track by the process ID that the
  client sends. A second client that sends the same process ID shares the
  track.
- The daemon names its lock file by a stat of the lock path when it starts
  serving, after `bind` took the lock. A lock file replaced between `bind`
  and that stat is recorded as the daemon's own, and the daemon never
  notices the replacement.
- No test refuses the reclaim thread, because the sampler thread starts it
  and a test's refusal reaches only the threads that the test's own thread
  starts. The code logs one line and starts no reclaim now.
- The service commands find `service.lock` in `$HOME/.local/state/deadband`
  whatever `XDG_STATE_HOME` says, as the plist's path comes from `HOME`
  too. That is the state directory of the daemon that launchd starts, since
  the plist sets no `XDG_STATE_HOME`.
- A background process that holds a reclaimer's standard output keeps one
  reading thread and one pipe in the daemon until it closes the pipe. The
  reclaim does not wait for it.
- A client that does not read its reply delays the later replies that the
  same thread writes by up to 1 second each. It never holds the queue lock.
- A job's `Granted` reply can reach its client before its `Queued` reply,
  when another thread grants the job between the moment the asking thread
  releases the lock and the moment it writes `Queued`. The client then holds
  its lease, and the lease's watcher ignores the late `Queued` line.

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
