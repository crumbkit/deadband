# Running deadband

deadband has four commands that do its work. `deadband daemon` admits jobs,
`deadband run` asks the daemon before it starts a job, `deadband status`
shows the machine and the daemon's jobs, and `deadband top` draws the last
ten minutes in the terminal. On macOS, `deadband service install` makes
launchd run the daemon at login.

Three more commands describe deadband to a program or an agent.
`deadband commands list` lists every command with its arguments and
examples. `deadband schemas list` lists the JSON documents that
deadband prints and the format of its config file, and
`deadband schemas show <ID>` prints the JSON Schema of each one.

## Installing

Each [GitHub release](https://github.com/crumbkit/deadband/releases) has
an archive of the `deadband` binary for macOS on Apple silicon and for Linux
on x86_64 and aarch64, and a SHA256SUMS file that lists each archive's
checksum. Unpack the archive and put `deadband` on your `PATH`.

Or build and install the binary from a checkout:

```sh
cargo install --locked --path crates/deadband
```

Cargo puts `deadband` in `~/.cargo/bin`.

To publish a release, land a pull request that sets the new version in the
workspace `Cargo.toml` and moves CHANGELOG.md's Unreleased entries under that
version. Then tag the merge commit as `v<version>` and push the tag. The
Release workflow, `.github/workflows/release.yml`, refuses a tag that differs
from the version in `Cargo.toml`, runs the advisory check and the tests,
builds the archives and attaches them to the release.

## Running jobs through the daemon

Start the daemon in a terminal:

```sh
deadband daemon --max-jobs 2
```

Then start each heavy job through `deadband run`, with the job's command
after `--`:

```sh
deadband run --label tests -- cargo nextest run
```

`deadband run` waits until the daemon admits the job, runs the job with its
own standard input, output and error, and exits with the job's exit status.
A job that a signal ended exits with 128 plus the signal's number, as a
shell reports it. When the program is not found, `deadband run` exits with
127, and when the program is found but cannot start, it exits with 126, as
`env` and `timeout` do.

When `deadband run` receives a SIGTERM or SIGINT, it passes the same signal
on to the job unchanged, keeps waiting, and exits with the job's status, so
a `kill` of `deadband run` ends the job (exit 143 or 130) and the daemon
records the job as ended. A Ctrl-C at the terminal reaches the job directly
and through `deadband run`, so the job receives the SIGINT twice; a program
that stops at once on a second SIGINT takes that path on one Ctrl-C.
`deadband run` replaces an ignored SIGINT or SIGTERM that its caller passed
down, such as the ignored SIGINT of a background job in a non-interactive
shell script, so the job no longer inherits the ignore. If the signals
cannot be registered, the job runs without forwarding. If only the forwarding
thread cannot start, the job runs without forwarding and `deadband run` ignores
SIGTERM and SIGINT until the job ends.

The daemon samples memory pressure, the 1-minute load average and the free
space of the disk that holds `$HOME` once a second. It admits a waiting job
when all of these hold:

- Fewer than `--max-jobs` jobs run. The default for `--max-jobs` is a
  quarter of the logical cores, rounded up, and at least 1, so a machine
  with 14 cores runs 4 jobs at once.
- When the priority file holds at least one entry and `--max-jobs` is more
  than 1, fewer than `--max-jobs` minus 1 jobs run in the job's lane, so a
  lane runs at most `--max-jobs` minus 1 jobs. With `--max-jobs 4`, each lane
  runs at most 3. Each lane leaves the last place open for the other, so a
  fast job starts at once on a machine full of normal jobs, and the reverse.
- The disk has at least 20 GiB free, and its free space is at least 3% of
  the disk.
- Memory pressure is normal.
- The 1-minute load average, plus the threads of each running job that
  started in the last 60 seconds, is below the number of logical cores. The
  load average takes about a minute to show a new job, so the daemon adds
  those threads itself.

A job that arrives while nothing runs starts unless the disk is critically
low, so one job never waits forever on load or memory. A job that waits on
the disk starts once free space is back above both limits. A job counts as running until its `deadband run` ends. A waiting
job prints one line on standard error that says why it waits and how many
jobs start before it. An unreadable disk holds no job, and the daemon's log
says why.

## Watching the last ten minutes

`deadband top` opens a read-only view in the terminal. Each job is one row
that shows when it waited, when it ran and how it ended, and the view holds a
line for the load and a line for what held jobs, on one time axis. Press `q`
or Ctrl-C to quit. The terminal must be at least 80 columns by 16 lines.

The rows come from the daemon, which remembers the last 64 jobs that ended,
in memory only, so a restart forgets them. The load and the holds come from
the view's own polls, once a second, so they start empty when the view opens.
A job marked `lost` ended without saying how, for example when its
`deadband run` was killed. Each row shows the job's workspace, its label, its
short command, its lane and its times; the view shows no full command line.
The workspace and job columns are 16 and 26 columns wide on a terminal of 100
columns or more, and 12 and 16 below that. A program that embeds the
deadband library sets `default-features = false` to leave the view out.

A stuck job moves to the top of the rows, and a red `▓` bar covers each
period in which it showed no activity, also after the job recovers or ends,
while that period is in the window. The state column reads `stuck 46m`, a red
`2 STUCK` label sits left of the hold label, and one line above the rule gives
the count and the busiest process outside the jobs.

A SIGTERM or SIGKILL sent to `deadband top` leaves the terminal in raw mode.
Run `reset` to restore it.

## Workspaces and short commands

`deadband status` and `deadband top` name each job by its workspace and its
label, and show its short command in place of its command line.

The workspace is the name of the nearest directory, at or above the job's
working directory, that holds a regular file named `.deadband-workspace`. A
tool that makes workspaces, such as mxv, writes that file; its content is
never read. The daemon walks up at most 64 directories, and a marker that is
a directory or a symbolic link does not count. A job outside any workspace
has none, and its row shows only its label. A program that embeds deadband
can name the workspace in `Asked::workspace`, and the daemon then keeps that
name. A workspace name keeps at most 64 bytes, and each control character in
it becomes a space.

The short command is the kind that the job history records: the program's
file name, plus up to two plain lowercase subcommand words when the config's
`kind_tools` lists the program. With `kind_tools = ["cargo"]`,
`cargo nextest run -p api` shows as `cargo nextest run`; with the default
empty list it shows as `cargo`. The rule stops at the first word that starts
with `-`, so `tool --token s3cr3t deploy` shows as `tool`. It skips words with
capitals, digits, `/`, `=` or `:`, and words longer than 32 bytes. A secret
made only of lowercase letters, `-` and `_` can still show when it is one of
the first two words after a listed program, such as `mysql hunter` with
`mysql` listed. A command whose program name is not plain, such as
`./Deploy.sh`, has an empty short command, and its row shows only its name.

The name joins the workspace and the label with a middle dot, as in
`triage · api-tests`. The label is left out when it equals the workspace, as
for `mxv up`, or when it equals the first word of the short command, which is
the label `deadband run` gives when `--label` is absent. A job with no
workspace is named by its label. `deadband status` prints one line per job:

```text
running: triage · api-tests (pid 40122): cargo nextest run
waiting: board-sync (pid 40127): cargo test
```

A line whose short command is empty ends after the pid. With a daemon older
than this release, which sends no workspace or short command, `status` prints
the label and the full command line as before, and `top` shows the label
alone.

`deadband status --json` prints the `deadband/status/v2` document. Each job
carries `label`, `workspace`, `short`, `pid` and `lane`, and no `command`;
`workspace` is absent outside a workspace, and `short` is absent from an older
daemon.

## Stuck jobs

The daemon watches the process tree of each job that `deadband run` started.
A job counts as stuck after `stuck_after`, 5 minutes by default, in which its
whole tree used under 1 second of CPU in total, read and wrote under 1 MiB of
disk, and no process in the tree started or ended. deadband never ends,
pauses or signals a stuck job, and the job keeps its place. Its exit code and
its output do not change.

`deadband status` lists each stuck job first, names the busiest process
outside the jobs when it uses at least 50% of one core, and exits 1 with the
failure code `deadband::job::stuck`. It also exits 1, with
`deadband::job::unreadable`, when the daemon could not read a job's
processes on macOS. A platform with no process reader exits 0 and says
nothing about stuck jobs.

The waiting `deadband run` prints one line on standard error when its job
becomes stuck, every 10 minutes while it stays stuck, and when it is active
again.

A job that waits on purpose, such as one that waits for a container, shows
no activity and is flagged. Add its command to `stuck_ignore`. These three
optional keys sit at the top of the daemon's config file, which the next
section describes. An invalid value in any of them has the same effect as an
unreadable file: the daemon logs one line, runs no reclaimer and no
`on_stuck`, and detects stuck jobs with the defaults.

```toml
stuck_after = "5m"            # at least 1m; "off" turns detection off
stuck_ignore = ["^docker "]   # a job whose command line matches any of these regular expressions is not watched
on_stuck = ["/usr/local/bin/tell-me"] # run once when a job becomes stuck
```

`on_stuck` runs once each time a job becomes stuck, on a thread of its own.
It reads one JSON object on standard input, with the job's `label`, its
`quiet_seconds`, and `busiest`, which is `null` when `/bin/ps` could not be
read, `{"state":"quiet"}` when no process outside the jobs reaches 50% of one
core, and otherwise `{"state":"busy","name":"syspolicyd","cpu_percent":470}`.
deadband discards its output. A failure is one log line, and a command still
running after 60 seconds is logged once and left running, because deadband
never signals it. At most 8 run at once; a job that becomes stuck while 8 run
gets none, and the log says so.

## The job history

Each daemon keeps a record of the jobs that end on its machine, so an offline
script can compare admission policies. It appends one line of JSON for each
job that leaves the daemon, and one line about the machine every 60 seconds.
Nothing in deadband reads the file, and nothing leaves the machine.

The file is `deadband/jobs.jsonl` in `$XDG_STATE_HOME`, or
`~/.local/state/deadband/jobs.jsonl` when `XDG_STATE_HOME` is not set. Its
directory has mode 0700 and the file has mode 0600. A daemon started with
`DEADBAND_SOCKET` set records nothing. A daemon started with
`DEADBAND_SOCKET_DIR` set records history like the default daemon, so a
private daemon that uses that variable writes this file too. The daemon that writes the file holds a
lock on `jobs.lock` in the same directory, and a second daemon that finds the
lock held records nothing, logs one line and still admits its jobs.

When the file passes 64 MiB, the daemon renames it to `jobs.1.jsonl`, replacing
any earlier one, and starts a new file. The two files together hold about
128 MiB, and this rotation is the only thing that deletes history.

A failure to record never holds a job. When the file cannot be written, the
daemon logs one line and tries again with the next record, and it logs one line
when writing works again. When more than 256 lines wait for the disk, a line is
dropped and the `dropped` field of the machine lines rises. A crash can leave
a torn last line. The next open adds a newline, so a reader skips the one line
it cannot parse.

Each line has `"type":"job"` or `"type":"machine"`, and `"v":1`. A reader must
refuse a line with a higher `v`. `jq` reads the file.

A job line has these fields:

| Field | Meaning |
| --- | --- |
| `queued_at_ms`, `started_at_ms`, `ended_at_ms` | Wall-clock times in Unix milliseconds. `started_at_ms` is null for a job that left before it ran. |
| `seated` | True when a new daemon took over the running job, so its wait is unknown. Such a job counts as starting when it asked, and its `load_at_start` and `warm` are null. |
| `label` | The label, at most 64 bytes. |
| `kind` | The program's file name, such as `deploy.sh` for `./deploy.sh hunterpass`. When the config's `kind_tools` lists that name, the kind also keeps up to two plain lowercase subcommand words, such as `cargo nextest run`, and stops at the first word that starts with `-`. A leading `env` and the `NAME=value` words after it are skipped. The program's file name may hold digits and dots, such as `wait-for-stack.sh`. |
| `options` | The names of up to 32 options, without any value: each `--name` form and each exact `-x` form. |
| `repo`, `checkout` | The final directory name of the main checkout and of the worktree, or null outside git. |
| `warm` | True when a directory that `warm_dirs` names has an entry under the checkout when the job asks. Null when `warm_dirs` is empty or the job is outside git. |
| `lane`, `threads`, `ahead` | The lane, the threads granted, and the jobs ahead when it asked. |
| `exit` | `{"exited":{"code":N}}`, `{"signalled":{"signal":N}}`, `"lost"` or `"left"`. `"left"` is a job that left before it ran. |
| `child_cpu_ms`, `child_peak_rss_bytes` | What the kernel reports of the job's waited-for children, or null when the client sent none. |
| `load_at_start`, `load_mean`, `load_max` | The 1-minute load average when the job started and across the run. |
| `pressure_s`, `samples`, `running_max` | The seconds of the run with memory pressure above normal, the readings behind the load figures, and the most jobs that ran at once. |

`child_cpu_ms` and `child_peak_rss_bytes` are lower bounds on a cold build. A
compiler that a cache's server runs is not a child of the job, so its work is
almost invisible. Peak memory is the largest single process, and it is not a
sum across parallel compilers.

A machine line has `at_ms`, `load_1m`, `pressure`, `running`, `waiting`, `held`
(why the first waiting job waits), `max_jobs`, `cores` and `dropped`, the lines
that the daemon has dropped since it started.

The line never holds the command's arguments or option values, environment
variables, job output or absolute paths. A job that never reaches a daemon,
because the daemon was down, the caller set `DEADBAND_BYPASS`, or the job runs
inside a granted job, has no line.

## Threads

The daemon gives each job a number of threads: the logical cores divided
by `--max-jobs`, and at least 1. A fast-lane job gets one more. With 14
cores and `--max-jobs 4`, a normal job gets 3 threads and a fast job 4.
`deadband run` sets these variables to the job's threads, and leaves each
one that its caller already set:

| Variable | Value |
| --- | --- |
| `CARGO_BUILD_JOBS` | the threads |
| `NEXTEST_TEST_THREADS` | the threads |
| `RUST_TEST_THREADS` | the threads |
| `CMAKE_BUILD_PARALLEL_LEVEL` | the threads |
| `MAKEFLAGS` | `-j` and the threads |

A job that starts at once because the daemon cannot be asked still gets
these variables, with the threads of the default `--max-jobs`: the cores
divided by a quarter of the cores rounded up, and one more in the fast
lane. On a machine with 4 or fewer cores that default allows one job at
once, so the job gets every core, and one more thread in the fast lane.
Otherwise it gets a share of the cores. When the number of cores is unknown,
a normal-lane job gets 1 thread and a fast-lane job gets 2.

## Lanes

Each job waits in the fast lane or the normal lane. Waiting fast jobs start
before every waiting normal job, and each lane starts its jobs in the order
they arrived. `deadband run` lowers a normal-lane job's priority to nice 5,
or leaves it where it is when its caller already set a higher nice value.
A fast-lane job runs at its caller's priority.

`deadband run --lane fast` or `--lane normal` chooses the lane. Without
`--lane`, a job runs in the fast lane when the priority file matches it,
and in the normal lane otherwise. The priority file is `deadband/priority`
in `$XDG_CONFIG_HOME`, or `~/.config/deadband/priority` when
`XDG_CONFIG_HOME` is not set. It holds one entry per line, and a `#`
starts a comment:

```text
# Jobs at or under this directory run in the fast lane.
~/work/urgent-fix
# Jobs labelled release, or run in a directory named release, run in the fast lane.
release
```

An entry that starts with `/` or `~` matches a job whose working directory
is that directory or under it. Any other word matches a job whose `--label`
is that word, or whose working directory has a part named that word. The
word `exclusive` matches nothing. `deadband run` reads the file each time it
starts, so a change applies to the next job. The daemon also reads the file
every second to decide whether the lanes reserve a place, so emptying the
file ends the reservation within a second.

## Jobs inside jobs

`deadband run` sets `DEADBAND_HELD=1` for a job the daemon granted. A
`deadband run` that finds `DEADBAND_HELD` set runs its job at once without
asking the daemon, because the job that started it already holds a place.
Without this, a build that runs its tests through `deadband run` would wait
for a place that it holds itself.

`DEADBAND_HELD` and `DEADBAND_BYPASS` each take effect when the variable is
set to a value that is neither empty nor `0`.

`DEADBAND_BYPASS=1` makes `deadband run` run its job at once without asking
the daemon and without setting its threads or its priority.

## When the daemon cannot be asked

`deadband run` does not wait on a daemon that is missing, silent or gone.
When no daemon listens, when the daemon does not answer within 2 seconds,
or when it closes the connection, the job starts at once and `deadband run`
prints one line on standard error that says why. After the daemon has
queued a job, `deadband run` waits for the grant with no deadline, so a
daemon that hangs while it keeps the connection open also keeps the job
waiting. Stopping that daemon closes the connection. The waiting
`deadband run` then tries to reach a daemon every 100 ms for 5 seconds and
asks again, so a restarted daemon queues the job once more. The job starts at
once, with the same line on standard error, only when no daemon answers
within those 5 seconds. When the probe cannot read the machine, the
daemon admits each job as soon as a place is free, and its log says why.
The daemon treats a reading that is more than five sampling intervals old
(at least 250 ms) as a failed probe, so a stalled sampler does not hold jobs
back for ever; its log says so once.

The daemon stops serving, and the `deadband daemon` command prints why and
exits, in three cases. The first is that the socket path no longer names the
socket file the daemon bound, or the lock path no longer names the lock file
the daemon holds, checked once every 60 samples (about a minute). A removed
lock file would let a second daemon start on the same socket. The second is
that the sampler thread panicked. The third is that the operating system
refused a thread the daemon needs, such as the sampler or a connection's
thread, which happens at the user's limit on threads or processes. Launchd
then starts a new daemon, and `deadband run` starts its jobs at once while
none listens. When the operating system refuses the thread of a reclaim, of
an `on_stuck` command or of a `/bin/ps` run, the daemon logs one line and
keeps serving, and the next one tries again.

`deadband status` prints the load, memory pressure and swap, followed by the
running and waiting jobs and the last reclaim, or `daemon: not running`
when no daemon answers. While a reclaim runs, it also prints
`reclaiming: <need>, started <age> ago`. When jobs wait, it also prints a line that names the
first waiting job and says why it does not start yet.

## Reclaiming memory and disk

A job that waits for memory or disk waits for something to free it. The
daemon can ask a reclaimer, an external command that you name, to free
memory or disk when the machine runs short. A reclaimer might stop the
servers of idle workspaces, or delete build output that can be rebuilt.
deadband never stops anything itself; the reclaimer decides what to stop.

A daemon started with `DEADBAND_SOCKET` set ignores the config file, logs one
line saying so, and runs no reclaimer and no `on_stuck` command. A reclaim
holds a lock on `reclaim.lock` in the state directory while it runs, so
daemons of one user never reclaim together; a daemon that finds the lock held
logs one line and tries again after `reclaim_after`.

The daemon reads its config file once, when it starts. The file is
`deadband/config.toml` in `$XDG_CONFIG_HOME`, or
`~/.config/deadband/config.toml` when `XDG_CONFIG_HOME` is not set.
`deadband schemas show deadband/config/v1` prints the file's JSON Schema,
which lists every key with what it sets. Each `[[reclaim]]` table names one
reclaimer:

```toml
# All six settings are optional; these are the defaults.
reclaim_after = "60s"     # how long memory stays short before a reclaim, and the wait between reclaims; at least 10s
reclaim_timeout = "5m"    # how long the daemon waits for one run of a reclaimer; more than 0
reclaim_memory = "4G"     # the memory each reclaim asks for
reclaim_disk = "20G"      # the disk each reclaim asks for
warm_dirs = []            # directories under a job's checkout that show a warm build, such as ["target"]
kind_tools = []           # programs whose kind and short command keep their subcommands

[[reclaim]]
name = "workspaces"
command = ["/usr/local/bin/some-tool", "reclaim", "--json"]
```

The first word of `command` must be an absolute path. The daemon looks up
a bare program name on its own `PATH`, and launchd starts the daemon with a
short `PATH` that may not hold the program, so a bare name can fail to
start. The other words are passed to the program as they are, with no
shell.

When the daemon reads the config file, it checks the program of each
reclaimer and of `on_stuck`. A program that is not an absolute path to an
executable file gets one line in the daemon's log, which names the program
and says why it cannot start. The daemon keeps the reclaimer, so a program
installed later works at the next reclaim without a restart. A reclaim whose
program cannot start records `it could not start <program>: <error>`, which
`deadband status` shows.

A size is a whole number with an optional `K`, `M`, `G` or `T`, each a
power of 1024. An age is a number with a unit, such as `90s`, `5m` or
`1h`. `reclaim_after` must be at least 10 seconds, and `reclaim_timeout`
more than 0. `warm_dirs` lists directories, relative to a job's checkout, and
only the job history reads it. Without the file, or without a `[[reclaim]]`
table, the daemon never reclaims. A file that cannot be read, has an unknown
key or breaks one of these limits turns reclaiming off, and the daemon's log
says why in one line.

`kind_tools` lists program file names. The job history and the short command
that `status` and `top` show both read it.
The kind of a job whose program is listed keeps up to two subcommand words,
such as `cargo nextest run`. The kind of any other job is the program's file
name alone, so the words after a script's name, which may be secrets, are
never recorded. A list for common build tools looks like this:

```toml
kind_tools = ["cargo", "npm", "make", "git", "just", "mxv"]
```

The daemon reclaims memory once memory pressure has been above normal, or
a job has waited for memory, for `reclaim_after` without a break. It
reclaims disk as soon as free disk is critically low, under 20 GiB or 3%
of the disk. For each reclaimer, in the order the file lists them, the
daemon:

1. Runs the command with `--need memory=4G --dry-run` added, or
   `--need disk=20G --dry-run` for disk, and logs in one line what the
   reclaimer would free.
2. When the dry run plans to free any bytes, runs the same command without
   `--dry-run`, and logs what the reclaimer freed.

The daemon stops asking once the reclaimers have freed the need. It runs
at most one reclaim at a time. After a reclaim of memory, memory must stay
short for another `reclaim_after` before the next one. Memory and disk
each keep their own wait, so a disk reclaim that frees nothing never holds
back a memory reclaim, and the reverse. After a reclaim of a resource, the
daemon waits `reclaim_after` before it reclaims that resource again. Each
reclaim in a row that frees nothing doubles that wait, up to 30 minutes,
and the log says that a reclaimer would free nothing only for the first of
them. The wait returns to `reclaim_after` once a reclaim frees something
or the resource is no longer short.

The disk need is a fixed size, 20G unless `reclaim_disk` sets another. On
a large disk, freeing 20G may leave free space under 3% of the disk, which
still holds jobs. On a 2 TB disk, for example, 3% is about 60 GB. Set
`reclaim_disk` to at least 3% of the disk to lift free space above the
limit in one reclaim; otherwise the daemon asks again after its wait. A
reclaim runs beside admission, so a job that the machine can take starts
while a reclaimer runs.

A reclaimer that cannot start, exits with a code other than 0, prints
something other than its report, or runs past `reclaim_timeout` counts as
failed. The daemon logs the failure in one line and moves on to the next
reclaimer. A failed reclaimer holds no job. deadband never signals or
stops a reclaimer. When one runs past its timeout, the daemon stops
waiting for it and starts no other reclaim until it exits.

The reclaimer reads nothing on standard input, and the daemon discards its
standard error. The daemon stops reading the report 250 ms after the
reclaimer exits, so output that a background process writes after that is
dropped, and a report that a loaded machine delivers later than that counts
as failed. It prints one JSON object on standard output. deadband
reads only `data`, an array of objects, and two fields of each object:

| Field | Meaning |
| --- | --- |
| `freed` | The bytes the item freed, or would free on a dry run, as a whole number. Required. |
| `name` | The item's name, which the log shows when `freed` is above 0. Optional. |

deadband adds up `freed` across `data` and ignores every other field. For
example:

```json
{"data": [{"name": "idle-one", "freed": 5368709120}, {"name": "busy", "freed": 0}]}
```

`deadband status` shows the last reclaim: which reclaimer ran, the need it
asked for, how long ago it ended, the bytes it reported freeing and why it
failed, if it did, such as
`last reclaim: workspaces asked for memory=4G 3m ago, freed 2 GiB`.
`deadband status --json` gives the same in the daemon's `reclaim` field,
where `at` is the time the reclaim ended in RFC 3339 in UTC with
milliseconds.

While a reclaimer runs, `deadband status` also shows `reclaiming: <need>, started
<time> ago`, and `deadband status --json` gives it in
the `reclaiming` field: `need`, and `since` as the start time in RFC 3339 in
UTC, or `null` when no reclaim runs.

## Embedding deadband in a program

A Rust program that links the `deadband` library can ask for admission and
run the daemon without the command line. Each is one call, and the
`deadband` commands use the same calls.

A program reads its environment once, as the binary's `main.rs` does:
the socket path with `socket::path`, the number of cores with
`available_parallelism`, and the `Environment` with `Environment::read`.
`admit` needs no `Machine`; only `run_daemon` does. The
priority file is matched against `Environment::cwd`, so a long-lived program
that starts a job in another directory passes an `Environment` whose `cwd`
is the job's directory. `Environment::preset` records the parallelism
variables the program had when it read its environment, so a job given its
own value for one of them needs that variable in `preset`. To admit a job,
the program calls `admit`, starts the job with the returned variables, and
ends the lease:

```rust,ignore
use deadband::admission::{Asked, admit};
use deadband::environment::Environment;

// Read the environment once, at the entry point of the program.
let socket = deadband::socket::path(|name| std::env::var_os(name));
let environment = Environment::read(|name| std::env::var_os(name), std::env::current_dir().ok());
let cores = std::thread::available_parallelism().ok();

let asked = Asked {
    socket: &socket,
    environment: &environment,
    cores,
    label: "tests",
    command: "cargo nextest run",
    // None lets the daemon find the workspace from the marker file.
    workspace: Some("triage"),
    lane: None,
    notices: None,
};
let mut notify = |line: &str| eprintln!("deadband: {line}");
let admitted = admit(&asked, &mut notify);
let status = start_the_job(admitted.variables());
admitted.end(status.code());
```

A program that measures its child can call `admitted.end_measured(status,
usage)` to give the daemon the job's usage for the job history.
`deadband::probe::child_usage` reads it on macOS. Without that call, the job's
line records null usage.

A short-lived process that exists to run one job may also call
`admitted.lower_priority(&mut notify)` before it starts the job, as
`deadband run` does. In the normal lane it lowers the calling process to
nice 5 for the rest of its life, and leaves a process that already runs at a
higher nice value where it is, so a long-lived program does not call it.

`admit` waits until the daemon admits the job. It waits at most 2 seconds for
the daemon's first answer, and then the job starts without a grant. That
limit does not cover sending the request, which has no deadline, so a daemon
that has stopped reading can block `admit` when the command text is longer
than the socket's send buffer, about 8 KiB on macOS. Once the daemon has
queued the job, the wait for the grant has no deadline and cannot be
cancelled. `admit` writes nothing itself; each line it reports goes to the callback: why the job waits, why it starts
without the daemon, and why the priority file could not be read. A job that
runs inside a granted job, or with `DEADBAND_BYPASS` set, is not asked:
`admit` reports nothing and returns no lane and no variables. The job's pid
in `deadband status` is the calling process's pid. A program that holds the
lease for a child process reports its own pid, not the child's.

To run the daemon, build a `Machine` as the binary's `main.rs` does (the probe, the socket
path, a log, the cores and the `Environment`), and call `run_daemon` with it
and the most jobs that may run at once, or `None` for the default:

