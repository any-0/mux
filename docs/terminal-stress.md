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
  --output /tmp/mux-terminal-stress --cycles 240 --seed 76431
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

Seven test methods check pyte's extensions, including sixteen hand-authored
external-spec fixtures in `stress/fixtures/protocol.json`, tested at every byte
split. Fixture expectations are partial cell/cursor fields authored from
[kitty underline semantics](https://sw.kovidgoyal.net/kitty/underlines/) and
[xterm control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html),
never generated from mux or the Python oracle. The wide-glyph margin fixture
is independently transcribed from pinned upstream
[libvterm 61screen_unicode.test](https://github.com/neovim/libvterm/blob/934bc2fbf21800ac3458a499df8820ca5fb45fd3/t/61screen_unicode.test).
It exposed a pyte 0.8.2 defect: a wide glyph at the last column did not wrap
intact. The oracle corrects that behavior according to the upstream fixture;
replaying the saved dynamic-prompt failure then matches all thirteen checkpoints. This is specification validation,
not a claim that a third emulator was executed. Vectors cover:
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
an initial fixed prompt and then a changing command counter and actual cwd,
no user startup files, and its normal real line editor. A Unicode directory name
changes prompt width at narrow terminal geometries. CI runs 240 cycles per
profile, seed 76431, with a 30-minute per-profile action budget and 45-minute job
budget. Summary files report actual elapsed seconds, actions and checkpoints.
The seeded session repeatedly:

- Runs `cat` on a Unicode/blank-line fixture and `head` on styled long lines.
- Runs `cat` on 2,400 logical styled rows every fourth cycle. The 240-cycle soak
  produces 144,000 large-output rows and 8,880 head rows per profile,
  plus small cat output (432,000 large rows across the three shells).
  Long rows wrap; styles cycle through no underline, straight, double, curly,
  dotted and dashed, with RGB underline color and bold/faint/italic transitions.
- Edits a long wrapped command with Home/end, forward moves and deletions;
  repeatedly resizes among five geometries, redraws the line editor, interrupts
  the command, and checks the resulting multiline prompt and cursor.
- Runs real Vim on a Unicode file, edits its buffer, resizes its alternate
  screen, explicitly redraws, exits, and checks the shell screen restoration.
- Attaches a second equal-size client, then a larger third client. Four
  competing per-client resizes alternate ownership of the shared PTY size,
  as specified in README. Each client is compared with the independent
  source projected into its own top-left viewport; cells outside the source
  are expected default blanks and cursor positions are clamped to that viewport.
  Returns all clients to equal geometry, then exercises graceful and SIGKILL
  recovery while a foreground command holds a known styled screen.
- Creates twelve windows, traverses the strip, and checks tile backgrounds
  and labels from the scenario's count/selection model.

The unproxied suite checks shell → Vim → shell → cat → shell, Vim suspension and
`fg` resumption, background-job notifications, twelve windows, narrow/one-row
resizes, selection, focus-mode entry/exit, session/window renames, a second
session, pane splitting/killing, the session tree, leader and mux Vim modes.

Completed output is gated by decoded cells and bounded output quiescence,
with five-second deadlines rather than unbounded sleeps. Process icons have a
bounded polling deadline because their updates are asynchronous. Clock, cwd
time labels, prompt colors, data fixtures and resize RNG are controlled.
Dynamic cwd/counters are captured, so offline replay does not rerun expansion. The
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
were added subsequently; all three shells passed shape checks and the new
session/mode assertions on `656b777` in
[run 36750933430](https://github.com/any-0/mux/actions/runs/36750933430).
The expanded 240-cycle dynamic-prompt/unequal-client soak is a later revision
and must pass before its results are claimed.

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
separately. Unequal-size clients are tested after repaint against the documented
last-resize-wins policy; reader suspension is exercised by the separate interaction workload; server queue saturation is not measured.

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
multiline-prompt deduplication without shell integration. Dynamic cwd and command-count prompts are covered; wall-clock/git prompts
and asynchronous prompt plugins are not yet covered. Output quiescence
is a bounded synchronization mechanism, not proof that an arbitrarily delayed
plugin has finished. These limitations are deliberate and must not be presented
as full terminal compatibility or exhaustive persistence coverage.

## Supported-feature inventory and independent expectations

| Behavior | Workload and expectation | Remaining boundary |
| --- | --- | --- |
| Terminal cells, Unicode, SGR, cursor | Source PTY replay versus client bytes; external-spec fixtures | Pixel rendering, blink, strike, OSC palette, graphics |
| Multiline shell prompts and redraw | Real bash/zsh/fish, changing cwd/counter, wrapped edits, SIGWINCH, Ctrl-L | Arbitrary async prompt plugins |
| Input delivery | `input_sessions.py`: edited command writes exact action-derived file bytes; multiline bracketed paste must await Enter | Every keyboard protocol/mouse gesture |
| Vim alternate screen | Actual Vim edit, resize, redraw and exit; source cell/style/cursor oracle | Exhaustive Vim interaction |
| Multiple clients | Three retained terminal streams, competing sizes and cursor clipping | Sustained backpressure |
| Windows, sessions, panes and modes | Script-owned names/counts/selection, split/kill, tree, focus, leader/Vim mode and foreground jobs | Every nested split topology and UI editing path |
| Persistence | Graceful/SIGKILL restore of completed styled screen | Idle prompt deduplication and arbitrary crash loss window |
| Reproduction/minimization | Seeded action log, linear offline replay, retained raw bytes and minimized witnesses | General automatic action delta debugging |

Input-file expectations do not depend on the terminal oracle: this catches
misdelivered keys even if the source and rendered screens agree. The separate
input workload can be reproduced with:

```sh
./scripts/stress-nix python3 stress/input_sessions.py --binary target/debug/mux \
  --output /tmp/mux-input-bash --shell bash
```

The user-reported undisclosed bug remains unresolved. These two valid baseline
fixes are not claimed to identify or fix that bug; test selection follows the
supported-feature inventory rather than a presumed trigger.


## Interaction workload

`interaction_sessions.py` adds FIFO-gated background output while each real
shell holds a wrapped command under its embedded-newline prompt. A producer
acknowledgement records the commanded output index; comparisons use captured
source bytes, never the acknowledgement as a rendered-screen oracle. Explicit
Ctrl-L gates compare cells, attributes and cursor before/after resize. Editing
then submits a command whose exact file bytes are owned by the scenario.

A second real client is SIGSTOP'd while a bounded sequence of changing styled
screens is emitted. The active client must continue to match the source oracle;
after SIGCONT both clients must converge. This proves reader suspension and
continued progress, but does not prove that kernel buffers and the daemon's
bounded writer queue reached saturation. A faithful saturation test still
needs a transport-level occupancy/backpressure observable; output count alone
is insufficient. Arbitrary delayed async prompt plugins also remain uncovered.

Copy mode searches and yanks a script-owned unique Unicode line surrounded by
long wrapped logical lines. Resizing while in copy mode must retain the exact
clipboard bytes at three geometries. This is an independent selection/history
observable, not an emulator-derived prediction of mux's copy viewport. Cell,
style and cursor comparisons resume after leaving copy mode and repainting;
full independent styled copy-viewport/reflow emulation remains a gap.

```sh
./scripts/stress-nix python3 stress/interaction_sessions.py \
  --binary target/debug/mux --output /tmp/mux-interactions --shell bash
./scripts/stress-nix python3 stress/replay.py /tmp/mux-interactions
```
