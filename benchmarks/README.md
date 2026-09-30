# Benchmark draft: execution blocked, no performance results

This is an **unvalidated microbenchmark harness**, not a completed comparison.
Do not cite performance numbers until correctness gates pass in the pinned Nix
environment. Recovery benchmarks described below remain to be implemented.

## Reproducible environment

The benchmark devShell uses the existing `flake.lock` nixpkgs revision
`b7c2ada94fe99c15b0dbcf4d11fd7850b957a436`. It shares `packages.mux`'s
`pkgs.rustPlatform` toolchain; there is no rust-overlay or host Rust fallback.
Evaluation of that revision on x86_64-linux produced:

| Dependency | Nix-selected version / source revision |
| --- | --- |
| Rust / Cargo | 1.97.1 / 1.97.1 |
| tmux | 3.7b |
| Bash | 5.3p15 |
| Python | 3.14.6 |
| tmux-resurrect | unstable-2022-05-01, `ca6468e2deef11efadfe3a62832ae67742505432` |
| tmux-continuum | unstable-2022-01-25, `fc2f31d79537a5b349f55b74c8ca69abaac1ddbb` |

The plugin revisions and source hashes are part of the locked nixpkgs package
expressions. These are Nix-evaluated versions, **not versions successfully run**
on this machine. Python's pyte emulator and all other benchmark dependencies are
also provided by that revision.

Run directly on a quiet Linux machine with working Nix:

```sh
nix develop .#benchmark --command python3 benchmarks/run.py \
  --output /tmp/mux-benchmark-run-1 --trials 30
```

Output must be a new directory. Do not run tests/builds concurrently. The runner
builds mux once with `cargo build --locked --release` inside the Nix shell and
then runs each variant sequentially. It rejects changes to runtime sources or
Cargo inputs relative to mux commit
`9c74b7029e6112175d0914c0efc12d87317efccf`. Benchmark documentation and harness
changes do not change that runtime pin. If the test/fix task produces a new
runtime commit, explicitly update the pin and rerun all three variants.

## Implemented measurement path (not yet validated)

- Three independent variants: mux, tmux baseline, and tmux with resurrect and
  continuum. Explicit plugin `run-shell` commands come from Nix package paths;
  option checks verify resurrect's save script and continuum's status scheduler.
- One pane, one fresh isolated server per trial, one identical Bash startup
  wrapper without user startup files. PTYs are real and attached throughout.
  Pane content is checked as exactly 100 columns by 40 rows; mux's sidebar and
  tmux's status row get extra outer-terminal space. No process-global HOME
  override or user config modification is required.
- Equal 20,000-row history capacity; 10,000 identical numbered ASCII output rows
  of 80 visible columns and 82 bytes including CRLF. No line wraps or history
  truncation. No scroll workload runs while output is still being generated.
- Startup ends at the decoded fresh-shell prompt, not daemon creation. Output
  timing ends at a decoded end marker with the expected contiguous final rows.
  It includes shell command dispatch and workload interpreter startup.
- Both variants bind PageUp to half-page-up. After a mode-entry warm-up, a
  sample starts immediately before writing that key to the attached PTY and
  ends when pyte decodes exactly the previous 40 rows minus 20 row numbers.
  CLI acknowledgements and raw substring appearances cannot end a sample.
- This measures client input to **decoded terminal-cell viewport**, not physical
  display, GPU rendering, pixels, or human-perceived latency. PTY read batching,
  Python scheduling and emulator work are included; the tool does not estimate
  or subtract those costs. The 50-ms select timeout returns immediately on data.
- PSS and RSS are summed from `/proc/*/smaps_rollup` for the isolated daemon,
  client and their live descendants. RSS double-counts shared mappings; PSS does
  not. Memory is sampled after output, not peak memory. Python harness is
  excluded. CPU uses process-tree user/system ticks; exited workload/plugin
  helpers are missed, so **output CPU is a lower bound**, not complete CPU cost.
  Idle CPU uses a three-second window. Improve accounting before making total
  CPU-efficiency claims.