```rust,ignore
let stopped = deadband::serve::run_daemon(&machine, None);
```

`run_daemon` returns when the socket cannot be bound, or when accepting a
connection fails with an error that retrying cannot fix, with the reason as
an `Unbound`. It logs and retries a failure that can pass, such as too many
open files. The daemon is no longer sound once `run_daemon` returns: its
connection threads keep running and can still grant a waiting client from
the last machine reading, because the sampler has stopped, and a second
daemon can bind the socket. A program must end its process at that point, as
the `deadband` binary does.

## Where the socket is

The daemon listens on a Unix socket, and `run` and `status` connect to it.
deadband takes the socket's path from the first of these variables that is
set and not empty:

| Variable | Socket |
| --- | --- |
| `DEADBAND_SOCKET` | the path it names |
| `DEADBAND_SOCKET_DIR` | `deadband.sock` in that directory |
| the directory `getconf DARWIN_USER_TEMP_DIR` prints (macOS only, not a variable) | `deadband.sock` in that directory, whatever `TMPDIR` is |
| `TMPDIR` | `deadband.sock` in that directory |
| `USER` | `/tmp/deadband-<USER>.sock` |

When none is set, the socket is `/tmp/deadband.sock`. The daemon makes the
socket readable and writable by its own user only. The daemon holds a lock
on a file named after the socket with `.lock` added, so a second daemon on
the same socket refuses to start. A socket file that a stopped daemon left
behind is replaced. When something other than a socket is at the socket's
path, the daemon refuses to start and removes nothing.

