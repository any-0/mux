# mux test harness

Black-box and differential tests for mux. The harness drives the real `mux`
binary through pseudo-terminals and parses everything the client writes with an
independent terminal emulator (`alacritty_terminal`), so a test sees what a
person at that terminal would see. It is a crate of its own, outside mux's
workspace, because `alacritty_terminal` enables `vte/std`, which must not leak
into mux's build through feature unification.

## Running

```sh
cargo build --release          # in the repository: the binary under test
cd harness
cargo test --release           # everything, a few minutes
cargo test --release --test e2e_render
```

`MUX_BIN` selects another binary. Running the suite against an older build is
how a fix is shown to be one: see `scripts/check-harness-witnesses`.

On Linux, `harness/docker/run` builds mux and runs mux's own tests, vt100's
tests and the whole harness in a `rust:1.90` container with vim, less, nano,
dialog, htop, tmux and Midnight Commander installed:

```sh
harness/docker/run                    # everything
harness/docker/run --test e2e_apps    # one suite
```

Knobs for longer runs: `MUX_FUZZ_SEEDS`, `MUX_RESIZE_SEEDS`,
`MUX_PANIC_SEEDS`, `MUX_OPERATION_SEEDS` and `MUX_OPERATION_STEPS`.
`MUX_KEEP_RIG=1` keeps a rig's directory (daemon log, fixtures) for a
post-mortem.

## Suites

| Suite | What it holds mux to |
| --- | --- |
| `emulator_diff` | vt100 and alacritty agree cell by cell (text, colours, attributes, cursor) on random streams of everything real programs send, at sizes from 1x2 to 40x3 and 24x80. A disagreement is shrunk to a minimal stream. |
| `resize_invariants` | Any sequence of resizes keeps every logical line of text and the cursor's place in it. |
| `panic_fuzz` | Random sequences and raw bytes, resizes, scrollback views and the journal's format/restore round trip, at sizes down to 1x1, never panic and history always restores; no single escape sequence costs more than a few milliseconds. |
| `e2e_render` | A focused pane shows exactly what its program drew; incremental painting matches a fresh client's full paint after resizes, including resizing back to the same size; a stalled terminal is neither disconnected nor flooded with stale frames; a terminal that draws some characters wider than mux does stays aligned and never scrolls. |
| `e2e_input` | Every key a terminal sends, in normal and application cursor mode, reaches the program byte for byte. |
| `e2e_features` | Splits keep the newest output, underline styles and colours follow what the terminal supports, focus reports, text glyphs. |
| `e2e_persistence` | A killed or stopped daemon comes back with every pane, its history and its colours, at another size too, and after compaction. |
| `e2e_operations` | Random sequences of splits, windows, kills, focus mode, resizes, detaches and output, checked against a fresh client every few steps. |
| `e2e_apps` | vim, less, nano, dialog, Midnight Commander and tmux look the same in a mux pane as run directly. Missing programs are skipped. |

## The reference

alacritty is a faithful xterm-style emulator, but not a perfect one. Where it
departs from xterm or tmux in ways generated streams reach, `Reference::feed_token`
corrects for it, and each correction is listed there with the alacritty source it
comes from: tabs at a pending wrap, `ED 1` on the second row, `DCH` past the end,
the phantom pending wrap with autowrap off, `CBT` without tab stops, cursor
movement ignoring margins, edits at a pending wrap, `DECSTR`, `DECRC` not
restoring origin mode or the GL shift, and `CHA` in origin mode. alacritty also
tears wide characters in insert mode; a stream that makes it do so is discarded
rather than judged.
