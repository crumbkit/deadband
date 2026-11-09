# Running deadband

deadband has four commands. `deadband daemon` admits jobs, `deadband run`
asks the daemon before it starts a job, `deadband status` shows the
machine and the daemon's jobs, and `deadband top` draws the last ten minutes
in the terminal.

## Installing

Build and install the binary from a checkout:

```sh
cargo install --locked --path crates/deadband
```

Cargo puts `deadband` in `~/.cargo/bin`.

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
`deadband run` was killed. Each job shows at most its label, its lane and
its times; the view shows no command line. A program that embeds the
deadband library sets `default-features = false` to leave the view out.

A SIGTERM or SIGKILL sent to `deadband top` leaves the terminal in raw mode.
Run `reset` to restore it.

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

`DEADBAND_BYPASS=1` makes `deadband run` run its job at once without asking
the daemon and without setting its threads or its priority.

## When the daemon cannot be asked

`deadband run` does not wait on a daemon that is missing, silent or gone.
When no daemon listens, when the daemon does not answer within 2 seconds,
or when it closes the connection, the job starts at once and `deadband run`
prints one line on standard error that says why. After the daemon has
queued a job, `deadband run` waits for the grant with no deadline, so a
daemon that hangs while it keeps the connection open also keeps the job
waiting. Stopping that daemon closes the connection, and the job then
starts at once. When the probe cannot read the machine, the
daemon admits each job as soon as a place is free, and its log says why.

`deadband status` prints the load, memory pressure and swap, followed by the
running and waiting jobs and the last reclaim, or `daemon: not running`
when no daemon answers. When jobs wait, it also prints a line that names the
first waiting job and says why it does not start yet.

## Reclaiming memory and disk

A job that waits for memory or disk waits for something to free it. The
daemon can ask a reclaimer, an external command that you name, to free
memory or disk when the machine runs short. A reclaimer might stop the
servers of idle workspaces, or delete build output that can be rebuilt.
deadband never stops anything itself; the reclaimer decides what to stop.

The daemon reads its config file once, when it starts. The file is
`deadband/config.toml` in `$XDG_CONFIG_HOME`, or
`~/.config/deadband/config.toml` when `XDG_CONFIG_HOME` is not set. Each
`[[reclaim]]` table names one reclaimer:

```toml
# All four settings are optional; these are the defaults.
reclaim_after = "60s"     # how long memory stays short before a reclaim, and the wait between reclaims; at least 10s
reclaim_timeout = "5m"    # how long the daemon waits for one run of a reclaimer; more than 0
reclaim_memory = "4G"     # the memory each reclaim asks for
reclaim_disk = "20G"      # the disk each reclaim asks for

[[reclaim]]
name = "workspaces"
command = ["/usr/local/bin/some-tool", "reclaim", "--json"]
```

The first word of `command` must be an absolute path. The daemon looks up
a bare program name on its own `PATH`, and launchd starts the daemon with a
short `PATH` that may not hold the program, so a bare name can fail to
start. The other words are passed to the program as they are, with no
shell.

A size is a whole number with an optional `K`, `M`, `G` or `T`, each a
power of 1024. An age is a number with a unit, such as `90s`, `5m` or
`1h`. `reclaim_after` must be at least 10 seconds, and `reclaim_timeout`
more than 0. Without the file, or without a `[[reclaim]]` table, the daemon
never reclaims. A file that cannot be read, has an unknown key or breaks
one of these limits turns reclaiming off, and the daemon's log says why in
one line.

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
standard error. It prints one JSON object on standard output. deadband
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

`deadband status` shows the last reclaim: when it ended, which reclaimer
ran, the need it asked for, the bytes it reported freeing and why it
failed, if it did. `deadband status --json` gives the same in the daemon's
`reclaim` field.

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
    lane: None,
};
let mut notify = |line: &str| eprintln!("deadband: {line}");
let admitted = admit(&asked, &mut notify);
let status = start_the_job(admitted.variables());
admitted.end(status.code());
```

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
| `TMPDIR` | `deadband.sock` in that directory, which macOS makes per user |
| `USER` | `/tmp/deadband-<USER>.sock` |

When none is set, the socket is `/tmp/deadband.sock`. The daemon makes the
socket readable and writable by its own user only. The daemon holds a lock
on a file named after the socket with `.lock` added, so a second daemon on
the same socket refuses to start. A socket file that a stopped daemon left
behind is replaced. When something other than a socket is at the socket's
path, the daemon refuses to start and removes nothing.

## Running the daemon under launchd

launchd starts a user agent with few environment variables, so the agent
sets `TMPDIR` to the directory that a terminal's `TMPDIR` names. It also
sets `PATH`, with your home directory written out in full, because
launchd does not expand `~` or `$HOME`. The jobs that `deadband run` starts
inherit their caller's `PATH`, but reclaimers inherit the daemon's. Print it
with:

```sh
getconf DARWIN_USER_TEMP_DIR
```

Save this plist as `~/Library/LaunchAgents/io.github.crumbkit.deadband.plist`,
with your home directory and that temporary directory in place of the
examples:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>io.github.crumbkit.deadband</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/me/.cargo/bin/deadband</string>
    <string>daemon</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>TMPDIR</key>
    <string>/var/folders/ab/abcdefgh/T/</string>
    <key>PATH</key>
    <string>/Users/me/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardErrorPath</key>
  <string>/Users/me/Library/Logs/deadband.log</string>
</dict>
</plist>
```

Load it, and check that the daemon answers:

```sh
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.github.crumbkit.deadband.plist
deadband status
```

Stop it with:

```sh
launchctl bootout gui/$(id -u)/io.github.crumbkit.deadband
```

Stopping the daemon does not stop any job. Each running `deadband run`
keeps running its job, and each waiting one starts its job at once.

A daemon that launchd restarts starts with an empty queue and an empty list
of running jobs. It admits up to `--max-jobs` more jobs while jobs that the
earlier daemon granted may still be running. The jobs that were waiting on
the earlier daemon start at once when the connection closes, and the new
daemon does not count them. The machine can therefore run more than
`--max-jobs` jobs until those jobs end.