## Running the daemon under launchd

On macOS, `deadband service install` sets up a launchd user agent that
starts `deadband daemon` at login and starts it again whenever it stops:

```sh
deadband service install
deadband status
```

It writes `~/Library/LaunchAgents/io.github.crumbkit.deadband.plist` and
loads it with `launchctl bootstrap`. The plist runs the binary at the path
that started `deadband service install`, without following a symbolic link,
so a package manager's link keeps working after an upgrade. `--max-jobs N`
passes the same flag to the daemon.

launchd starts a user agent with few environment variables, so the plist
sets two. `TMPDIR` is what `getconf DARWIN_USER_TEMP_DIR` prints, which is
the directory that a terminal's `TMPDIR` names, so the daemon's socket is
where `deadband run` looks. `PATH` is the binary's own directory, then
`~/.local/bin`, `~/.cargo/bin`, `/opt/homebrew/bin`, `/usr/local/bin` and
the system directories, with your home directory written out in full,
because launchd does not expand `~` or `$HOME`. The jobs that `deadband run`
starts inherit their caller's `PATH`, but reclaimers inherit the daemon's.
Install copies neither variable from its caller, because an agent's sandbox
can set them to private or temporary directories.

`deadband service install --dry-run` prints the plist and the launchctl
steps and changes nothing. Running install again leaves an identical plist
alone and says so. When the plist changes and the daemon has no running or
waiting jobs, install unloads the agent with `launchctl bootout`, waits up
to five seconds for the old daemon to exit, writes the new plist and loads
it with `launchctl bootstrap`. Install counts the jobs of the daemon whose
socket is under the directory that `getconf DARWIN_USER_TEMP_DIR` prints,
whatever the caller's `TMPDIR` is. When the daemon has jobs, or does not
answer within 2 seconds while launchd has loaded the agent, install leaves
the old plist in place and the daemon running, and says that the new plist
waits for an install when no job runs. Running install again then finds the
change and reloads the agent. `--restart` reloads the agent without asking the
daemon for its jobs, and reloads it even when the plist is unchanged.

