# The mux command-line interface

This document lists each mux command. The list is complete.

mux has three kinds of invocation:

- An **attach** invocation starts a client. `mux` alone is an attach invocation.
- A **query** prints information to the standard output. A query does not need an
  attached client.
- A **command** changes the state of the daemon. A command needs an attached
  client.

## How to find help

`mux --help` prints the full command list. `mux -h` does the same.

mux does not supply help for one command. `mux new-window --help` does not print
help. The command reads `--help` as an argument and reports an error. Use this
document or `mux --help` instead.

mux has no shell completions.

## Queries

A query prints one line for each item. A query does not need an attached client.
Use a query in a prompt or in a picker. Add `--json` to print a JSON array with
stable session and pane IDs. Add `--pane ID` to choose the session or window
whose items are listed.

| Command | Alias | Result |
| --- | --- | --- |
| `mux list-sessions` | `mux ls` | One line for each session |
| `mux list-windows` | — | One line for each window of the current session |
| `mux list-panes` | — | One line for each pane of the current window |

## Commands

A command acts on the pane that starts it. mux reads the `MUX_PANE` variable to
find that pane. Add `--pane ID` to target a different pane explicitly. The
explicit option has priority over `MUX_PANE`. mux reports an error when either
value names a pane that no longer exists; it does not redirect the command to a
different attached client.

State-changing commands still need an attached client in the target pane's
session. Targeting a pane in a detached session reports an error and does not
move a client from another session.

### Sessions

| Command | Arguments | Result |
| --- | --- | --- |
| `mux new-session [-s NAME]` | An optional name | Makes a session and selects it |
| `mux rename-session NAME` | Exactly one name | Renames the current session |
| `mux set-session-root` | None | Makes the directory of the active shell the session root |
| `mux kill-session` | None | Kills the current session |
| `mux choose-tree` | None | Opens the session tree |
| `mux detach` | None | Detaches the active client |

`mux detach-client` is an alias for `mux detach`.

### Windows

| Command | Arguments | Result |
| --- | --- | --- |
| `mux new-window` | None | Makes a window |
| `mux rename-window [NAME]` | One name, or none | Names the current window |
| `mux select-window [-t] NUMBER` | A number from 1 through 9 | Selects that window |
| `mux swap-window [-t] NUMBER` | A number of 1 or more | Exchanges the current window with that window |

`mux rename-window` without a name clears the name. The window then shows the
title of its program again.

The `-t` flag is optional for `select-window` and for `swap-window`. `mux
select-window 2` and `mux select-window -t 2` give the same result.

`select-window` accepts a number from 1 through 9 only. A larger number causes an
error.

### Panes

| Command | Arguments | Result |
| --- | --- | --- |
| `mux split-window [-h\|-v]` | An optional flag | Splits the active pane |
| `mux select-pane -L\|-D\|-U\|-R` | Exactly one flag | Focuses the adjacent pane |
| `mux resize-pane -L\|-D\|-U\|-R [N]` | A flag and an optional count | Moves the nearest divider by N cells |
| `mux resize-pane -Z` | The `-Z` flag | Toggles focus mode |
| `mux focus-mode` | None | Toggles focus mode |
| `mux break-pane` | None | Moves the active pane into a new window |
| `mux join-pane [-h\|-v] -t NUMBER` | An optional flag and a window number | Moves the active pane into that window |
| `mux kill-pane` | None | Kills the active pane |
| `mux vim-mode` | None | Starts the Vim mode |

The flags of `split-window` follow the tmux convention. Read the flags carefully:

- `mux split-window` makes a top pane and a bottom pane.
- `mux split-window -v` makes a top pane and a bottom pane. The result is the
  same as no flag.
- `mux split-window -h` makes a left pane and a right pane.

`mux resize-pane` moves the divider by one cell if you give no count.

`mux resize-pane -Z` and `mux focus-mode` do the same operation.

`mux join-pane -t NUMBER` puts the pane on the left or on the right. Give `-v` for
a top position or a bottom position.

### Other commands

| Command | Arguments | Result |
| --- | --- | --- |
| `mux jump-to-bell` | None | Goes to the first pending bell |
| `mux set-theme PATH` | Exactly one path | Applies the colors of that file |
| `mux kill-server` | None | Stops the daemon and each of its panes |

`mux stop` is an alias for `mux kill-server`.

## Options

The attach options apply only to an attach invocation. `--pane` applies to
commands and queries, while `--json` applies only to queries.

| Option | Argument | Result |
| --- | --- | --- |
| `--config PATH` | A path to a TOML file | Applies the bindings of that file after the built-in bindings |
| `--session NAME` | A session name | Attaches to that session, or makes it |
| `--pane ID` | A pane ID | Targets a command or selects a query context |
| `--json` | None | Prints a query as a JSON array |
| `--help`, `-h` | None | Prints the command list |

Session and pane IDs identify those objects for their lifetimes. A window's
numeric JSON `id` is its current one-based position within the session.

The file that `--config` names must exist. mux stops with an error if the file is
not available.

Without `--config`, mux reads `$XDG_CONFIG_HOME/mux/config.toml`. mux reads
`$HOME/.config/mux/config.toml` if `XDG_CONFIG_HOME` is not set. A missing file is
not an error. mux then uses the built-in bindings.

## The `auto` invocation

`mux auto` attaches in the same way as `mux`. `mux auto` first asks for a
confirmation if the `SSH_TTY` variable is set and is not empty. Use `mux auto` in
a shell startup file. See [ssh.md](ssh.md).

`auto` must be the first argument. `mux auto --session work` is correct.

## The reserved invocation

`mux __server SOCKET` starts the daemon. mux starts the daemon automatically. Do
not use this invocation manually.

## Errors

mux writes each error to the standard error output. mux then stops with a
non-zero status.

An unknown argument gives this message: `mux: unknown argument "..."; run mux --help`.

An attach invocation from a pane of the same daemon gives an error. This message
tells you how to attach a second client:

```
mux: already inside this mux; run `env -u MUX mux` to attach a second client anyway
```

## A complete alias list

| Alias | Full command |
| --- | --- |
| `mux ls` | `mux list-sessions` |
| `mux detach-client` | `mux detach` |
| `mux stop` | `mux kill-server` |
| `mux resize-pane -Z` | `mux focus-mode` |
| `mux split-window -v` | `mux split-window` |
