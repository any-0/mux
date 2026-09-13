# mux

`mux` is a small standalone terminal multiplexer built for this repository's
specific tmux workflow. A background daemon owns real PTYs, so shells keep
running when an attached terminal disconnects. It does not invoke, wrap,
configure, or replace tmux.

The interface has a narrow numbered strip on the left. Each window takes two
rows: its number is on the first row and the centered icon for the active
pane's foreground job is on the second. Only each
label and its one-cell horizontal padding are colored; the window groups are
centered vertically, and their width adapts when the window count gains or
loses digits. A continuous vertical separator divides the strip from the
terminal or pane layout. The active window keeps its colored tab but leaves
its number blank and shows a dot. Focus mode and the session tree
hide the strip and use the full terminal.

Program icons use exact executable names reported by the operating system.
Filenames, arguments, and terminal titles do not affect detection. The foreground
job's root process takes precedence over its helpers; pipelines prefer their
group leader, then the remaining root with the lowest PID. Stopped and exited
processes are excluded. Scripts identify as their interpreter. Unknown executables
show `·`. The name-to-icon list lives in `src/server/process.rs`; Nix's
`.<name>-wrapped` executable names use the same entry as `<name>`.
Icons refresh every 250 ms even in quiet panes, and unchanged icons cause no
repaint.

The separator uses the active window color and becomes a left-pointing `┤` on
the active row without coloring the terminal background. One blank vertical
column separates that line from the content on its right. A terminal bell sends
an enchanted pink-and-orchid shimmer across both rows of the window label. Its highlight
moves smoothly along the horizontal axis. The active window plays the shimmer
once; unseen windows rest on the bright magenta-and-white end of that cycle
between passes, then repeat the animation until selected. Bells from other
sessions use the same cycle in a three-cell alert at the top left, showing `!`
for one notification or the notification count for several. Neither window nor
cross-session alerts ever look like an ordinary inactive label while a bell
remains pending. `bell_style` turns the animation down to a steady label or off
entirely.
The strip uses the inspected tmux Dusk colors. Vim mode leaves the window colors
alone and uses `accent` behind them.

Split panes are divided by single lines that join where they meet, so a divider
ending against another shows `┤`, `├`, `┬`, `┴`, or `┼` rather than one line
breaking the other.

Every color mux paints is exact, and a client that reports no 24-bit color
support is sent the nearest entry of the 256-color palette instead. Terminals
are asked through `COLORTERM`, or a `TERM` that names direct color.

Every screen is painted into a cell buffer and compared against the frame the
client is already showing, so only the cells that actually changed are sent and
a repeated frame costs nothing. Repaints are coalesced into at most one frame
every 8 ms, and the daemon blocks until an event or the next process sample,
bell animation, or expiring message is due.

## Documentation

- [`docs/cli.md`](docs/cli.md) — every command, argument, and alias
- [`docs/scripting.md`](docs/scripting.md) — the `MUX` and `MUX_PANE` variables, queries, and the clipboard command
- [`docs/ssh.md`](docs/ssh.md) — lost connections, `mux auto`, and the clipboard through SSH
- [`docs/architecture.md`](docs/architecture.md) — the daemon, the socket, the state, and the source files

## Build and run

Build without installing anything:

```sh
cargo build --release
./target/release/mux
```

Linux and macOS are supported, including Apple Silicon Macs.

The first client starts the daemon and `Session 1`. Closing or losing that
client detaches it; running `mux` again reattaches while the PTYs continue to
run. A named session can be selected or created from the command line:

```sh
./target/release/mux --session work
```

The socket is `$XDG_RUNTIME_DIR/mux.sock`, or `/tmp/mux-$UID/mux.sock` when no
runtime directory is set. Directories mux picks for itself are created private
to the user and refused if something else already owns them. One daemon at a
time owns the state directory, held with a lock file, so two clients starting
at the same moment cannot end up with two daemons restoring the same sessions.
Stop the daemon and its running processes explicitly with:

```sh
./target/release/mux kill-server
```

The implemented interactive operations are also available as tmux-style
commands. When run inside mux they act on the originating pane and its attached
client:

```sh
mux choose-tree
mux detach
mux new-window
mux new-session -s work
mux rename-session coding
mux rename-window logs     # no name restores the program's title
mux split-window           # top/bottom
mux split-window -h        # left/right
mux select-pane -L
mux resize-pane -L 5       # move the nearest divider five cells
mux focus-mode             # toggle focus mode; also mux resize-pane -Z
mux break-pane             # the active pane gets a window of its own
mux join-pane -h -t 2      # and goes back into window 2
mux swap-window -t 1
mux select-window -t 2
mux vim-mode
mux kill-pane
mux kill-session
```