When a launchctl step fails, install says which and exits 6. A failed
bootout leaves the old plist and the old daemon, and a failed bootstrap
leaves the plist installed but not loaded; running install again finishes
either. A plist written by hand, or by an earlier install script, with the
same label is replaced like any other changed plist. Install refuses a file
at that path that is not an XML property list, and changes nothing.

`deadband service install` and `deadband service uninstall` each take an
exclusive lock on `service.lock` in the state directory
(`$XDG_STATE_HOME/deadband`, or `~/.local/state/deadband`) before they read
the plist or ask launchd, and hold it until they finish. A second command
that starts meanwhile looks for the lock every 100 milliseconds for about
ten seconds. When the first command finishes in that time, the second one
runs after it. Otherwise the second one changes nothing, names the lock
file and exits 6 with `deadband::service::locked`; run it again once the
first has finished. So an install racing an uninstall never loads an agent
whose plist the uninstall then deletes. The uninstall question names the
jobs it read before it took the lock, and uninstall asks launchd again once
it holds it.

The plist sets `KeepAlive`, so launchd starts the daemon again whenever it
stops, and `ThrottleInterval` to 60 seconds. launchd starts a daemon that
ran for 60 seconds or longer again at once, and waits out the rest of the
60 seconds for one that exited sooner. A daemon exits at start when another
daemon holds its socket, such as one started by hand with `deadband daemon`,
so while that daemon runs launchd's daemon starts once a minute instead of
every ten seconds, launchd's own default. When launchd has not loaded the
agent and a daemon already answers on the socket, install says `a daemon
that launchd did not start answers on <socket>`, loads the agent and does
not wait for an answer. Stop that daemon yourself, and launchd's daemon
takes the socket at its next start.