- Storage is logical file bytes and allocated blocks in each isolated state
  directory, after output and before copy mode. tmux baseline retains no
  restart state. Continuum keeps its default 15-minute interval: this short run
  need not trigger a periodic save. This storage result therefore does **not**
  compare equivalent durable snapshots.
- One retained warm-up plus 30 trials per variant, with deterministic shuffled
  variant order (seed 20260930), 50 scroll samples per trial. Nearest-rank p95
  and median are emitted. Scroll aggregates pool 1,500 samples; they are not
  1,500 independent process launches. All measured trials must pass gates;
  failures remain in raw samples and suppress that variant's aggregates.

Each trial retains `sample.json`, `commands.json`, and binary `client.ansi`.
The run retains exact versions/paths, source pin, harness revision, lockfile,
CPU/memory/kernel/load/cgroup information, build log and summary. Inspect these
before transferring a run into `benchmarks/results/`. No generated result is
silently accepted into documentation.

## Required recovery work before completing the comparison

The runner currently does **not** measure save/restart/restore or recovery
fidelity. Add a separate harness with these gates and do not substitute a
successful reattach for restored terminal state:

1. Add equal multi-pane layouts, session/window names, selected window/pane,
   independent working directories, ANSI colors, Unicode/wraps, and uniquely
   numbered history with a full content digest. Verify all expected state after
   restart. Record restarted fresh shells separately from foreground-process
   recovery; neither application memory nor running tasks can be reconstructed.
2. **Clean save:** time resurrect's configured save script to completion and
   verify the snapshot is complete. For mux, time clean `kill-server`, including
   its final journal flush/sync; mux has no comparable explicit save command.
   Report this operation difference instead of pretending the timers are equal.
   Separately time daemon restart, restore, attach and first correct viewport.
3. **Process crash:** let continuum's real status-driven scheduler create a
   completed save, verify the snapshot, emit numbered output since that save,
   then SIGKILL only the isolated server PID. Test crashes near 0%, 25%, 50%,
   75% and 99% of its 15-minute interval; retain observed snapshot age at crash.
   mux receives the identical output/sleep schedule. Do not call a manual save
   immediately before a crash. Thirty trials per age bucket are expensive:
   budget the run explicitly rather than shortening continuum's period silently.
4. After crashing, terminate residual old pane processes before restarting,
   preserving durable state. Check the restored history prefix/digest and last
   numbered row; report lost rows/bytes, snapshot age and broken layouts rather
   than aggregating failures away. Baseline tmux has no restart persistence;
   report unrecovered state rather than zero-time successful restoration.
5. Process SIGKILL is **not machine/power failure**: the OS page cache survives.
   Assess mux's journal sync window separately from continuum's snapshot age;
   do not claim disk-durability guarantees from a process-crash test.
6. Use cgroup CPU accounting or equivalent for complete descendant CPU cost,
   sample peak memory if claimed, run full history fidelity and multiple pane
   counts, and add filesystem/mount/cgroup limits and background-load evidence
   to the final environment manifest. Prefer per-trial summaries and confidence
   intervals for latency comparisons, preserving all individual samples.

## Recorded blocker on 2026-09-30

See `results/bootstrap/` for commands, evaluated versions and error evidence.
The selected cloud environment had no Nix, tmux or Rust installation. Nix
release/cache endpoints returned proxy HTTP 403. A GitHub-downloaded Nix
portable bootstrap could evaluate packages with its static executable, but
Bubblewrap failed to set up its UID map, PRoot reported unavailable ptrace and
an alternate-store build failed to create a required `/nix/store` source.
No benchmark variant ran and no performance samples exist. A working native
Nix store with reachable substitutes/build sources on this same machine, or a
new equivalent cloud machine with Nix preinstalled, is needed to continue.
