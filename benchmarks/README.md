# Benchmark draft: execution blocked, no performance results

This is an **unvalidated benchmark harness**, not a completed comparison.
Microbenchmark and recovery code are implemented; no mux/tmux benchmark has
executed. Do not cite performance numbers until the correctness gates pass in
the pinned Nix environment.

## Reproducible environment

The benchmark devShell uses the existing `.nix/flake.lock` nixpkgs revision
`2fc6539b481e1d2569f25f8799236694180c0993`. It extends the `.nix` development flake selected by `.envrc` and PR #2's
successful Nix CI. The separate root packaging flake has a different pin and
is not the benchmark toolchain. No rust-overlay or host Rust fallback is used.
Evaluation of that revision on x86_64-linux produced:

| Dependency | Nix-selected version / source revision |
| --- | --- |
| Rust / Cargo | 1.93.0 / 1.93.0 |
| tmux | 3.6a |
| Bash | 5.3p9 |
| Python | 3.13.12 |
| tmux-resurrect | unstable-2022-05-01, `ca6468e2deef11efadfe3a62832ae67742505432` |
| tmux-continuum | unstable-2022-01-25, `fc2f31d79537a5b349f55b74c8ca69abaac1ddbb` |

The plugin revisions and source hashes are part of the locked nixpkgs package
expressions. These are Nix-evaluated versions, **not versions successfully run**
on this machine. Python's pyte emulator and all other benchmark dependencies are
also provided by that revision.

Run directly on a quiet Linux machine with working Nix:

```sh
./scripts/benchmark-nix python3 benchmarks/run.py \
  --output /tmp/mux-benchmark-run-1 --trials 30
```

Output must be a new directory. Do not run tests/builds concurrently. The wrapper creates a detached temporary checkout of the exact runtime pin
without changing either PR branch; CI supplies its own separate pinned checkout.
The runner builds mux once with `cargo build --locked --release` inside the Nix shell and
then runs each variant sequentially. It rejects changes to runtime sources or
Cargo inputs relative to mux commit
`af93924bad3553806f3d5bb3d732714dba91f02e`. The source pin is tracked in `benchmarks/mux-revision`, currently PR #2
revision `af93924bad3553806f3d5bb3d732714dba91f02e`. Benchmark documentation
and harness changes do not change that runtime pin. If the test/fix task produces a new
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

## Implemented recovery run path (integration validation still blocked)

```sh
# Fastest integration check of the recovery pipeline, still with >=20 trials:
./scripts/benchmark-nix python3 benchmarks/recovery.py \
  --output /tmp/mux-recovery-clean-1 --mode clean --trials 30

# Real default-period process-crash tests. Plan several days of exclusive compute.
./scripts/benchmark-nix python3 benchmarks/recovery.py \
  --output /tmp/mux-recovery-crash-1 --mode crash --trials 30
```

`--mode both` runs both suites. The three variants run sequentially with shuffled
order. Each mode/age/variant retains one warm-up and 30 measured trials. The
crash suite's default timer budget is roughly seven days including warm-ups;
that is a calculation from configured waits, **not an observed run time**.
Do not shorten continuum's interval or move to another machine without
explicitly changing the experiment and its labeling.

### Seed and correctness gates

Each trial seeds session `bench`, windows `primary` and `secondary`, and three
panes: two top/bottom panes of 100x19 and 100x20 cells in window 1, and one 100x40
pane in window 2. Pane dimensions are queried and checked before accepting the
seed. tmux's split size is explicit to match mux's rounding. The selected state
is window 1, pane 2; each pane has an independent working directory.

Every pane gets the same 200 red ANSI-colored tagged UTF-8 records (`α漢` plus
ASCII payload), and an explicit 120-character soft-wrap probe. Full tagged
history is captured by mux's real copy/yank interaction into an atomically
written clipboard file, or tmux's `capture-pane`. The capture checks every
record and a SHA-256 digest of the full tagged corpus. Gaps, duplicates, order
changes, malformed tagged rows, foreign-pane data and Unicode corruption fail
the prefix gate. Soft wraps must reconstruct the complete probe.