When no daemon answers, `deadband status` says
`daemon: not installed; run deadband service install` when the plist is
missing, and `daemon: installed but not loaded; run deadband service
install` when launchd has not loaded it. When launchd has loaded the agent
and no daemon answers, it says `daemon: loaded but not answering; see
~/Library/Logs/deadband.log`, because the daemon may have exited at start.
When launchd could not be asked, it says `daemon: not running, and launchd
could not be asked whether it loaded the agent`. `deadband status --json`
gives the same state in `daemon_state`: `running`, `not_loaded`,
`not_installed`, `loaded_silent` or `launchd_unasked`.

`deadband service install` asks the daemon for its jobs up to 20 times, 100
milliseconds apart, after it loads the agent, and reports `daemon answers` or
`daemon does not answer; see ~/Library/Logs/deadband.log`. When nothing
listens, the asks fail at once and the wait is about two seconds. A process
that accepts the connection and never replies holds each ask for two
seconds, so the wait can reach 42 seconds. `deadband service uninstall` and
`install --restart` ask the daemon that launchd runs, on the socket under
the per-user temporary directory, and print its running and waiting jobs
first. Uninstall asks only when launchd has loaded the agent. It lists the
socket, lock, log, history and configuration that stay.

Stop the daemon and remove the agent with:

