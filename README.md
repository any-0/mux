# mux

A small standalone terminal multiplexer. A background daemon owns the PTYs, so
shells keep running when the attached terminal goes away, and sessions, layouts
and scrollback survive a daemon restart or reboot.

- A narrow numbered strip on the left shows each window's number and an icon
  for the active pane's foreground program. Bells animate the window's label.
- Split panes, focus (zoom) mode, a session tree with live previews, a theme
  picker, and a Vim-style scrollback/copy mode.
- Only changed cells are sent to the terminal, at most one frame every 8 ms.
  Clients whose terminal stops reading are never disconnected; they get the
  current screen once they catch up.
- 24-bit color where the terminal supports it (detected via `COLORTERM`/`TERM`),
  the nearest 256-color entry otherwise. Styled underlines only go to terminals
  known to draw them.

Further docs:

- [`docs/cli.md`](docs/cli.md) — every command, argument, and alias
- [`docs/scripting.md`](docs/scripting.md) — `MUX`/`MUX_PANE`, queries, the clipboard command
- [`docs/ssh.md`](docs/ssh.md) — lost connections, `mux auto`, clipboard over SSH
- [`docs/architecture.md`](docs/architecture.md) — daemon, socket, state, source layout

## Build and run

```sh
scripts/install         # incremental release build, installed to ~/.local/bin/mux
mux                     # attach, starting the daemon and "Session 1" if needed
mux --session work      # attach to (or create) a named session
mux kill-server         # stop the daemon and every pane (alias: mux stop)
```

Linux and macOS are supported. The socket is `$XDG_RUNTIME_DIR/mux.sock`, or
`/tmp/mux-$UID/mux.sock`. Only one daemon at a time owns the state directory.

Interactive operations are also available as tmux-style commands, which act on
the originating pane when run inside mux (`mux split-window -h`,
`mux resize-pane -L 5`, `mux join-pane -h -t 2`, ...). Read-only queries
(`mux ls`, `list-windows`, `list-panes`) need no attached client. See
[`docs/cli.md`](docs/cli.md).

The client/daemon protocol is versioned: a running daemon from an older build
must be stopped (`mux kill-server`) before attaching with a new one. Panes are
then restored into fresh shells.

### Development

```sh
nix develop ./.nix     # or direnv; any recent stable Rust also works
cargo test && cargo test --manifest-path vendor/vt100/Cargo.toml --lib
cargo clippy --all-targets -- -D warnings
(cd vendor/vt100 && cargo clippy --all-targets -- -D warnings)
```

`vendor/vt100` is a patched copy of the `vt100` crate; see its `MUX_PATCH.md`.

## Persistence

State lives in `$XDG_STATE_HOME/mux` (default `~/.local/state/mux`):

- The session tree (sessions, roots, windows, layouts, active panes, pane sizes
  and working directories) is written atomically whenever it changes.
- Every pane's output and resizes are appended to a journal, flushed within
  8 ms and synced every second. Journals are compacted to the last 20,000 rows
  once they grow past 4 MiB.
- On startup the daemon rebuilds every session, replays the journals to restore
  scrollback and formatting, and starts a fresh shell in each pane's last
  directory. Running programs themselves cannot be restored.

## Default bindings

Normal mode:

| Key | Behavior |
| --- | --- |
| `Alt-a` | Leader mode for one command |
| `Alt-s` | Session tree |
| `Alt-c` | Theme picker |
| `Alt-t` | New window at the session root |
| `Alt-Shift-t` | New session rooted at the current directory |
| `Alt-Shift-r` | Set the session root to the current directory |
| `Alt-1` … `Alt-9` | Select window |
| `Alt-w` | Vim mode (scrollback/copy) |
| `Alt-d` | Vim mode, starting a character jump |
| `Alt-f` | Toggle focus mode: hide the strip, pass every key to the pane |

Leader mode (`Alt-a`, then), with a help popup listing these:

| Key | Behavior |
| --- | --- |
| `$` | Rename session (`Enter` accepts, `Escape` cancels, `Ctrl-u` clears) |
| `,` | Rename window; empty restores the program's title |
| `-` / `\|` | Split top/bottom / left/right |
| `!` | Move the active pane into its own window |
| `<`, `>` | Move the window along the strip |
| `b` | Jump to the first pending bell |
| `x` | Kill the active pane (asks first) |
| `d` | Detach |
| `r` | Repaint the whole screen |
| `Alt-a` | Send the leader key to the pane |
| Arrows | Focus the pane in that direction |
| `Ctrl` + arrows | Move the adjacent divider (leader stays held) |

Session tree: `j`/`k` move, `l`/`h` unfold/fold, `Space` toggles, `Enter`
chooses, `Escape` closes, `x` kills the session (asks first). Rows are also
selected directly with `1`–`9`, `0`, and `Alt-b`–`Alt-z`. `Alt-a` `$`/`,`
renames the selected session/window.

Theme picker: `h`/`l`, arrows or `Tab` walk the themes (previewing them), `1`–`9`
jump, `Enter` applies, `Escape`/`q` closes.

Vim mode (counts supported):