Before and after recovery, attached-client pyte cells check an eight-cell
colored run from the first tagged row of each pane. This is a **sampled color
fidelity check**, not exhaustive terminal-attribute equality. Session/window
names, selected window, per-window active panes, pane sizes and working
directories must also match. All restored panes must answer a fresh shell
nonce through the attached PTY. The nonce is split across printf arguments,
so neither an echoed command nor a journal-replayed old prompt can satisfy
this live-shell gate.

### Clean save versus process crash

For **clean save**, resurrect's configured save script runs to completion; a
post-save hook marks completion only after its layout and pane-content archive
have been written. The harness verifies both window records, all three pane
records, gzip/tar integrity, every pane's full tagged history and wrap probe.
An option, file mtime, or successful command acknowledgement alone cannot pass.
Record `save_ms`, then separately time clean shutdown. For mux, `kill-server`
plus observed daemon exit includes final journal flush/sync; its metric is
`save_shutdown_ms`, since mux has no equivalent standalone save command. tmux
baseline has no save operation. Full history and sampled color/layout fidelity
must survive a clean restart.

For **process crash**, the harness never manually saves. It waits for an actual
continuum status-scheduler save at its unchanged default 15-minute interval,
verifies the complete snapshot, and records its completion timestamp and file
manifest. Other variants wait the same default interval from their startup.
Because scheduler completion is asynchronous, actual priming durations are
retained rather than assumed identical.

After that snapshot, the selected pane receives 20 numbered rows per second
at identical scheduled offsets in each variant. Dispatches over 0.5 seconds
late fail the workload gate. Crashes target 0%, 25%, 50%, 75% and 99% of the
900-second period. The 0% bucket emits one 20-row burst before crashing; its
actual age therefore exceeds zero and is reported. A subsequent periodic save
invalidates the age bucket. The .99 workload, including its completion markers,
fits inside the equal 20,000-row history capacity.

SIGKILL targets only the identified isolated server. Old pane descendants are
recorded before termination, checked by PID plus start time, and removed before
restart; PID reuse cannot authorize a signal. Termination gates verify that all
recorded old processes ended. No unrelated tmux or mux session is touched.

Restart launches an attached client. mux automatically replays its journals;
the persistence stack runs its configured resurrect restore script in a fresh
bootstrap server and removes the bootstrap session afterward. Record
`restart_attach_live_ms` through the first fresh shell response and, for tmux,
`restore_script_ms` separately through a marker written only after the restore
script completes; a CLI acknowledgement cannot end that timer. `decoded_prompt_ms` can include a persisted
prompt and is diagnostic only. Full fidelity verification cost is reported as
`verification_ms`, separately from restart/restore latency.

Crash recovery permits a contiguous history prefix, reports exact lost tagged
rows and tagged UTF-8 payload bytes, and rejects corrupted/reordered history.
Layout/wrap/sampled-color gates still apply. Baseline tmux must report recovery
as unsupported and all seeded/generated tagged rows lost; it never receives a
zero-time successful-restore result. Foreground application state is not
reconstructed; these fixtures verify fresh shells and durable terminal state.

`summary.json` groups by mode, age and variant. It excludes warm-ups, uses median
and nearest-rank p95, keeps failure counts and observed loss distributions, and
suppresses **all** timing aggregates in a group if any measured trial fails its
gates. Samples, commands, client ANSI streams, captured history, snapshot content
hashes, durable logical/allocated storage after stop, and environment/build logs remain available for inspection. Do not
report a surviving subset as a passing comparison.

### Validation performed without a Nix benchmark execution

Run the reproducible validation suite when Nix is available:

```sh
./scripts/benchmark-nix python3 -m unittest discover \
  -s benchmarks -p test_benchmarks.py -v
```

