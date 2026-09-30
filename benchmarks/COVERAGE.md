# Supported-operation benchmark inventory

Runtime pinned to main `d6dd228`; PR #4 fixes are not mixed into measurements.
Inventory sources: [mux CLI](../src/main.rs), [bindings](../src/config.rs),
[input handling](../src/server/input.rs), [Vim motions](../src/vim.rs),
[tmux command manual](https://man.openbsd.org/tmux.1). The manual is discovery
context; executed comparator is locked tmux 3.6a, and its actual options/bindings
and pane metadata are the final compatibility gates.

A pane means one shell PTY, a window one pane layout, a session a collection of
windows. These units match both tools. `new window vertically/horizontally`
means splitting a pane; creating a multiplexer window is a separate operation.
There is no claim that every tmux-only control or every synonymous key deserves
a separate performance metric.

| Operation family | Current evidence / expanded path | Endpoint / comparability |
| --- | --- | --- |
| Fresh empty startup | Accepted prior 30 trials | Decoded shell prompt |
| Populated layout startup | Interactive suite | New attached process sees all expected seeded pane prompts; existing daemon, explicitly attach rather than restore |
| Window creation/deletion | Interactive suite | New shell prompt; deleted single-pane window's marker disappears; metadata afterward |
| Window switching/back | Interactive suite | Expected seeded window viewport |
| Session creation/deletion/switch/back | Interactive suite | New shell / expected session-specific viewport; native confirmation separately prepared |
| Horizontal/vertical splits | Interactive suite | New independently running shell; two panes confirmed afterward |
| Pane deletion | Interactive suite, both orientations | Deleted pane absent, survivor rendered, pane count verified |
| Directional focus | Interactive suite, both orientations | Cursor at survivor's actual prompt; pane metadata afterward |
| Divider resize | Interactive suite, both orientations | Changed viewport followed by changed queried pane dimensions |
| Zoom/fullscreen and return | Interactive suite | Only active pane visible, then both visible; mux hides sidebar too, unlike tmux |
| Window reordering | Interactive suite | Changed bar/status with same pane content, metadata retained |
| Move pane into own window | Interactive suite (`break-pane`) | Moved pane visible alone; queried one-pane window |
| Join pane into another window | Interactive suite, separately labeled CLI dispatch | Command dispatch to both moved and survivor markers; no standard mux attached-key action |
| Detach/reattach | Interactive suite | Actual attached process exits; fresh attached process displays preserved viewport |
| Multiple clients | Interactive suite | Second actual attached PTY sees preserved content; same size |
| Outer terminal resize | Interactive suite | Native resize signal to correctly repositioned sidebar/status and retained shell viewport; exact pane dimensions verified afterward |
| History scroll | Accepted prior 30 trials / 600 events | Exact contiguous numbered viewport transition |
| History search/top/bottom | Interactive suite | Prepared search-editor query commit to off-screen matched record, then first/last retained records |
| Copy/yank and selection | Full-history seed gate plus timed line-yank receipt | Exact captured text; clipboard receipt is a different endpoint from display; Clipboard receipt explicitly labeled separately from render latency |
| Character/word/line/find motions, visual/block selection | Cursor-left/right, big-word-forward and prepared character-find commit implemented; other motions pending | Cursor/selection-cell gates needed; distinct motion semantics must be disclosed |
| Search repeat, pane-local jump list, character hints | Search repeats implemented; jump list/hints pending | Next/previous search match implemented; no identical tmux mux jump-list/hint UI |
| Sustained/background output | Interactive idle/busy profiles + accepted finite throughput | Equal 50 Hz in-place ANSI producer in isolated background window; finite active-pane output already measured |
| Working directory/session root, rename | Seeded cwd / existing clean fidelity; window/session rename editor opening and commit implemented | Session-root UI implemented in mux-only suite; policy differs from tmux working directories |
| Bell/activity navigation | Paired suite implemented; pending validation | Must generate actual bell in background pane and confirm target selection |
| Session tree preview/expand/collapse | Mux-only suite implemented; pending validation | tmux choose-tree exists but different preview/rendering semantics |
| Theme picker, palette update | Mux-only suite implemented; pending validation | Can report mux-only UI latency; no equivalent tmux picker |
| Manual clean save/restart/restore | Accepted 30 trials, preserved separately | Full history/layout/cwd/sampled style/fresh shell gates |
| Default-period crash recovery | Explicitly unmeasured, long-run limit | Never relabel clean results or shorten continuum timer |
| List/query commands | Setup and after-operation correctness evidence | Read-only metadata, not a visible interactive operation |
| SSH auto attach, arbitrary remote transport | Out of same-machine comparison | Network/SSH orchestration rather than terminal rendering |
| tmux layout presets, buffers, floating panes, links | Unsupported in pinned mux / incomparable | Do not silently substitute other operations |

Expanded interactive scale matrix: 1 window × 1 pane, 3 × 2, 6 × 4, each plus
one identical dedicated background window, and one temporary scratch window for
mutation. Both idle and busy profiles use the same initial layout and 1,000
numbered records per seeded pane. Every seeded full history is captured and
checked outside timing. The second session is created during each action sequence.
Twenty fresh isolated launches per variant/profile/scale plus one excluded warm-up
are randomized within each block on one runner. Timed operations use real keys
written to attached PTYs; no CLI acknowledgement can terminate their latency.
Raw frames, input timestamps, commands, process-tree resource evidence and failures
are retained. CPU ticks miss exited helpers and are a lower bound. Any unequal paired fixture or failed trial
blocks the complete profile/scale comparison; no surviving subset is reported.

Status: broad harness implemented and entering real pinned-Nix correctness CI;
these new actions have no accepted performance values until all gates and an
independent paired fixture audit pass. Prior measured micro/clean data remain valid.

Completed groups are independently publishable only after all three variants pass
their diagnostic preflight and every measured trial plus excluded warm-up passes
the independent fixture/input/resource audit. `--windows 1 --load idle` (or the
other declared scales/profiles) limits a run to a named group; it does not select
successful trials. Each group resets the declared shuffle seed. CI runs all six
groups sequentially on one hosted VM, uploading each complete audited group before
starting the next. This makes partial completion explicit and preserves evidence
if a later larger layout fails.

The follow-up paired suite additionally prepares exactly one bell in a background
shell, waits for its actual pending state, then times native alert navigation to
the tagged shell. The bell uses mux's supported steady visual policy and tmux's
native pending-alert state; cross-session alert-order policy is not compared.
Prepared character-find and forward-search commit, next/previous search matches
use exact cursor-record/column gates. The three-second profile CPU window is
separate from short action intervals and remains a process-tree tick lower bound.

`mux_ui.py` is a separate mux-only suite with 20 trials plus one warm-up: two
1,000-record single-pane windows at 100×40 content cells, folded/expanded tree,
live second-window preview and selection, theme picker opening/browsing/cancel,
actual isolated palette application, and session-root update from a real `cd`.
Theme preview correctness includes exact rendered cell background values, not
text alone. Its independent audit verifies inputs, timestamps and complete trials.
It follows the paired groups on the same runner and has no tmux ratio or ranking.
These new cases are implemented but unmeasured until pinned-Nix validation passes.
