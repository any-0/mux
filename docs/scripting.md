# How to write scripts for mux

mux gives each pane two environment variables. A script reads these variables to
find the daemon and the pane.

## The environment variables

| Variable | Content |
| --- | --- |
| `MUX` | The path of the socket of the daemon |
| `MUX_PANE` | The number of the pane that holds the shell |

mux sets both variables in each pane. mux sets them again after a restart of the
daemon.

### `MUX`

A script uses `MUX` for two purposes.

Use `MUX` to find out if the shell is in a pane:

```sh
if [[ -n "$MUX" ]]; then
    echo "This shell is in a mux pane."
fi
```

The mux client also reads `MUX` to find the daemon. `MUX` has a higher priority
than the default socket path. See [architecture.md](architecture.md).

### `MUX_PANE`

Each command reads `MUX_PANE` to find the pane that starts it. Thus `mux
split-window` splits the correct pane.

`MUX_PANE` can name a pane that is not available. This happens in a shell that
continues after a restart of the daemon. mux refuses the command instead of
sending it to an unrelated attached client.

Pass `--pane ID` to target a pane explicitly. This overrides `MUX_PANE`, which
is useful when a controller script manages several panes. Pane IDs remain stable
while the pane exists. An invalid or stale ID is an error.

mux removes the `TMUX` variable and the `TMUX_PANE` variable from each pane. Thus
a program in a mux pane does not find a tmux pane.

## Queries in a script

Three commands print information and change nothing. They do not need an attached
client. Use them in a prompt or in a picker.

```sh
mux list-sessions    # or mux ls
mux list-windows
mux list-panes
```

Each command prints one line for each item. See [cli.md](cli.md).

Add `--json` for a JSON array suitable for scripts:

```sh
mux list-sessions --json
mux list-windows --pane 12 --json
mux list-panes --pane 12 --json
```

Session objects include `id`, `name`, `root`, window and pane counts, and the
`attached` and `current` states. Window objects include their numeric `id`,
session identity, name, pane count, and active and focus-mode states. Pane
objects include their stable `id`, numeric `index`, session and window identity,
working directory, dimensions, and active state.

Commands that change state require an attached client in the target pane's
session. Window IDs in JSON are one-based positions and can change when windows
move; session and pane IDs remain stable while those objects exist.

## The clipboard command

The Vim mode of mux copies text with an external command. The
`clipboard_command` setting names this command. The default command is `yank`.

mux sends the selected text to the standard input of a command run by the daemon.
For clients attached through SSH, mux instead sends the selection to the client,
which emits OSC 52 directly to the terminal. See [ssh.md](ssh.md).

Set a different command in the configuration file:

```toml
clipboard_command = ["yank"]
```

The list must hold a command name. An empty list stops mux with an error.

## The theme command

The theme picker of mux applies a theme with an external command. The
`theme_command` setting names this command. The default command is `theme`. mux
adds the name of the theme to the end of the command.

mux does not change its own colors first. The external command changes each
program on the machine. That command then sends `mux set-theme` to mux.

The `theme_directory` setting names the directory of the themes. The default
directory is `$XDG_CONFIG_HOME/theme/themes`.

## An example: a status bar

This example prints the number of sessions:

```sh
mux ls | wc -l
```

This example prints the name of the current session with `jq`:

```sh
mux ls --json | jq -r '.[] | select(.current) | .name'
```