On this selected cloud machine, **39 correctness tests passed** using host
Python 3.12.14 and the same pyte 0.8.2 source version selected by nixpkgs.
Four tests exercise real attached PTYs: dimensions reach the child process,
erased raw output cannot satisfy a rendered-cell gate, a live-shell nonce
cannot pass from command echo, and the actual seed command renders its
UTF-8/red fixture correctly. Tests also cover the save/crash controller, real-save
hook versus incomplete archive, corruption/truncation/duplicates, Unicode/wraps,
metadata and color losses, recycled-PID signal protection, p95, baseline
unsupported recovery, and failed-trial suppression. The stdlib-only run passes
35 tests and skips those four pyte tests.

These are logic/PTY validation results, **not benchmark samples** or a claim
that mux/tmux restore integration passed. Controller fixtures use synthetic
state only inside temporary test directories. Their values never enter a
benchmark result. Exact commands, host/dependency versions and test output are
retained in `results/validation/`.

Remaining validation: run both real backends in the required Nix environment,
check repeated recovery under load, and complete the requested measured README
comparison. Total CPU accounting and peak-memory/multiple-workload coverage
remain limited as described above. Process SIGKILL leaves the OS page cache
alive; these tests do not establish machine/power-failure durability or an
upper bound on mux's sync-window loss.

## Exact Nix setup diagnosis on the selected cloud machine

Historical bootstrap attempts used this same cloud machine. The earlier logs
evaluated the root packaging flake (Rust 1.97.1), which was an incorrect choice
for the requested benchmark toolchain. They are preserved as historical setup
evidence, not execution evidence for the corrected `.nix` development pin.
`development-pin-evaluation.json` records the corrected local evaluation. No alternate machine, proxy, mirror,
Docker benchmark, or substitute host toolchain was used. Raw commands and
outputs are under `results/bootstrap/`.

1. No installed `nix`, `cargo`, `rustc`, or `tmux` was found. A GitHub-hosted
   nix-portable v012 artifact supplied a static Nix 2.20.6 executable. Its
   bootstrap executable is **not** the required mux Rust/toolchain environment.
2. These are **network-policy denials**: the cloud HTTP proxy answered
   `HTTP/1.1 403 Forbidden` to CONNECT for
   `https://cache.nixos.org/nix-cache-info` and
   `https://ftpmirror.gnu.org/bash/bash-5.3.tar.gz` (see `cache-http.log` and
   `gnu-source-http.log`). The earlier `releases.nixos.org` download was likewise
   denied, but that download is unnecessary now that a bootstrap exists.
   Nix's libcurl reports the proxy rejection as `Failure when receiving data
   from the peer (56)`. These denials were not bypassed.
3. Portable Bubblewrap failed with
   `bwrap: setting up uid map: Read-only file system`. A direct standard
   namespace probe confirms
   `unshare: cannot open /proc/self/uid_map: Read-only file system`.
   Portable PRoot reports `ptrace(PEEKDATA): Function not implemented` and
   `execve(.../bin/nix): Bad address`. These are separate runtime capabilities;
   there is no evidence that changing a Nix download setting fixes them.
4. A local XDG-cache configuration issue (`creating directory
   '/home/agent/.cache/nix': Read-only file system`) was resolved by
   `XDG_CACHE_HOME=/tmp/mux-nix-cache`. The remote-store probe then reached the
   cache URL and failed on its network denial, not that local directory.
5. The first alternate *chroot* store attempt (`--store /tmp/mux-nix-store`)
   used canonical `/nix/store` paths and failed opening a Bash source there.
   However, Nix's supported native local store configuration **does work**:
   `--store 'local?store=/tmp/mux-native-nix/store&state=/tmp/mux-native-nix/state&log=/tmp/mux-native-nix/log'`
   with `--option sandbox false --option build-users-group ''` successfully
   built a small shell probe. It needs neither ptrace nor user namespaces.
   That probe used host `/bin/sh` solely to test Nix store operation, never to
   benchmark mux. See `native-local-store-probe.log`.
6. The **actual benchmark devShell realization** command using that supported
   configuration reached dependency building and failed:

```sh
XDG_CACHE_HOME=/tmp/mux-nix-cache /tmp/mux-nix-portable/.nix-portable/bin/nix \
  --extra-experimental-features 'nix-command flakes' \
  --option sandbox false --option build-users-group '' \
  --option connect-timeout 3 --option download-attempts 1 \
  --store 'local?store=/tmp/mux-native-nix/store&state=/tmp/mux-native-nix/state&log=/tmp/mux-native-nix/log' \
  develop .#benchmark --no-write-lock-file --command true
