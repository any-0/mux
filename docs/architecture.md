# The architecture of mux

mux has two parts: a daemon and a client. This document tells how they operate
together.

## The daemon and the client

The **daemon** holds each pseudo-terminal (PTY). The daemon runs the shells. The
daemon keeps the layout, the scrollback, and the state of each session.

The **client** attaches to the daemon. The client draws the panes on the terminal.
The client sends each key to the daemon. The client holds no permanent state.

The first client starts the daemon automatically. The daemon then makes `Session
1`.

A lost client does not stop the daemon. See [ssh.md](ssh.md).

Only one daemon holds the state directory. A lock file makes this certain. Thus
two clients cannot make two daemons for the same sessions.

## The socket

The client finds the daemon through a Unix socket. mux selects the path in this
sequence:

1. The value of the `MUX` variable, if the variable is set.
2. `$XDG_RUNTIME_DIR/mux.sock`, if the variable is set.
3. `/tmp/mux-<UID>/mux.sock`.

mux makes each of its own directories private to the user. mux refuses a directory
with a different owner.

## The durable state

mux writes the durable state to `$XDG_STATE_HOME/mux`. mux writes to
`$HOME/.local/state/mux` if `XDG_STATE_HOME` is not set.

mux coalesces state changes when idle and submits pending changes at least every
250 ms during continuous activity. The state worker retains the latest pending
snapshot, with a 50 ms debounce and a one-second maximum debounce period.

The state has two parts:

- The **layout state** holds the session names, the roots, the selected windows,
  the pane layouts, the pane sizes, and the last working directories. mux commits
  this state atomically.
- The **pane journal** holds each output chunk and each resize of a pane. mux
  appends a framed record for each one.

Journal compaction starts at 4 MiB and keeps up to 20,000 rows with formatting.
Its threshold rises when the retained content is large. The worker replaces the
journal in order with later output. Buffered records are flushed after an 8 ms
deadline and dirty journals request a disk sync every second and at shutdown.
Output awaiting a completed sync can be lost if the machine fails.

Each pane has a 2 MiB budget for PTY output awaiting journal writes. Slow storage
holds those reservations and slows the pane's reader without blocking input or
rendering in the daemon. Scrollback uses a separate unlinked backing file; free
extents are reused after the last row or snapshot reference releases them.

A start of the daemon rebuilds each session, each window, and each pane. The
daemon replays the journals. The scrollback and the formatting come back. The
daemon then starts a new shell in the last directory of each pane.

A failed write loses the history of that pane only. The daemon continues. The
shells continue. mux reports the failure on the screen.

## The Zsh startup file

mux uses a small startup file for Zsh. The file exists while the daemon runs. mux
points `ZDOTDIR` at the directory of this file. mux puts the true directory in
`MUX_ORIGINAL_ZDOTDIR`.

The file sources the true `.zshrc` first. The shell sees `MUX` during this step.
Thus an automatic attachment in the `.zshrc` does not recurse.

The file also adds two prompt hooks. mux uses them to find the start of a prompt.

## The drawing

mux paints each screen into a cell buffer. mux compares the buffer with the frame
of the client. mux sends only the cells that changed. A repeated frame costs
nothing.

mux joins repaints into at most one frame every 8 milliseconds. A client whose
queue is full retains a pending full repaint, including after pane output stops.
Attached clients trigger process-icon sampling every 250 ms. Detached sessions
do not sample process icons, and the daemon sleeps when no work is pending.

mux sends exact colors. mux sends the nearest color of the 256-color palette to a
client without 24-bit color. mux reads `COLORTERM` and `TERM` to find out.

## The source files

| File | Content |
| --- | --- |
| `src/main.rs` | The argument parsing and the command list |
| `src/client.rs` | The client, the socket path, and the attachment |
| `src/config.rs` | The configuration, the bindings, and the actions |
| `src/frame.rs` | The cell buffer and the frame comparison |
| `src/protocol.rs` | The messages between the client and the daemon |
| `src/vim.rs` | The Vim mode |
| `src/server/mod.rs` | The daemon and the panes |
| `src/server/persist.rs` | The durable state and the Zsh startup file |
| `src/server/journal.rs` | The pane journal |
| `src/server/layout.rs` | The pane layout and the dividers |
| `src/server/render.rs` | The drawing of the panes |
| `src/server/ui.rs` | The window strip, the popups, and the dialogs |
| `src/server/input.rs` | The key handling |
| `src/server/terminal.rs` | The terminal emulation |
| `src/server/themes.rs` | The themes and the palette |
| `src/server/bell.rs` | The bell and its animation |
| `src/server/snapshot.rs` | The pane previews for the session tree |
| `src/server/command.rs` | The commands from the command line |
| `src/server/process.rs` | The foreground process of a pane |