`mux set-session-root` and `mux jump-to-bell` expose the two mux-specific
operations. `mux stop` remains an alias for `mux kill-server`.

Read-only commands print to standard output and need no attached client, so
they can feed a prompt or a picker:

```sh
mux list-sessions   # or mux ls
mux list-windows
mux list-panes
```

Install or replace `~/.local/bin/mux` with:

```sh
cargo build --release
install -m 755 target/release/mux ~/.local/bin/mux
```

Apart from that explicit install step, no binaries or configuration files are
installed; only the durable runtime state described below is written.

## Persistence and recovery

### Testing and upgrades

Run `./scripts/test-container` to build an isolated test image and run formatting,
unit tests, vendored terminal-parser tests, and strict Clippy checks. CI runs this
against a Docker-in-Docker service; no Rust packages are installed on the host.

The client/daemon protocol has an explicit version. A mismatched running daemon
must be stopped with its matching binary before attaching with the new version.
Stopping the daemon ends running pane programs; saved layout and history are
restored into fresh shells. Installing a binary does not restart a daemon.

### Saved state

Mux keeps durable state in `$XDG_STATE_HOME/mux`, or `$HOME/.local/state/mux`
when `XDG_STATE_HOME` is unset. This is event-driven rather than timer-based:

- Session names, roots, selected windows, pane layouts, active panes, the last
  selected pane across all sessions and terminal tabs, pane dimensions, and
  last observed working directories are committed atomically whenever they
  change.
- Every PTY output chunk and resize is appended to a framed pane journal.
  The writer flushes buffered records after at most 8 ms of activity and requests
  a disk sync every second while dirty, and on clean shutdown. Output still in
  transit or awaiting a completed sync can be lost on machine failure; there is
  no fixed byte bound. A partially written final record is discarded on recovery.
- Journals initially become eligible for compaction at 4 MiB, retaining up to
  20,000 terminal rows with their formatting. The threshold grows when the
  compacted content itself is large. Replacement and disk syncing run on the
  journal worker, in order with subsequent output.
- Starting the daemon rebuilds every session, window, and pane, replays the
  journals to restore scrollback and terminal formatting, then starts a fresh
  shell in each pane's last observed directory. A default attach returns to the
  last selected pane. If the journal ended at an untouched prompt, the new
  prompt replaces it instead of adding a duplicate below it.

Layout saves are coalesced while idle and submitted at least every 250 ms during
continuous activity. The state writer keeps only the latest pending layout,
with a 50 ms debounce and a one-second maximum debounce period. Storage delays
can extend completion times. Write failures are reported without ending shells.

PTY input runs on a separate bounded writer. PTY output keeps a per-pane 2 MiB
reservation through its journal write, so slow storage applies backpressure to
the producing pane. Scrollback backing space is reused once expired rows and
their snapshots release it.

State remains after `mux stop`, an unexpected daemon exit, logout, or reboot.
Process memory cannot be reconstructed: commands that were running when the
daemon ended are replaced by a fresh interactive shell, while their terminal
output remains in the pane's scrollback.

For the inspected Zsh setup, mux uses a small runtime-only startup file while a
shell is loading. The shell sees `MUX` while sourcing `.zshrc`, so its automatic
mux attachment does not recurse.

## Default bindings

Normal mode:

| Key | Behavior |
| --- | --- |
| `Alt-a` | Enter leader mode for one command |
| `Alt-s` | Open the session tree |
| `Alt-c` | Open the theme picker |
| `Alt-t` | Create a window at the session root |
| `Alt-Shift-t` | Create and switch to a new session, rooted at the current shell directory |
| `Alt-Shift-r` | Set the current session root to the current shell directory |
| `Alt-1` … `Alt-9` | Select window 1 … 9 |
| `Alt-w` | Enter scrollback/copy mode, called Vim mode |
| `Alt-d` | Enter Vim mode already asking which character to jump to |
| `Alt-f` | Enter focus mode: hide mux's sidebar, show the active pane, and pass every other key directly to it; press `Alt-f` again to leave |

Leader mode lists its available commands in a bordered popup along the bottom of
the screen, from the moment leader is pressed:

| Key after `Alt-a` | Behavior |
| --- | --- |
| `$` | Rename the current session in a centered editor; arrows, `Home`, `End`, `Backspace`, and `Delete` edit, `Enter` accepts, `Escape` cancels, and `Ctrl-u` clears |
| `,` | Name the current window; an empty name gives it back to the program's title |
| `-` | Split the active pane top/bottom |
| `\|` | Split the active pane left/right |
| `!` | Move the active pane into a window of its own |
| `<`, `>` | Move the current window one place along the strip |
| `b` | Jump to the first pending bell, including its session and pane |
| `x` | Kill the active pane after confirmation |
| `d` | Detach this client while its sessions keep running |
| `Alt-a` | Send the leader key to the active pane |
| Arrow keys | Focus the pane in that direction |
| `Ctrl` + arrow keys | Move the divider next to the active pane; leader stays held so this repeats |

The session tree takes over the full terminal, with a compact panel on the left
and a live, event-driven pane preview on the right. It opens folded and initially
shows sessions only.
`j`/`Down` and `k`/`Up` move; `l`/`Right` unfolds; `h`/`Left` folds; `Space`
toggles a session; `Enter` chooses; and `Escape` closes the tree. Expanding a
session shows its root path, windows, and panes. A selected session previews all
of its windows in a live tiled overview; selecting a window or pane shows that
specific terminal. The overview keeps short previews top-aligned and draws
separators between window tiles. Each preview title bar uses the same active,
inactive, or animated bell background as that session's window strip. `x`
kills the selected item's session after confirmation.
The preview header also shows the selected session root. `Alt-a` opens leader
mode from the tree too; `$` then renames the selected session and `,` the
selected window.
Preview contents update directly as PTY output arrives, without a polling
interval, and each crop follows the newest content or cursor row instead of an
empty physical screen bottom. The tree initially selects the current session.
Visible rows 1 through 9 are selected directly with `1` through `9`, row 10
uses `0`, and rows 11 through 35 use `Alt-b` through `Alt-z`. The shortcut is
shown on each row.

The theme picker is a dialog rather than a screen: a card centred over the panes
it was opened from, only as big as it needs to be, so the work underneath stays
in view. The installed themes run along the top as a strip of tabs, each wearing
a few of its own colours. Every colour in the card comes from the highlighted
theme rather than the one in use. Moving along the strip previews that theme
across mux's own UI, including the sidebar and dividers behind the dialog.

Every colour defined by `~/nix/dotfiles/themes/palettes.nix` is shown as an
explicit labelled swatch, using the same role names as that source. This
includes the background and surface roles as well as text, accents, status,
selection, and diff colours.

`h`/`l`, `Left`/`Right`, `j`/`k`, and `Tab`/`Shift-Tab` all walk the strip, `1`
through `9` jump straight to a theme, `Enter` applies the highlighted one, and
`Escape` or `q` closes the picker. The theme in use is marked with `●`. A strip
too long for one row wraps onto as many as it needs.

Themes come from `$XDG_CONFIG_HOME/theme/themes`, one directory per theme, each
holding a `mux.toml`; a directory without one is not offered.
The theme in use is read from the `current` link beside them. Applying a theme
runs `theme NAME`, which is what switches every program on the machine and sends
mux a `set-theme` of its own, so mux does not recolour itself ahead of the rest
of the desktop. `theme_command` and `theme_directory` change both halves of that
arrangement.

Vim mode accepts counts and provides:

| Keys | Behavior |
| --- | --- |
| `h`, `j`, `k`, `l` | Character/row movement |
| `Ctrl-d`, `Ctrl-u` | Half-page down/up |
| `w`, `W`, `e`, `E`, `b`, `B` | Word and WORD movement |
| `0`, `^`, `$` | Start, first nonblank, and end of line |
| `gg`, `G` | First/last line; a count selects that numbered line |
| `f`, `F`, `t`, `T` + character | Find/till on the current line |
| `;`, `,` | Repeat the last find in the same/opposite direction |
| `Space`, character, hint | Label visible matches and jump directly to one; overflow targets use two or more hint keys |
| `Ctrl-o`, `Ctrl-l`, `Tab` | Move to older/newer positions in the pane-local jump list |
| `Alt-1` … `Alt-9` | Switch windows without leaving Vim mode; selecting the current window returns to the previous one |
| `/`, `?` | Forward/backward search; `Enter` accepts and `Escape` cancels |
| `n`, `N` | Repeat the last search in the same/opposite direction |
| `v`, `V`, `Ctrl-v` | Character, line, and block selection |
| `y`, `yy`, `Y` | Yank a selection or motion, whole line(s), or to the end of the line |
| `Escape` | Clear an active selection; a subsequent `Escape` leaves Vim mode |

Counts work with motions, find/search repeats, selections, `yy`, and
yank-with-motion. Yanking runs the configured clipboard command with the
selected text on standard input. Over SSH, mux instead sends an OSC 52 clipboard
write through the attached client so it reaches the local terminal. A yank then
leaves Vim mode.

