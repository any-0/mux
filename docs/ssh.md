# mux and the Secure Shell

The daemon holds each pseudo-terminal (PTY). A client only draws the panes. Thus
a lost connection does not stop the programs in the panes.

## What happens after a lost connection

The Secure Shell (SSH) connection stops. The mux client stops with it. The daemon
continues. Each program in each pane continues.

Connect again. Then run `mux`. The client attaches to the same sessions. The
scrollback of each pane is complete.

A closed terminal window gives the same result. The client detaches. The daemon
continues.

## Automatic attachment after an SSH login

`mux auto` attaches in the same way as `mux`. `mux auto` first asks a question if
the `SSH_TTY` variable is set and is not empty:

```
Attach to mux? [Y/n]
```

Answer `y`, `yes`, or nothing to attach. Answer `n` or `no` to get a usual shell.
mux does not ask this question for a local login.

Put `mux auto` in a shell startup file. This example is for Zsh:

```sh
if [[ -o interactive ]] && [[ -z "$MUX" ]] && [[ "$TERM" != "dumb" ]]; then
    mux auto
fi
```

The `[[ -z "$MUX" ]]` test is necessary. mux sets `MUX` in each pane. Without the
test, each new pane starts a second client.

## The nesting guard

A client refuses to attach from a pane of the same daemon. mux gives this error:

```
already inside this mux; run `env -u MUX mux` to attach a second client anyway
```

The reason is the size of the terminal. The inner client uses the pane as its
terminal. The inner client then resizes that pane. Each resize causes the next
resize. The pane becomes one column wide.

Use `env -u MUX mux` if you want a second client for the same sessions.

## The clipboard through SSH

When the attached client reports a nonempty `SSH_TTY`, Vim-mode yanks travel to
that client over the mux socket. The client emits OSC 52 through its terminal,
allowing the local terminal emulator to update its clipboard. No remote clipboard
command is needed for this path.

For other clients, the daemon runs the configured clipboard command (by default,
`yank`). Programs inside panes can also emit OSC 52, which mux forwards to clients
attached to the same session. Pane OSC sequences are bounded to 64 KiB; oversized
clipboard sequences are discarded rather than copied partially.

The local terminal emulator must permit OSC 52 clipboard writes.
