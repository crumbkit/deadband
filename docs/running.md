# Running deadband

deadband has three commands. `deadband daemon` admits jobs, `deadband run`
asks the daemon before it starts a job, and `deadband status` shows the
machine and the daemon's jobs.

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

- Fewer than `--max-jobs` jobs run. The default for `--max-jobs` is half
  the logical cores, and at least 1.
- The disk has at least 20 GiB free, and its free space is at least 3% of
  the disk.
- Memory pressure is normal.
- The 1-minute load average, plus the threads of each job admitted in the
  last 60 seconds, is at most the number of logical cores. The load average
  takes about a minute to show a new job, so the daemon adds those threads
  itself.

A job that arrives while nothing runs always starts, so one job never waits
forever. A job counts as running until its `deadband run` ends. A waiting
job prints one line on standard error that says why it waits and how many
jobs start before it. An unreadable disk holds no job, and the daemon's log
says why.

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

A job that starts at once because the daemon cannot be asked gets none of
these variables.

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
word `exclusive` matches nothing. deadband reads the file each time
`deadband run` starts, so a change applies to the next job.

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
daemon admits every job at once and its log says why.

`deadband status` prints the load, memory pressure and swap, followed by the
running and waiting jobs, or `daemon: not running` when no daemon answers.

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
sets `TMPDIR` to the directory that a terminal's `TMPDIR` names. Print it
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
