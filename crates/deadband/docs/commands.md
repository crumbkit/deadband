# Commands of `deadband`

This reference is generated from the help of `deadband`. Edit the help in the tool's code, and not this file.

## `deadband`

Keep a busy machine usable while many heavy jobs compete for it

### Commands

- [`deadband status`](#deadband-status): Print the machine's load, memory pressure and swap, and the daemon's jobs
- [`deadband run`](#deadband-run): Wait until the daemon admits a job, then run it
- [`deadband daemon`](#deadband-daemon): Run the daemon that admits jobs, in the foreground
- [`deadband top`](#deadband-top): Show the last ten minutes of jobs, load and holds in the terminal
- [`deadband commands`](#deadband-commands): Explore deadband's commands
- [`deadband schemas`](#deadband-schemas): Explore the JSON documents that deadband prints and the files it reads

### Arguments

- `--json`: Print the result as a JSON document. The default prints text. Works after every subcommand.
- `--fields <FIELD>`: Print only these comma-separated fields, in this order. The default prints the compact columns. Takes more than one value. Works after every subcommand.
- `--long`: Add the long columns after the compact ones. The default prints only the compact columns. Works after every subcommand.
- `--full`: With --json, print every field of every row. The default prints only the fields of the columns. Works after every subcommand.

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

Run the daemon that admits jobs, in the foreground

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