OSC 52 clipboard writes produced inside a pane are relayed through attached mux
clients, so copying continues to work through nested mux and SSH sessions.

Vim state is pane-local. Switching panes or windows keeps each inactive pane at
its current viewport and restores its cursor, selection, search, and jump state
when it becomes active again.

`/` and `?` highlight every match of the pattern as it is typed, not only the
one the cursor jumps to. The match under the cursor is amber; the others keep
their terminal colors under a muted violet. `Escape` clears the highlight, and a
second `Escape` leaves Vim mode.

Leader help, Vim prompts, confirmations, rename input, and transient status or
error messages all use the same bordered popup instead of replacing a line of
terminal content. Anything that asks a question or takes input — confirmations,
rename input, Vim's search prompt — holds the middle of the screen. Anything
that only reports, meaning the leader help and transient messages such as
`yanked 42 bytes`, sits flush with the bottom rows instead, where it reads as
chrome rather than as an interruption. Transient messages stay for 1.6 seconds,
or until the next key, rather than for exactly one frame.

## Configuration

Configuration is TOML with one table per mode. Each entry maps a key to an
action. Built-in bindings are loaded first; user entries replace the action for
the same key and mode. Use `"unbind"` to remove one explicitly.

```toml
theme = "/home/j/.config/theme/current/mux.toml"
clipboard_command = ["yank"]
theme_command = ["theme"]
theme_directory = "/home/j/.config/theme/themes"
mouse = false
bell_style = "shimmer"
default_cursor_shape = "bar"

[normal]
"Alt-s" = "unbind"
"Alt-x" = "session-tree"
"Alt-q" = "detach"

[leader]
"v" = "split-vertical"

[vim]
"§" = "first-nonblank"
"Ctrl-d" = "half-page-down-center"

[tree]
"1" = "unbind"
"Alt-1" = "tree-select-1"

[themes]
"g" = "theme-select-1"
```

Key names use character keys or `Enter`, `Escape`, `Backspace`, `Tab`, `Up`,
`Down`, `Left`, `Right`, `Home`, `End`, `Delete`, `Insert`, `PageUp`,
`PageDown`, or `F1` through `F12`. Prefix modifiers with `Ctrl-`, `Alt-`, or `Shift-`. Character case
is meaningful: `w` and `W` are distinct.

Available normal actions are `session-tree`, `new-window`, `new-session`,
`set-session-root`, `select-window-1` through `select-window-9`, `enter-vim`,
`focus-mode`, `leader`, and `detach`.

Available leader actions are `rename-session`, `rename-window`,
`split-horizontal`, `split-vertical`, `focus-pane-left`, `focus-pane-down`,
`focus-pane-up`, `focus-pane-right`, `resize-pane-left`, `resize-pane-down`,
`resize-pane-up`, `resize-pane-right`, `break-pane`,
`swap-window-left`, `swap-window-right`, `jump-to-bell`, `kill-pane`, `detach`,
`leader-cancel`, and `theme-picker`.

Available tree actions are `tree-down`, `tree-up`, `tree-choose`, `tree-cancel`,
`tree-expand`, `tree-collapse`, `tree-toggle`, `kill-session`, and
`tree-select-1` through `tree-select-35`.

Available theme actions, in the `[themes]` table, are `theme-next`,
`theme-previous`, `theme-choose`, `theme-cancel`, `theme-picker`, and
`theme-select-1` through `theme-select-9`. `theme-picker` also belongs in normal, leader, or tree mode,
which is where the picker is opened from.

Available Vim actions are `left`, `down`, `up`, `right`, `half-page-down`,
`half-page-up`, `half-page-down-center`, `half-page-up-center`,
`word-forward`, `big-word-forward`, `word-end`, `big-word-end`,
`word-backward`, `big-word-backward`, `line-start`, `first-nonblank`,
`line-end`, `go-top`, `go-bottom`, `find-forward`, `find-backward`,
`till-forward`, `till-backward`, `repeat-find-forward`,
`repeat-find-backward`, `search-forward`, `search-backward`, `repeat-search`,
`repeat-search-reverse`, `jump-character`, `visual`, `visual-line`,
`jump-older`, `jump-newer`,
`visual-block`, `yank`, `yank-to-line-end`, `escape`, and the preset-oriented fixed
motions `up-3`, `down-3`, `up-10`, and `down-10`.

A theme file is the palette and nothing else. It says what its colours are, not
what they are for, because every program on the machine is themed from the same
roles and only mux knows where mux puts them:

```toml
variant = "dark"

[palette]
background = "#241e2d"
foreground = "#ece7f2"
surface = "#2e2739"
surface_raised = "#4a4158"
muted = "#968aa6"
accent = "#9fa8f2"
secondary = "#cba3d2"
success = "#8fd0a0"
warning = "#e3b46b"
danger = "#f28ca0"
selection = "#3f3552"
diff_add = "#2c4434"
diff_delete = "#4d2b38"
diff_change = "#4a4030"
```

Anything left out keeps its built-in value. `variant` decides what mux writes on
a saturated fill — a dark theme uses its own `background`, a light one white —
which is the one thing a palette cannot say in colours alone.

mux maps the palette to its own interface: the window strip idles in
`surface_raised` and takes `secondary` for the current window, with a
`accent` underlay while Vim mode is active; dividers, search
highlights, and pane edges are `surface_raised`; panels and popups sit on
`surface` in `foreground` with `muted` headings; the selected row and a Vim
selection are `selection`; popup borders are `success` and anything asking a
question is `warning`; and a bell rings in `accent`, shimmering towards
`foreground`. Point `theme` at a file to use it, or override roles in the
`[palette]` table of the config itself.

`mouse` is off by default, leaving click-to-select and scrollback to the
terminal emulator. Turning it on hands mux the mouse: clicking a pane focuses
it, clicking the strip selects that window, and the wheel scrolls the pane's
history in Vim mode, leaving it again at the bottom. Programs that ask for
mouse reporting — Vim, `less`, `htop` — receive the events themselves, in the
encoding they requested, and mux stays out of the way.

`theme_command` is what the picker runs to apply a theme, with the theme name
appended; `theme_directory` is where it looks for themes, defaulting to
`$XDG_CONFIG_HOME/theme/themes`. Both point at the `theme` command by default,
which owns the switch for every program on the machine.

`bell_style` is `"shimmer"` by default, the animation described above.
`"steady"` keeps the same colors but stops the motion: a pending bell rests on
the bright end of the cycle and stays there until its window is selected.
Nothing is animating, so the daemon sends no frames at all while a bell waits,
which is worth having over a slow connection or while recording. `"none"` drops
the visual entirely; the bell is still recorded, so `jump-to-bell` still finds
the pane that rang it. The setting is per client, so two terminals attached to
the same session can differ.

`default_cursor_shape` is `"bar"` by default; `"block"` and `"underline"` are
also supported. This per-client setting controls the cursor until an application
requests its own shape. A cursor reset restores the configured default, including
when Neovim exits. Shapes are steady, without blinking. Mux's Vim mode uses a
block. Change the setting and reattach to apply it; shell cursor overrides are
unnecessary.

Unknown tables, keys, actions, invalid mode/action combinations, an unknown
`bell_style`, and empty clipboard or theme commands stop startup with a
contextual error.

`$XDG_CONFIG_HOME/mux/config.toml`, or `$HOME/.config/mux/config.toml` when
`XDG_CONFIG_HOME` is unset, is read automatically when it exists. Nothing is
created there; a missing file simply keeps the built-in bindings. Apply a
different file, which must exist, with:

```sh
./target/release/mux --config path/to/config.toml
```

### Neovim-derived preset

[`config/julian.toml`](config/julian.toml) is ready to use and contains only the
relevant motion changes from the inspected Neovim configuration:

- `§` and `Shift-§` for first nonblank
- swapped `;` and `,` directions
- centered `Ctrl-d` and `Ctrl-u`
- `Ctrl-Left`/`Ctrl-Right` word motions
- three-row Ctrl-arrow and ten-row Shift-arrow vertical motions

Enable it explicitly; it is never installed or copied automatically:

```sh
./target/release/mux --config ./config/julian.toml
```

Copy it to `~/.config/mux/config.toml` to have it applied on every attach.

## Current limitations

- Each split owns an independent PTY. New splits divide their space evenly and
  are moved afterwards with `resize-pane`, one divider at a time; dragging a
  divider with the mouse and general tmux command compatibility remain out of
  scope.
- Windows are named explicitly with `rename-window` or fall back to the title
  their active pane sets. Names appear in the session tree and the previews,
  not in the numbered strip, which stays as wide as its numbers.
- Scrollback keeps up to 20,000 rows per pane. Older rows are encoded in small,
  independently compressed blocks; opening Vim mode decodes only the blocks it
  reads and releases them again afterwards.
- Restored panes start new shells; foreground processes and in-memory
  application state cannot survive the daemon process ending.
- Multiple clients may attach, but a session's selected window is shared and
  the most recent resize sets its PTY size.
- Terminal emulation covers the common VT/xterm behavior supported by the
  `vt100` parser; uncommon control sequences and exotic Vim features such as
  registers, macros, and marks are not implemented.