```

`native-nix-develop.log` identifies the failed Bash 5.3 source URL above and the
resulting `nix-shell-env.drv` dependency failure. The new devShell expression
itself evaluates successfully; missing dependency access is the current blocker.
A nonstandard logical store also cannot use ordinary `/nix/store` substitutes
without matching store paths, so enabling just the cache is insufficient for
that custom-store bootstrap.

**Required owner/platform action:** provide the locked benchmark devShell
closure in a native `/nix/store` on this selected machine, or provision a native
Nix store and permit its standard dependency fetches under the cloud's network
policy (the observed denied hosts are `cache.nixos.org` and `ftpmirror.gnu.org`).
A pre-populated closure can avoid the blocked network downloads. Enabling
ptrace/user namespaces is not required for the supported native-store path with
sandboxing disabled. This cannot be achieved by a repository-only Nix setting;
there is no authorization to bypass the proxy denial. No move to Neo was made.

**Performance samples remain 0.** No benchmark median/p95 or advantage has been
measured, and the actual-results README requirement remains open.

## Hosted CI feasibility assessment (separate context)

PR #2's [successful Nix CI run](https://github.com/any-0/mux/actions/runs/36733766947)
validated runtime `d328bd3cf503e22855818a20353964c4879bc67b` with the actual
`.nix` Rust/Cargo 1.93.0 development pin. PR #1's `Benchmark harness validation`
workflow follows that supported installer route, checks out exactly that runtime
into a separate directory, and runs the Nix unit/PTY suite and serial real
micro/clean-recovery smoke gates. It uploads raw diagnostics even on failure.
Smoke trial zero and diagnostic timings are **not a performance comparison**;
no median/p95 is generated. Their context is explicitly
`hosted-ci-correctness-validation`, never selected-cloud or Julian's hardware.

A controlled **hosted-CI pilot** of 30 microbenchmark/clean trials per variant
is feasible in one dedicated job after compilation finishes, with no parallel
matrix/services/builds on that runner. Use the same job for all variants, retain
CPU/model, image version, kernel, filesystem, cgroup, runner/run identity and
background-load evidence, randomize variant order and preserve paired raw
samples. Gate all results on correctness. Running variants in separate jobs
would confound variant and runner and does not satisfy same-runner pairing.

A hosted VM does not establish stable hardware, exclusive physical compute,
local disk durability, physical terminal latency or reproducibility on Julian's
machine. Runner image labels and physical hosts can vary; pin software with
Nix and record the actual image/hardware rather than implying the runner label
pins hardware. Hosted-CI pilot data, if later requested/run, belongs in a separate
README table and artifact directory and cannot fulfill the selected-cloud or
user-hardware measurements by relabeling it.

The full default crash suite is not feasible as one hosted runner job:
[GitHub limits hosted jobs to six hours](https://docs.github.com/en/actions/reference/limits).
Its approximately seven-day timer budget exceeds that limit. A single triplet
at one age fits, but accumulating 30 trials by distributing across jobs changes
runners; analyze per-runner triplets and runner variation rather than pooling
those samples as one identical-machine experiment. No shortened continuum
period, synthetic save timestamp or checkpoint handoff makes it equivalent to
the specified real default-period crash suite. The dedicated cloud/local-machine
run remains pending. No hosted-CI performance numbers have been accepted.