```sh
deadband service uninstall
```

Uninstall asks before it acts; `--yes` skips the question, and `--no-input`
without `--yes` refuses. It deletes the plist only after launchd has
unloaded the agent, or when launchd had not loaded it. On other platforms
both commands exit with `deadband::service::unsupported`.

Stopping the daemon does not stop any job. Each running `deadband run`
keeps running its job, and each waiting one starts its job at once.

A daemon that launchd restarts starts with an empty queue and an empty list
of running jobs. Each `deadband run` whose job the earlier daemon granted
connects again, trying once per 100 ms, and tells the new daemon that its
job is already running. The new daemon counts that job at once, even past
`--max-jobs`, and new jobs wait until enough of the running jobs have ended.
A job whose daemon never returns runs to its end and keeps its own exit
code.

The new daemon does not count every job. A new daemon can admit a job in the
moment before the running jobs have told it about themselves, at most about
100 ms per retry after it starts listening. The jobs that were waiting on the
earlier daemon ask the new daemon again, and the new daemon queues them. A
waiting job that finds no daemon within 5 seconds starts at once, and the new
daemon does not count it. Jobs that started while no daemon listened are not
counted either, because they never held a place. The first restart onto this
release counts nothing from clients of an older binary, because those clients
never tell a new daemon about their jobs. In all of these cases the machine
can run more than `--max-jobs` jobs until those jobs end.