| Keys | Behavior |
| --- | --- |
| `h` `j` `k` `l`, `Ctrl-d` `Ctrl-u` | Move, half page |
| `w` `W` `e` `E` `b` `B` | Word / WORD motions |
| `0` `^` `$`, `gg` `G` | Line start / first nonblank / end, top / bottom (count = line) |
| `f` `F` `t` `T`, `;` `,` | Find/till on the line, repeat |
| `Space` + character + hint | Jump to any visible match |
| `Ctrl-o`, `Ctrl-l`/`Tab` | Older / newer jump-list position |
| `/` `?`, `n` `N` | Search (all matches highlighted), repeat |
| `v` `V` `Ctrl-v` | Character / line / block selection |
| `y` `yy` `Y` | Yank selection or motion / lines / to end of line |
| `Alt-1` … `Alt-9` | Switch windows without leaving Vim mode |
| `Escape` | Clear selection, then leave |

Yanking pipes the text to `clipboard_command` and leaves Vim mode; over SSH it
is sent as OSC 52 through the client instead. OSC 52 writes from programs in a
pane are relayed too. Vim state is kept per pane.

## Configuration

`$XDG_CONFIG_HOME/mux/config.toml` (default `~/.config/mux/config.toml`) is read
when it exists; `--config PATH` uses another file. Unknown keys, actions or
values are startup errors.

```toml
theme = "/home/j/.config/theme/current/mux.toml"  # palette file, see below
clipboard_command = ["yank"]       # receives yanked text on stdin
theme_command = ["theme"]          # run with the theme name appended by the picker
theme_directory = "/home/j/.config/theme/themes"  # one dir per theme, each with mux.toml
mouse = false                      # true: click focuses panes/windows, wheel scrolls
bell_style = "shimmer"             # or "steady" (no animation) or "none"
default_cursor_shape = "bar"       # or "block", "underline"
# truecolor = true                 # override terminal detection
# styled_underlines = true         # likewise; also MUX_STYLED_UNDERLINES=1/0
glyphs = "font"                    # "text" for fonts without the private-use icons

[normal]                           # one table per mode: key = action
"Alt-s" = "unbind"
"Alt-x" = "session-tree"

[leader]
"v" = "split-vertical"

[vim]
"Ctrl-d" = "half-page-down-center"

[tree]
"Alt-1" = "tree-select-1"

[themes]
"g" = "theme-select-1"

[palette]                          # optional role overrides, as in a theme file
accent = "#9fa8f2"
```

Keys are characters or `Enter`, `Escape`, `Backspace`, `Tab`, `Up`, `Down`,
`Left`, `Right`, `Home`, `End`, `Delete`, `Insert`, `PageUp`, `PageDown`,
`F1`–`F12`, with optional `Ctrl-`, `Alt-`, `Shift-` prefixes. Case matters.
User entries replace the built-in action for that key; `"unbind"` removes it.

Actions per mode:

- **normal**: `session-tree`, `new-window`, `new-session`, `set-session-root`,
  `select-window-1`…`9`, `enter-vim`, `focus-mode`, `leader`, `detach`,
  `refresh-client`, `theme-picker`
- **leader**: `rename-session`, `rename-window`, `split-horizontal`,
  `split-vertical`, `focus-pane-{left,down,up,right}`,
  `resize-pane-{left,down,up,right}`, `break-pane`, `swap-window-{left,right}`,
  `jump-to-bell`, `kill-pane`, `detach`, `refresh-client`, `leader-cancel`,
  `theme-picker`
- **tree**: `tree-down`, `tree-up`, `tree-choose`, `tree-cancel`,
  `tree-expand`, `tree-collapse`, `tree-toggle`, `kill-session`,
  `tree-select-1`…`35`, `theme-picker`
- **themes**: `theme-next`, `theme-previous`, `theme-choose`, `theme-cancel`,
  `theme-picker`, `theme-select-1`…`9`
- **vim**: `left`, `down`, `up`, `right`, `half-page-{down,up}[-center]`,
  `word-forward`, `big-word-forward`, `word-end`, `big-word-end`,
  `word-backward`, `big-word-backward`, `line-start`, `first-nonblank`,
  `line-end`, `go-top`, `go-bottom`, `{find,till}-{forward,backward}`,
  `repeat-find-{forward,backward}`, `search-{forward,backward}`,
  `repeat-search`, `repeat-search-reverse`, `jump-character`, `visual`,
  `visual-line`, `visual-block`, `jump-older`, `jump-newer`, `yank`,
  `yank-to-line-end`, `escape`, `up-3`, `down-3`, `up-10`, `down-10`

### Themes

A theme file is a palette; anything left out keeps its built-in value.
`variant` decides the text color on saturated fills (dark: `background`,
light: white).

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

The picker lists the directories in `theme_directory` that contain a
`mux.toml`, marks the one the `current` link points at, and applies a choice by
running `theme_command NAME`; that command is expected to call
`mux set-theme PATH` back.

## Limitations

- New splits divide space evenly; resize with `resize-pane`, not mouse drags.
- Scrollback is capped at 20,000 rows per pane (stored compressed).
- Restored panes start new shells; running programs do not survive the daemon.
- Multiple clients may attach, but a session's selected window is shared and
  the most recent resize sets the PTY size.
- Emulation covers common VT/xterm behavior; Vim mode has no registers, macros
  or marks.
