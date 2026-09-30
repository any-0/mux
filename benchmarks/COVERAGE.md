# Operation inventory and coverage matrix

Source inventory: pinned mux `d6dd228` CLI in `src/main.rs`, default bindings and
valid mode actions in `src/config.rs`, action handlers in `src/server/input.rs`,
and README shortcut/Vim tables. Comparator: Nix tmux 3.6a commands and default
key tables, resurrect save/restore scripts and continuum status scheduler.
Bindings are explicitly configured for tmux so the same attached input invokes
matched semantic units. A mux session maps to a tmux session, window to window,
and pane to independently running PTY. A horizontal split creates left/right
panes; vertical creates top/bottom panes, not a new window.

This is a coverage inventory, **not a claim that all rows are measured**. The
new operation suite is undergoing real Nix CI diagnostic validation. Existing
accepted evidence is retained in README and results/hosted-ci/36744338567.

| Operation family | New attached-input suite | Visible correctness endpoint / semantic verification | Existing evidence or limit |
| --- | --- | --- | --- |
| Populated startup/attach | Implemented across 1×1, 3×2, 6×4 layouts | Original tagged shell is rendered, full fixture geometry recorded | Existing cold empty daemon startup and clean persistence restart measured separately |
| Window switch away/back | Implemented | Target tagged pane replaces source; window count unchanged | None previously |
| Session switch away/back | Implemented | Tagged other session / original pane appears | Second session has equal one-pane tagged history |
| Create window | Implemented | New real shell prompt plus window-count increase | No CLI acknowledgement endpoint |
| Left/right and top/bottom split | Implemented | Both old and fresh-shell content visible; two independent PTYs | Temporary 100×40 window matches mutation units at every scale |
| Focus in four directions | Implemented | Cursor reaches selected tagged prompt; active pane verified | Two orientations in matched temporary window |
| Resize divider in both axes | Implemented | Divider leaves old coordinate; dimensions must change | One-cell movement, not no-op resize |
| Zoom/fullscreen and return | Implemented | Other pane disappears/reappears | mux also hides sidebar; zoom viewport dimensions differ and must be labeled |
| Delete pane | Implemented | Deleted marker disappears, survivor remains; pane count decreases | mux confirmation included in input sequence |
| Break/move pane into own window | Implemented | Same live shell appears alone; window count increases | Join-pane has CLI API only in mux; not yet timed |
| Delete window | Implemented via last-pane deletion | Window count decreases and shell content disappears | Equivalent supported lifecycle operation |
| Create/delete session | Implemented | Fresh shell appears / original session is rendered | UI naming differs; counts/identity are required supplemental gates |
| Second client attach | Implemented | Same real populated pane rendered in second PTY | Clients same size; simultaneous control/size contention not yet measured |
| Detach/reattach | Reattach implemented | Original live tagged shell reappears after actual detach | Detach completion currently a preparation gate, not a reported render latency |
| History top/bottom and search | Implemented | Requested historical record appears in pane content | Existing PageUp viewport latency measured with exact 40-row transitions |
| History selection/copy/paste | Pending | Must verify copied bytes and visible selection/result | Existing complete history copy is correctness evidence, not copy latency |
| Window reorder/swap | Pending | Position and tagged live content must move together | Supported leader < and >; CLI swap-window also available |
| Window/session rename | Pending | Edited accepted label visible and metadata equal | Separate from startup preparation naming |
| Session root/CWD changes | Not yet timed | Root/cwd must match shell and tree metadata | Existing clean restore cwd fidelity passed |
| Session tree expand/collapse/preview | Not yet timed | Selected tree row and live preview must agree | mux and tmux choose-tree previews differ; report independently |
| Theme/color selection | Incomparable UI, not yet timed | Palette and style oracle required | tmux has colors/options, no equivalent mux theme picker |
| Bell navigation | Not yet timed | Bell target pane/session visible and acknowledgement cleared | mux jump-to-bell; tmux activity navigation differs |
| Vim word/find/count motions, jumps, visual/block modes | Not yet timed | Cursor, selection and bytes oracle needed | tmux copy mode overlaps partly; mux label jumps/jump list have no identical counterpart |
| Terminal/client resizing | Not yet timed | Correct pane PTY geometry and repainted content | Existing restoration wrap/geometry gates are not resize latency |
| Busy/background output | Implemented idle/busy factor | Fixed-size 50 Hz background frame producer in separate identical window | Existing foreground finite output throughput measured |
| Clean save/shutdown/restart/restore | Already measured | Full histories/style/layout/cwd/fresh live shells | 30 accepted trials per variant |
| Default-period crash recovery | Explicitly unmeasured | Scheduled snapshot age, loss accounting, live shells | Repeated same-runner suite exceeds hosted six-hour limit |
| Shell startup variants / arbitrary applications | Not yet compared | Shell/application readiness/content oracle required | Same pinned Bash in current suite; Vim integration tests are not performance measurements |
| CLI list/control operations | Not used as interactive latency surrogate | Queries provide post-render semantic evidence only | CLI-only controls need a separately labeled control-plane suite |

## Experimental factors and acceptance

Three scales have 1, 3 or 6 populated windows with 1, 2 or 4 panes per window,
plus one dedicated background window and a one-pane reference session. Each pane
gets 1,000 tagged ASCII history rows; temporary mutation panes get 100 identical
rows. Every variant uses the same pinned Bash, content area 100×40, 20,000 history
capacity and matched split geometry. Outer terminal compensation matches the
pane viewport despite different chrome; physical outer dimensions differ.
The busy factor produces fixed-size non-scrolling ANSI frames at 50 Hz in the
background window, rather than allowing history size to depend on backend speed.

20 retained measured process trials plus one excluded warm-up per scale/load/
variant, deterministic shuffled variant order, sequentially on one hosted VM.
Per-operation distributions are process-trial samples, not thousands of treated-
as-independent key presses. Input-write to decoded-visible-result is measured;
resource samples surround operations outside their latency interval. CPU ticks
are a lower bound and may miss exited helpers. Raw input, commands, frame snapshots,
geometry, per-process PSS/RSS, timing and failure evidence are retained. Any failed
trial or unequal fixture geometry blocks all three variant aggregates for that
factor; failed samples are never dropped to obtain passing timings.

The operation order is fixed within a trial to make reversible mutations restore
the fixture; variants are shuffled within each paired block. This does not claim
independent randomized operation order. Populated attach is a running-daemon
startup, not cold daemon startup or journal restoration. No PR4 changes are mixed
in: runtime remains `d6dd228`, while PR4 `656b777` is an unmerged candidate.
