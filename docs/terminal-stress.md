# Independent terminal correctness stress

This harness starts from verified main `d6dd228054231e77772bd17a412d8f0d07871835`
(the merge of PR #1, after PR #2). It is separate from `benchmarks/` and does not
change the benchmark runtime pin. The draft PR is
https://github.com/any-0/mux/pull/4.

## Reproduce with the project Nix pin

`.nix/flake.lock` selects nixpkgs
`2fc6539b481e1d2569f25f8799236694180c0993`; the `stress` devShell supplies Rust,
Cargo, Python/pyte, bash, zsh, fish, Vim and coreutils from that pin. The root
packaging flake and a host Rust installation are not used.

```sh
./scripts/test-nix
./scripts/stress-nix python3 -m unittest discover -s stress -p test_oracle.py -v
./scripts/stress-nix cargo build --locked
./scripts/stress-nix python3 stress/run.py --binary target/debug/mux \
  --output /tmp/mux-terminal-stress --cycles 40 --seed 76431
./scripts/stress-nix python3 stress/replay.py /tmp/mux-terminal-stress/bash
./scripts/stress-nix python3 stress/replay.py /tmp/mux-terminal-stress/zsh
./scripts/stress-nix python3 stress/replay.py /tmp/mux-terminal-stress/fish
./scripts/stress-nix python3 stress/sidebar_sessions.py --binary target/debug/mux \
  --output /tmp/mux-sidebar-bash --shell bash
```

Use new output directories. Repeat the last command for zsh and fish. Increase
`--cycles` for a longer run; `--cycles N --seed 76431 --shells bash` keeps the
same first N generated cycles and isolates a failing profile. The minimized
witnesses below eliminate the need to repeat the full session for either bug.

The hosted workflow runs each shell on its own Ubuntu runner, with fresh HOME,
XDG config/state/runtime directories, shell startup files, daemon, and client
PTYs. Local profiles also use separate directories/processes but do not claim
OS-level container isolation. Docker's registry and the official Nix release
endpoint returned HTTP 403 in the cloud workspace. No alternate Rust setup,
proxy changes, restriction bypass, or replacement package pin was used. The
repository's supported Nix CI route executed successfully.

## What makes the oracle independent

`stress/proxy.py` launches a real shell on an inner controlling PTY. It records
actual inner output bytes, input bytes, and `TIOCSWINSZ` events before forwarding
output to mux. `stress/oracle.py` feeds those source bytes to pyte, independently
of mux's vendored Rust vt100 parser, frame renderer, session state, or journal
encoding. A second independent terminal instance decodes the real attached
client's output; the pane rectangle is compared with the source terminal.

The comparison includes every visible cell's text, wide continuation cells,
foreground/background, bold, faint, italic, inverse, underline style and color,
plus cursor position, visibility and shape. Canonicalization is limited to NFC
combining characters and equivalent indexed-color encodings (for example SGR
36 and SGR 38;5;6). Default color remains distinct from an explicit color.

Small independently specified protocol vectors check pyte's extensions:
colon/semicolon SGR colors, all six underline states, SGR 22 transitions,
private modifyOtherKeys negotiation, split UTF-8/control strings, DEC 1049
alternate screen, and cursor position/visibility/shape. The expected protocol
for bold and faint follows [xterm's actual SGR implementation](https://github.com/ThomasDickey/xterm-snapshots/blob/master/charproc.c),
which sets the BOLD and ATR_FAINT bits independently. SGR 22 clears both;
this is also documented in [kitty's independent bold/faint controls](https://sw.kovidgoyal.net/kitty/misc-protocol/#independent-control-of-bold-and-faint-sgr-properties).
The original stress failure was a style mismatch, despite identical text and
cursor positions.

Because the recorder proxy is itself the foreground process of mux's outer
pane PTY, it cannot provide a valid shell/Vim process-icon test. The separate
`sidebar_sessions.py` suite runs the real shell directly, without that proxy.
Its expected window count, active index, tile positions, foreground icons,
mode colors, session names/counts, and pane selection come from the scripted
actions and the declared profile palette. Queries and rendered cells are
observations only; they do not generate expected state.

## Real sustained workload

Each shell has a three-line colored custom prompt with embedded newlines,
fixed cwd/time labels, no user startup files, and its normal real line editor.
The seeded session repeatedly:

- Runs `cat` on a Unicode/blank-line fixture and `head` on styled long lines.
- Runs `cat` on 2,400 logical styled rows every fourth cycle. Forty cycles
  produce 24,000 large-output rows, plus 1,480 head rows and small cat output.
  Long rows wrap; styles cycle through no underline, straight, double, curly,
  dotted and dashed, with RGB underline color and bold/faint/italic transitions.
- Edits a long wrapped command with Home/end, forward moves and deletions;
  repeatedly resizes among five geometries, redraws the line editor, interrupts
  the command, and checks the resulting multiline prompt and cursor.
- Runs real Vim on a Unicode file, edits its buffer, resizes its alternate
  screen, explicitly redraws, exits, and checks the shell screen restoration.
- Attaches a second equal-size client and compares both streams to the same
  independent source; then exercises graceful and SIGKILL recovery while a
  foreground command holds a known styled screen.
- Creates twelve windows, traverses the strip, and checks tile backgrounds
  and labels from the scenario's count/selection model.

The unproxied suite checks shell → Vim → shell → cat → shell, Vim suspension and
`fg` resumption, background-job notifications, twelve windows, narrow/one-row
resizes, selection, focus-mode entry/exit, session/window renames, a second
session, pane splitting/killing, the session tree, leader and mux Vim modes.

Completed output is gated by decoded cells and bounded output quiescence,
with five-second deadlines rather than unbounded sleeps. Process icons have a
bounded polling deadline because their updates are asynchronous. Clock, cwd
labels, prompt colors, data fixtures and resize RNG are controlled. The
artifacts retain actual chunk boundaries, so replay does not depend on timing.

## Confirmed failures and fixes

1. **Simultaneous bold and faint lost bold.** Main's vt100 attributes treated
   SGR 1 and 2 as mutually exclusive. Styled `head` exposed 840 attribute
   differences. The minimized `BOTH` witness exposes four differing cells with
   equal text/cursor and retained undercurl/color. Attributes now retain both;
   formatted intensity transitions clear old bits before setting the next
   combination. Rust regressions cover parsing, SGR 22, all sixteen transition
   pairs, formatted round trips and journal compaction.
2. **Sidebar process tiles lose their rightmost background at ten windows.**
   Two-digit numbers widen the tile to four cells, while the icon row previously
   painted three. With the test's explicitly configured inactive background
   `#223344`, main leaves row 2, column 3 (zero-based) at default background.
   The icon string is now padded to the full label width in both ordinary and
   bell rendering. The PTY witness and Rust render regression check active and
   inactive tile edges.

```sh
# Fixed implementations must pass.
./scripts/stress-nix python3 stress/witnesses.py --case intensity \
  --binary target/debug/mux --output /tmp/fixed-intensity
./scripts/stress-nix python3 stress/witnesses.py --case sidebar \
  --binary target/debug/mux --output /tmp/fixed-sidebar

# Build the original main with the same pinned toolchain, then prove both fail.
git worktree add --detach /tmp/mux-stress-baseline d6dd228054231e77772bd17a412d8f0d07871835
./scripts/stress-nix sh -c 'cd /tmp/mux-stress-baseline; cargo build --locked --target-dir /tmp/mux-baseline-target'
./scripts/stress-nix python3 stress/witnesses.py --case intensity --expect-baseline-failure \
  --binary /tmp/mux-baseline-target/debug/mux --output /tmp/baseline-intensity
./scripts/stress-nix python3 stress/witnesses.py --case sidebar --expect-baseline-failure \
  --binary /tmp/mux-baseline-target/debug/mux --output /tmp/baseline-sidebar
```

The expectation flag accepts only the named correctness failure; startup
errors/timeouts do not count as a baseline reproduction. Small raw baseline
bytes, resize/action traces and provenance are checked in under
`stress/evidence/`. Full baseline/fixed and long-session artifacts are in
[run 36748241151](https://github.com/any-0/mux/actions/runs/36748241151):
[bash, including both baseline witnesses](https://github.com/any-0/mux/actions/runs/36748241151/artifacts/11113218944),
[zsh](https://github.com/any-0/mux/actions/runs/36748241151/artifacts/11113298703),
and [fish](https://github.com/any-0/mux/actions/runs/36748241151/artifacts/11114460020).
That run passed all three long sessions (221 checkpoints each), unproxied
sidebar sessions, graceful/crash recovery, and offline checkpoint replay.
Its comparisons covered cursor position/visibility; cursor-shape comparisons
were added subsequently and require the later run to pass before being claimed.

Artifacts contain client `.ansi`, byte-offset resize events, pane capture JSONL,
action logs, source snapshots, full mismatch snapshots/cell diffs, profile
configs, daemon stderr, summaries, and (in later runs) binary/toolchain manifests.
Replay is linear in trace size. Resize action indices disambiguate an idle
checkpoint and a later resize sharing the same client byte offset.

## Explicit limits

This is a terminal-state oracle, not a pixel/font-rendering test. It covers the
listed SGR properties, not every terminal extension (for example sixel,
hyperlinks, blinking/strikethrough, runtime OSC palette changes or kitty input).
The PTY proxy changes process ancestry, which is why process icons are tested
separately. Multi-client coverage uses equal dimensions and does not establish
semantics for competing unequal-size clients or severely stalled consumers.

Terminal resize/reflow behavior is not specified by ECMA-48. The cell oracle
compares **after explicit shell/Vim repaint** at the new geometry, rather than
assuming pyte and mux use identical pre-repaint clipping/reflow policies.
The existing Rust tests still cover scrollback reflow and wide-character
preservation. This harness does not independently prove every copy-mode
scrollback cell after arbitrary reflow histories.

Recovery holds a known foreground screen with bracketed paste disabled; fresh
recorded shell output is composed over the independent pre-stop screen.
It checks completed durable output after a 1.5-second checkpoint allowance,
not an arbitrary crash-window loss budget, recovery of live OS jobs, or idle
multiline-prompt deduplication without shell integration. Dynamic time/git/cwd
prompts and asynchronous prompt plugins are not yet covered. Output quiescence
is a bounded synchronization mechanism, not proof that an arbitrarily delayed
plugin has finished. These limitations are deliberate and must not be presented
as full terminal compatibility or exhaustive persistence coverage.
