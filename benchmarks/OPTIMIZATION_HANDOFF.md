# Baseline benchmark handoff for runtime optimization

This benchmark PR changes harness/docs only. Baseline mux remains
`d6dd228054231e77772bd17a412d8f0d07871835`; do not silently substitute PR #4 or
an optimization candidate into an existing result directory.

## Reproduce one declared paired group

```sh
./scripts/benchmark-nix python3 benchmarks/interactive.py \
  --output /tmp/mux-paired-w3-idle --trials 20 --windows 3 --load idle
./scripts/benchmark-nix python3 benchmarks/audit_interactive.py /tmp/mux-paired-w3-idle
```

The wrapper creates a clean pinned runtime worktree unless
`BENCH_MUX_SOURCE_DIR` supplies a checkout whose HEAD equals
`benchmarks/mux-revision`. It uses the project's locked `.nix#benchmark` shell,
Rust/Cargo 1.93.0, tmux 3.6a and actually loaded pinned persistence plugins.
An optimization experiment needs its own explicit candidate pin/result label
and candidate-aware audit; the baseline auditor intentionally rejects another
source revision. Commit the optimization candidate before measuring; never
label a dirty checkout as the baseline. Preserve the baseline assertion and accepted baseline files.
Build both candidates in the same Nix environment, and run them sequentially on
one otherwise idle runner; do not overlap measurements with compiler/test load.

## Raw evidence and profiling limits

- `results/hosted-ci/36744338567/raw.zip`: accepted 30-trial scrolling,
  finite-output, fresh-start and clean save/restart/recovery baseline.
- `results/hosted-ci/36762906986/`: accepted four core groups, 45 endpoints;
  both original 6×4 groups rejected for logical pane-slot mismatch.
- `results/hosted-ci/36769182665/`: three accepted 53-endpoint groups,
  three-second CPU observations and state storage. Whole 3×2 busy group rejected
  after one untimed bell-preparation failure. Manifest distinguishes accepted,
  blocked and unmeasured scopes. Failure ZIP and traceback are preserved.
- `results/hosted-ci/36773777142/`: diagnostic-only evidence, zero performance
  trials. Fixed tree-choice preflight passed, but the old broad sidebar-change
  check allowed bell navigation before a pending bell existed.

- `results/hosted-ci/36774371342/`: complete replacement sweep, 360 paired trials at
  all six scale/load groups (53 endpoints each), plus 20 mux-only UI trials.
  Use this separate same-runner dataset for the final broad baseline; do not pool
  it with the earlier partial or historical runs.

Each accepted group ZIP contains `environment.json`, commands, complete seeded
histories, process-tree resident memory, timestamped actual PTY inputs,
`client.ansi`, and every trial's `sample.json`. CPU profile observations in these
files are **tick counters, not stack-sampling profiles or flamegraphs**. The
Nix benchmark shell currently does not provide `perf`/a profiler; add a pinned
profiling dependency to the optimization task's environment if needed. Current
CPU counters miss exited helpers; RSS double-counts shared pages, PSS does not,
and neither is peak memory. CPU observations are before interactive mutations,
not CPU consumed by the complete action sequence.

At the accepted 3×2 idle fixture in run 36769182665, observed median CPU was
8.444% of one CPU for mux, 0% for tmux and 0.331% for the stack. That is a
repeatable profiling lead, not proof of a particular hot function. The separate
complete replacement sweep has its own CPU distributions in the README; do not
interpret cross-runner differences as an optimization effect. Continuous
journaling and default native policies must remain disclosed when interpreting
CPU/storage against an unsaved periodic stack.

## Correctness and timing pitfalls

- Match **content area**, 100×40 cells and exact pane geometry/history. Outer
  PTYs differ to compensate native chrome: mux 105×40, tmux 100×41.
  Busy workload is one equal 50 Hz, five-row ANSI producer in another window.
- Four-pane native numbering differs: mux native `[1,4,2,3]` corresponds to
  top-left, top-right, bottom-left, bottom-right. Tags use logical slots; compare
  full history digests and prompt positions, not native index alone.
- Attached input and CLI control use different connections. After sending a
  shell command, wait for its **visible receipt** before selecting another
  window; a CLI acknowledgement does not prove attached bytes were consumed.
- A generic sidebar repaint is not a bell. With the pinned default palette and
  `bell_style="steady"`, prepare the exact target label's `9fa8f2` background
  before timing native pending-alert navigation. This wait is untimed.
- Check content by slicing at its actual offset; sidebar cells are not blank.
  A frame containing the intended prompt alone can be a tree preview: verify
  tree disappearance, content placement and active-window metadata.
- Time input write to **decoded correct viewport/cursor/style**, not CLI exit.
  Reject predicates that are already true before input. Metadata checks follow
  the rendered endpoint. Clipboard receipt and actual detach process exit are
  distinct endpoints; clipboard polling can add up to 50 ms.
- Mux Escape first clears a retained search highlight before leaving copy mode;
  tmux's configured `q` cancels it directly. Prepare each native mode correctly.
  Prepared editor commits and native confirmations have separately labeled
  endpoints; do not include them in a claim of cold populated startup.
- Preserve whole-group failures. Never delete a failed trial to obtain 20
  survivors, pool across different hosted VMs, or call diagnostic preflight
  measurements performance results. `continue-on-error` can make the GitHub
  step conclusion show success despite a failed outcome: inspect raw samples.
- Clean recovery remains distinct from process-crash recovery at continuum's
  real 15-minute interval. The default-period crash suite is unmeasured and is
  not a prerequisite for these interactive measurements.
