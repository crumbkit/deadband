# Commands of `deadband`

This reference is generated from the help of `deadband`. Edit the help in the tool's code, and not this file.

## `deadband`

Keep a busy machine usable while many heavy jobs compete for it

### Commands

- [`deadband status`](#deadband-status): Print the machine's load, memory pressure and swap, and the daemon's jobs
- [`deadband run`](#deadband-run): Wait until the daemon admits a job, then run it
- [`deadband daemon`](#deadband-daemon): Run the daemon that admits jobs, in the foreground
- [`deadband service`](#deadband-service): Install or uninstall the launchd agent that runs the daemon at login
- [`deadband top`](#deadband-top): Show the last ten minutes of jobs, load and holds in the terminal
- [`deadband commands`](#deadband-commands): Explore deadband's commands
- [`deadband schemas`](#deadband-schemas): Explore the JSON documents that deadband prints and the files it reads
- [`deadband skills`](#deadband-skills): Give coding agents deadband's skill
- [`deadband completion`](#deadband-completion): Set up deadband's completion in a shell

### Arguments

- `--json`: Print the result as a JSON document. The default prints text. Works after every subcommand.
- `--fields <FIELD>`: Print only these comma-separated fields, in this order. The default prints the compact columns. Takes more than one value. Works after every subcommand.
- `--long`: Add the long columns after the compact ones. The default prints only the compact columns. Works after every subcommand.
- `--full`: With --json, print every field of every row. The default prints only the fields of the columns. Works after every subcommand.
- `--no-input`: Fail instead of asking a question. Works after every subcommand.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband status`

Print the machine's load, memory pressure and swap, and the daemon's jobs

### Examples

Print the machine's load, memory pressure and swap.

```sh
deadband status
```

Print the same sample as a JSON document.

```sh
deadband status --json
```

Effects: read-only, closed-world.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband run`

Wait until the daemon admits a job, then run it.

`run` exits with the job's exit status, or with 128 plus the signal's number when a signal ended the job. It exits with 127 when the program is not found and with 126 when it is found but cannot start, as `env` and `timeout` do.

### Arguments

- `--label <LABEL>`: The label that `deadband status` shows for the job. The default is the program's name
- `--lane <LANE>`: The lane the job waits in. Waiting fast jobs start first and get one more thread, and normal jobs run at nice 5. The default is the fast lane when the priority file matches the job, and the normal lane otherwise. One of `fast`, `normal`.
- `<COMMAND>`: The command to run, after `--`. Required. Takes more than one value.

### Examples

Run cargo test once the daemon admits it.

```sh
deadband run --label tests -- cargo test
```

Effects: additive, not idempotent, open-world.

Writes: applies all items or none.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.
- `126`: the program given to `run` was found but could not start.
- `127`: the program given to `run` was not found.

## `deadband daemon`

Run the daemon that admits jobs, in the foreground.

`daemon` exits with 0 when another daemon already serves the socket, so launchd does not start it again.

### Arguments

- `--max-jobs <N>`: The most jobs that run at once. The default is a quarter of the logical cores, rounded up, and at least 1

### Examples

Admit jobs while at most 2 run at once.

```sh
deadband daemon --max-jobs 2
```

Effects: additive, not idempotent, closed-world.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband service`

Install or uninstall the launchd agent that runs the daemon at login

### Commands

- [`deadband service install`](#deadband-service-install): Install the launchd agent that runs the daemon at login and restarts it when it stops
- [`deadband service uninstall`](#deadband-service-uninstall): Unload the launchd agent and delete its plist. Running jobs keep running

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband service install`

Install the launchd agent that runs the daemon at login and restarts it when it stops

### Arguments

- `--max-jobs <N>`: The most jobs that the daemon runs at once, passed to `deadband daemon`. The default is the daemon's own
- `--restart`: Reload a changed agent even while the daemon has running or waiting jobs. The jobs keep running
- `--dry-run`: Report what the command would change, and change nothing

### Examples

Install the launchd agent that runs the daemon at login.

```sh
deadband service install
```

Print the plist and the launchctl steps.

```sh
deadband service install --dry-run
```

Effects: additive, idempotent, closed-world.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.
- `6`: deadband could not install or uninstall its launchd agent.

## `deadband service uninstall`

Unload the launchd agent and delete its plist. Running jobs keep running

### Arguments

- `--yes`: Proceed without asking for confirmation
- `--dry-run`: Report what the command would change, and change nothing

### Examples

Unload the launchd agent and delete its plist without asking.

```sh
deadband service uninstall --yes
```

Effects: destructive, idempotent, closed-world.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.
- `6`: deadband could not install or uninstall its launchd agent.

## `deadband top`

Show the last ten minutes of jobs, load and holds in the terminal

### Examples

Show the last ten minutes of jobs in the terminal.

```sh
deadband top
```

Effects: read-only, closed-world.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.
- `5`: `top` has no terminal to draw on, or lost it.
- `130`: the person pressed Ctrl-C in `top`.

## `deadband commands`

Explore deadband's commands

### Commands

- [`deadband commands list`](#deadband-commands-list): List deadband's commands with their arguments, examples and columns

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband commands list`

List deadband's commands with their arguments, examples and columns

### Arguments

- `<PATH>`: The names of the commands below the root that lead to the branch, such as `books list`. Takes more than one value.
- `--limit <N>`: Print at most this many rows. Default: `25`.
- `--all`: Print every row

### Examples

List every command with its arguments.

```sh
deadband commands list
```

List only `deadband run` and its arguments.

```sh
deadband commands list run
```

### Columns

- `path`: The command line that runs the command, such as `shelf books list`.
- `about`: The command's one-line description, or its full description when it has no one-line description.

Effects: read-only, closed-world.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband schemas`

Explore the JSON documents that deadband prints and the files it reads

### Commands

- [`deadband schemas list`](#deadband-schemas-list): List the JSON documents that deadband prints and the files it reads
- [`deadband schemas show`](#deadband-schemas-show): Print the JSON Schema of each document or file format

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband schemas list`

List the JSON documents that deadband prints and the files it reads

### Arguments

- `--limit <N>`: Print at most this many rows. Default: `25`.
- `--all`: Print every row

### Examples

List the documents deadband prints and its config file format.

```sh
deadband schemas list
```

### Columns

- `id`: The document's id, such as `shelf/books-list/v1`, which `schemas show` takes.
- `kind`: output when a command prints the document with its schema id, unwrapped when a command prints it without the envelope that holds its schema id, and file when it is a file format that users write and no command prints.
- `form`: stream when a command prints the document as a stream, one document on each line as each row arrives, and document when a command prints it once or it is a file format.
- `description`: What the document holds, in one line.

### Columns that `--long` adds

- `commands`: The command lines that print the document. It is empty for the failure document, which every command prints when it fails with --json, and for a file format.

Effects: read-only, closed-world.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband schemas show`

Print the JSON Schema of each document or file format

### Arguments

- `<ID>`: The ids of the documents that `schemas list` lists, separated by spaces. Required. Takes more than one value.

### Examples

Print the JSON Schema of the daemon's config file.

```sh
deadband schemas show deadband/config/v1
```

### Columns

- `id`: The document's id, such as `shelf/books-list/v1`.
- `schema`: The document's 2020-12 JSON Schema.

Effects: read-only, closed-world.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband skills`

Give coding agents deadband's skill

### Commands

- [`deadband skills install`](#deadband-skills-install): Install deadband's skill into each agent skills directory that exists
- [`deadband skills check`](#deadband-skills-check): Check that each installed copy of deadband's skill matches this deadband
- [`deadband skills print`](#deadband-skills-print): Print deadband's skill

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband skills install`

Install deadband's skill into each agent skills directory that exists

### Arguments

- `--dir <DIR>`: Install into this skills directory in place of the agent skills directories
- `--force`: Replace a file or a link that install did not write with the skill's own folder

### Examples

Install deadband's skill into each agent skills directory that exists.

```sh
deadband skills install
```

Install it, replacing a file or a link that install did not write.

```sh
deadband skills install --force
```

### Columns

- `state`: What install did: failed, skipped, written or unchanged.
- `dirs`: The skills directories that share this copy, as named. Directories that resolve to one place through a symbolic link share one copy.
- `reason`: Why install skipped or failed, or what it replaced under --force, such as `linked to /src/shelf/skill`, or null.

### Columns that `--long` adds

- `path`: The SKILL.md file that install wrote or left, at its real path.

Effects: additive, idempotent, closed-world.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband skills check`

Check that each installed copy of deadband's skill matches this deadband

### Arguments

- `--dir <DIR>`: Check this skills directory in place of the agent skills directories

### Examples

Report each agent skills directory whose copy is missing or differs.

```sh
deadband skills check
```

### Columns

- `state`: Whether the copy is the binary's: missing, differs, unreadable, linked or current.
- `dirs`: The skills directories that share this copy, as named. Directories that resolve to one place through a symbolic link share one copy.
- `reason`: Why the copy differs, cannot be read or is linked, such as `written by shelf 0.2.0` or `linked to /src/shelf/skill`, or null.

### Columns that `--long` adds

- `path`: The SKILL.md file that was checked, at its real path.

Effects: read-only, closed-world.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband skills print`

Print deadband's skill

### Examples

Print deadband's skill.

```sh
deadband skills print
```

Effects: read-only, closed-world.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband completion`

Set up deadband's completion in a shell

### Commands

- [`deadband completion install`](#deadband-completion-install): Install deadband's completion where the shell loads it
- [`deadband completion check`](#deadband-completion-check): Check that deadband's installed completion file matches this deadband
- [`deadband completion print`](#deadband-completion-print): Print deadband's completion registration script for a shell

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband completion install`

Install deadband's completion where the shell loads it

### Arguments

- `--shell <SHELL>`: Install for this shell in place of the one that SHELL names. One of `bash`, `elvish`, `fish`, `powershell`, `zsh`.
- `--force`: Replace a file or a link that install did not write; a link's target stays as it is

### Examples

Install deadband's completion for fish.

```sh
deadband completion install --shell fish
```

Install it for zsh, replacing a file or a link that install did not write.

```sh
deadband completion install --shell zsh --force
```

### Columns

- `state`: What install did: failed, skipped, written or unchanged.
- `shell`: The shell: bash, elvish, fish, powershell or zsh.
- `path`: The completion file that install wrote or left. When the file is a link to a file that install wrote, the path of that file.
- `reason`: Why install skipped or failed, or what it replaced under --force, such as `linked to /src/shelf/shelf.fish`, or null.

Effects: additive, idempotent, closed-world.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband completion check`

Check that deadband's installed completion file matches this deadband

### Arguments

- `--shell <SHELL>`: Check the file of this shell in place of the one that SHELL names. One of `bash`, `elvish`, `fish`, `powershell`, `zsh`.

### Examples

Report zsh's completion file when it is missing or differs.

```sh
deadband completion check --shell zsh
```

### Columns

- `state`: Whether the file holds the script that install writes: missing, differs, unreadable, linked or current.
- `shell`: The shell: bash, elvish, fish, powershell or zsh.
- `path`: The completion file that was checked. When the file is a link to a file that install wrote, the path of that file.
- `reason`: Why the file differs, cannot be read or is linked, such as `linked to /src/shelf/shelf.fish`, or null.

Effects: read-only, closed-world.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.

## `deadband completion print`

Print deadband's completion registration script for a shell

### Arguments

- `--shell <SHELL>`: Print the script of this shell in place of the one that SHELL names. One of `bash`, `elvish`, `fish`, `powershell`, `zsh`.

### Examples

Print the registration script for fish.

```sh
deadband completion print --shell fish
```

Effects: read-only, closed-world.

### Exit codes

- `3`: deadband could not read the machine.
- `4`: the daemon could not listen on its socket.
