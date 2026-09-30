# Hosted-CI paired benchmark results

[Run 36744338567](https://github.com/any-0/mux/actions/runs/36744338567)
measured all three variants sequentially on one hosted Ubuntu VM using the
project's pinned Nix development toolchain and mux main commit `d6dd228`.
All 30 paired trials per variant passed correctness gates and an independent
raw-data audit. Values below are **median / p95**.

| Variant | Scroll viewport latency (ms) | PSS (MiB) | RSS (MiB) |
| --- | ---: | ---: | ---: |
| mux | 2.388 / 2.652 | 8.867 / 8.900 | 14.693 / 14.727 |
| tmux | 2.300 / 2.834 | 13.235 / 13.239 | 19.617 / 19.621 |
| tmux + resurrect + continuum | 2.433 / 3.507 | 13.259 / 13.298 | 19.641 / 19.680 |

Each trial used the same Bash shell, 100×40 pane, 20,000-row history capacity
and 10,000 numbered output rows. Scroll timings cover 600 actual attached-PTY
PageUp events per variant, ending at the correct decoded viewport; they include
harness scheduling/emulation, not physical display latency. RAM sums daemon,
client and live descendants after output. One warm-up per variant is excluded.
Hosted VM caches and shared physical compute limit generalization to other machines.

Thirty clean recovery trials per variant also passed: mux and the plugin stack
preserved all 600 tagged rows, sampled formatting, layout, cwd and selection,
and confirmed fresh live shells. Median / p95 restart-to-live times were
**253.843 / 258.021 ms** for mux and **826.810 / 834.535 ms** for the stack.
Baseline tmux lost all 600 rows as expected. These are clean-save results;
**default-period crash recovery remains unmeasured**.

[Raw hosted dataset](results/hosted-ci/36744338567/raw.zip) includes
startup, output, CPU, storage and save timings with their limits. Reproduce with:

```sh
./scripts/benchmark-nix python3 benchmarks/run.py \
  --output /tmp/mux-benchmark-run-1 --trials 30 --rows 10000 \
  --scroll-samples 20 --idle-seconds 3
./scripts/benchmark-nix python3 benchmarks/recovery.py \
  --output /tmp/mux-recovery-clean-1 --trials 30 --mode clean
```

The selected cloud machine's dependency downloads remain policy-blocked;
these results are explicitly hosted CI, not measurements of Julian's hardware.

## Detailed accepted results (2026-09-30)

The runner reported AMD EPYC 7763, four logical CPUs, 16,373,452 KiB total RAM,
kernel `6.17.0-1022-azure`, image `ubuntu24 / 20260927.320.1` and ext4 storage.
This records a VM allocation, not exclusive physical hardware. Runtime was
`d6dd228054231e77772bd17a412d8f0d07871835`; executed harness checkout was GitHub's
PR merge `4a7223d4c16628d17d00f7294edb7570bb32103b`, corresponding to branch
`1c1f9417d742d4b0e67e8fe4ffde8941c28f779c`. The three variants ran serially in
seeded shuffled order on this one runner, after compilation and correctness checks.

All entries below are median / nearest-rank p95, with 30 samples unless noted.
Higher throughput percentiles represent higher rates, not worst-case latency.

| Metric | mux | tmux | tmux + persistence |
| --- | ---: | ---: | ---: |
| Startup to decoded prompt (ms) | 37.724 / 39.929 | 17.895 / 19.312 | 188.943 / 203.487 |
| Output to decoded end marker (ms) | 113.108 / 118.186 | 142.301 / 159.338 | 145.211 / 162.216 |
| Output throughput (MiB/s) | 6.914 / 7.251 | 5.496 / 5.665 | 5.385 / 5.595 |
| Output CPU lower bound (s) | 0.100 / 0.100 | 0.050 / 0.070 | 0.060 / 0.070 |
| Idle CPU (% of one CPU, 3 s window) | 0.992 / 1.323 | 0.000 / 0.331 | 0.330 / 0.661 |
| State logical bytes | 839243.500 / 843921.000 | 0.000 / 0.000 | 0.000 / 0.000 |
| State allocated bytes | 851968.000 / 856064.000 | 0.000 / 0.000 | 0.000 / 0.000 |

Output is 820,019 bytes of low-entropy ASCII including the end marker. These
are fresh-process trials with warm filesystem caches, not cold boot or maximum
sustained output tests. CPU tick accounting misses exited helpers; it is a lower
bound. Storage was sampled before copy mode: mux had a continuous journal,
whereas continuum's 15-minute scheduler had not saved any files. The zero plugin
storage observation is **not** equal durable-snapshot storage or a storage advantage.
PSS is post-output resident proportional memory, not peak memory or disk cache.

Pooled scroll events are correlated within each process. Independently computed
per-trial median latency and within-block differences are:

| Variant | Trial-median latency, median / p95 (ms) | Paired difference from tmux, median / p95 (ms) | Paired PSS difference, median / p95 (KiB) |
| --- | ---: | ---: | ---: |
| mux | 2.382 / 2.578 | 0.085 / 0.314 | -4473.000 / -4435.000 |
| tmux | 2.297 / 2.806 | 0.000 / 0.000 | 0.000 / 0.000 |
| tmux-persistence | 2.420 / 2.809 | 0.088 / 0.558 | 26.000 / 68.000 |

These paired differences describe this run; no confidence interval or universal
speed ranking is claimed. mux's median scroll latency was slightly higher than
tmux's here, while its measured post-output PSS was lower.

### Clean recovery measurements

Each variant has 30 accepted trials plus one retained excluded warm-up. Both
persistence variants lost zero of the 600 tagged rows / 37,200 tagged UTF-8 bytes,
and passed full history, wrap, metadata, sampled ANSI-style and fresh-shell gates.
Baseline tmux lost all 600 rows / 37,200 bytes in every trial; restore is unsupported.

| Operation (ms, median / p95) | mux | tmux + persistence |
| --- | ---: | ---: |
| Save + shutdown (mux); explicit save only (stack) | 4.109 / 23.220 | 262.158 / 263.491 |
| Stop, including attached client teardown | 4.109 / 23.220 | 7.095 / 7.313 |
| Restart + attach + live shell nonce | 253.843 / 258.021 | 826.810 / 834.535 |
| Explicit restore script | included in restart | 368.734 / 370.126 |
| Subsequent fidelity verification | 4040.713 / 4050.677 | 3403.982 / 3407.870 |

The save columns describe different operations: mux flushes its journal during
shutdown; the stack explicitly saves before a separate stop. Restart-to-live
includes startup, restoration and fresh-shell confirmation; verification happens
afterward and includes harness waits. Clean recovery is not process-crash or
power-loss recovery. The real default-period crash suite has not executed.

### Retained evidence and independent audit

[Manifest](results/hosted-ci/36744338567/manifest.json),
[micro summary](results/hosted-ci/36744338567/micro-summary.json),
[clean summary](results/hosted-ci/36744338567/clean-summary.json), and
[audit](results/hosted-ci/36744338567/audit.json) accompany the
[complete raw ZIP](results/hosted-ci/36744338567/raw.zip), retained in git beyond
GitHub artifact expiration. ZIP SHA-256:
`4f31c3751d35df6a8b1a87657b244c484370ca210aadaaf9c7c7e636e4193683`.
It contains 93 micro and 93 clean samples, including three warm-ups per suite,
all command/input logs, attached client ANSI streams, full captured histories,
recovery archives, environments, lockfiles and build logs. Socket filesystem
entries are not portable ZIP content and were skipped by the artifact uploader;
all sample evidence required by the audit is present. CI cleanup reported no
orphan-process termination entries.

```sh
unzip benchmarks/results/hosted-ci/36744338567/raw.zip -d /tmp/hosted-mux-data
./scripts/benchmark-nix python3 benchmarks/audit_hosted.py /tmp/hosted-mux-data
```

The audit independently rechecks all 10,000 numbered records in each retained
history, viewport transitions, PageUp input timestamps and elapsed time math,
process-memory sums, shuffled serial trial order, warm-up exclusion and every
published aggregate. It also rejects clean fidelity failures. Offline auditing
needs only Python's standard library; no host-built backend produced these data.
The run's preflight passed 44 Nix tests and six actual PTY integration gates;
the separate project Tests and benchmark correctness workflows also passed.

## Requested measurement coverage and remaining crash budget

| Requested measurement | Accepted evidence | Remaining limit |
| --- | --- | --- |
| Startup | 30 fresh starts per variant; decoded prompt endpoint | Warm caches, not cold boot |
| Output throughput | 30 identical 820,019-byte workloads per variant; decoded end marker | Finite ASCII workload, not sustained saturation |
| Scroll and RAM | 600 events and 30 process-tree PSS/RSS samples per variant | Decoded cells and post-output memory |
| CPU and storage | 30 samples per variant | CPU lower bound; journal and unsaved periodic state are not equivalent snapshots |
| Clean save/shutdown | 30 trials each; mux combined flush/shutdown and stack explicit save plus separate stop | Different operations are labeled separately |
| Clean restart/restore | 30 trials each; restart-to-live for both, explicit stack restore script separately | mux restoration is included in restart, not separately instrumented |
| Clean recovery fidelity | Complete tagged histories, wraps, sampled style, metadata and fresh shells passed | Finite fixture; does not restore old shell process memory |
| Default-period process-crash recovery | No accepted samples | Same-runner duration exceeds hosted job limit |

The crash harness retains continuum's real 900-second scheduler and never
manually saves in its crash path. mux and baseline tmux wait one full interval; the plugin variant waits for
an actual scheduler-produced snapshot, whose first-save timing may differ. Each
then crashes at age fractions 0, .25, .5, .75 and .99 while recording equal
post-snapshot output. It rejects a second save before the requested crash age.
No clean result above has been relabeled as crash evidence.

The configured 30 trials plus a warm-up, three variants and five ages require
`31 × 3 × 900 × (5 + 0 + .25 + .5 + .75 + .99)` = **626,913 seconds / 174.14
hours / 7.26 days** with a 900-second priming allowance for each variant, before
startup, fidelity checks and restoration. This is a planned budget; the plugin's
first scheduled save can occur sooner. Even giving that variant zero priming
time, the minimum supported 20 trials plus warm-up at only age zero requires
`21 × 2 × 900` = **10.5 hours** for mux and baseline alone, exceeding GitHub's
six-hour hosted-job limit.
These are timer-budget calculations, not measured execution times.

One three-variant triplet budgets 45 minutes at age zero or 67.5 minutes at
age .5, plus overhead; an earlier first scheduled snapshot can reduce that
budget to 30 or 52.5 minutes, respectively. That can provide a diagnostic but cannot
supply repeated median/p95 evidence. Splitting repeated trials across fresh CI
jobs changes the runner; checkpointing state between jobs also changes the
crash experiment. Neither closes the controlled single-runner gap. No additional
long diagnostic job was started merely to add a smoke sample.

The practical supported next step is a quiet Linux machine or long-lived runner
with the same pinned Nix closure and uninterrupted time, then:

```sh
./scripts/benchmark-nix python3 benchmarks/recovery.py \
  --output /tmp/mux-recovery-crash-1 --mode crash --trials 30
```

The selected cloud still needs policy-supported Nix dependency access or a
pre-populated locked closure. That environment setup and long runtime remain
required; changing continuum's interval, moving environments silently or reporting
clean timings as crash timings would not resolve the blocker. No new runner
provisioning or paid compute was initiated.

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
expressions. These versions actually executed on the hosted runner; the selected cloud
machine still cannot realize the dependency closure. Python's pyte emulator and all other benchmark dependencies are
also provided by that revision.

Run directly on a quiet Linux machine with working Nix:

```sh
./scripts/benchmark-nix python3 benchmarks/run.py \
  --output /tmp/mux-benchmark-run-1 --trials 30 --rows 10000 \
  --scroll-samples 20 --idle-seconds 3
```

Output must be a new directory. Do not run tests/builds concurrently. The wrapper creates a detached temporary checkout of the exact runtime pin
without changing either PR branch; CI supplies its own separate pinned checkout.
The runner builds mux once with `cargo build --locked --release` inside the Nix shell and
then runs each variant sequentially. It rejects changes to runtime sources or
Cargo inputs relative to mux commit
`d6dd228054231e77772bd17a412d8f0d07871835`. The source pin is tracked in `benchmarks/mux-revision`
and selects upstream main after both original PRs merged. Benchmark documentation
and harness changes do not change that runtime pin. If the test/fix task produces a new
runtime commit, explicitly update the pin and rerun all three variants.

## Implemented measurement path

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
  variant order (seed 20260930), 20 scroll samples per trial in this measured run (CLI default: 50). Nearest-rank p95
  and median are emitted. Scroll aggregates pool 600 samples; they are not
  600 independent process launches. All measured trials must pass gates;
  failures remain in raw samples and suppress that variant's aggregates.

Each trial retains `sample.json`, `commands.json`, and binary `client.ansi`.
The run retains exact versions/paths, source pin, harness revision, lockfile,
CPU/memory/kernel/load/cgroup information, build log and summary. Inspect these
before transferring a run into `benchmarks/results/`. No generated result is
silently accepted into documentation.

## Implemented recovery run path

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

On this selected cloud machine, **43 correctness tests passed** using host
Python 3.12.14 and the same pyte 0.8.2 source version selected by nixpkgs.
Eight tests exercise real attached PTYs, including terminal identity/status queries,
scroll-region/cursor preservation, wide-cell overwrite, and: dimensions reach the child process,
erased raw output cannot satisfy a rendered-cell gate, a live-shell nonce
cannot pass from command echo, and the actual seed command renders its
UTF-8/red fixture correctly. Tests also cover the save/crash controller, real-save
hook versus incomplete archive, corruption/truncation/duplicates, Unicode/wraps,
metadata and color losses, recycled-PID signal protection, p95, baseline
unsupported recovery, and failed-trial suppression. The stdlib-only run passes
35 tests and skips those eight pyte tests. A ninth integration check requires
the actual Nix-generated shell and verifies that resurrect-style `-c` commands
execute. The host follow-up runs 44 tests: 43 pass and that Nix-only test skips.

These are logic/PTY validation results, **not benchmark samples** or a claim
that full mux/tmux performance/recovery trials passed. Real Nix integration
evidence is recorded separately below. Controller fixtures use synthetic
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
6. The **corrected development benchmark devShell realization** command using that supported
   configuration reached dependency building and failed:

```sh
XDG_CACHE_HOME=/tmp/mux-nix-cache /tmp/mux-nix-portable/.nix-portable/bin/nix \
  --extra-experimental-features 'nix-command flakes' \
  --option sandbox false --option build-users-group '' \
  --option connect-timeout 3 --option download-attempts 1 \
  --store 'local?store=/tmp/mux-native-nix/store&state=/tmp/mux-native-nix/state&log=/tmp/mux-native-nix/log' \
  develop ./.nix#benchmark --no-write-lock-file --command true
```

`development-nix-develop.log` records this corrected `.nix` invocation and identifies
the same failed Bash 5.3 source URL (needed for Bash 5.3p9) and the
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

At this historical local-cloud checkpoint, performance samples were zero.
The later hosted paired run above supplies measured results in a separate context.

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
pins hardware. Hosted-CI data belongs in a separate
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
run remains pending. The micro and clean hosted run above is accepted; default-period crash measurements remain pending.


## CI correctness evidence

The initial supported Nix run [36735486799](https://github.com/any-0/mux/actions/runs/36735486799)
passed 39 tests and built runtime `d328bd3cf503e22855818a20353964c4879bc67b`,
but real interaction exposed a pyte private cursor-query error and a cwd
sampling barrier omission. The second run
[36736567725](https://github.com/any-0/mux/actions/runs/36736567725) passed 40
tests, mux micro/clean recovery and persistence-tmux micro gates. Other gates
failed on an intermediate viewport and an orphan-wide-cell pyte display error.
Both complete uploaded ZIPs are retained in `results/ci-validation/`, including
commands, raw PTY bytes, environment and rejected diagnostic samples. These
are deliberately **not performance results**. Fixes reply to actual PTY queries,
wait for the complete final viewport and render an orphan empty wide-cell stub
as a blank. The latter retains text/style fidelity gates; it does not replace
missing tagged characters. A later run explicitly pins PR #2's new runtime
`af93924bad3553806f3d5bb3d732714dba91f02e` rather than pooling these revisions.

The renderer also decodes tmux's CSI S/T scroll optimizations with cursor and
margin preservation, verified by an attached-PTY regression. Replaying the raw
failed tmux streams then yields the expected contiguous final workload rows.
Pyte collapses secondary device-identity queries into primary ones; the harness
suppresses its incorrect identity reply and lets tmux use its terminal fallback.
Cursor-status queries receive actual PTY responses. This terminal model has no
identity extensions and is shared by all variants; startup includes the
backend's negotiation behavior. These are decoded-viewport measurements, not
physical display latency.

[Run 36738815708](https://github.com/any-0/mux/actions/runs/36738815708)
passed 43 pinned-Nix tests, all three real micro gates, clean mux recovery and
baseline tmux's expected unavailable recovery. The persistence variant's clean
fidelity gate correctly rejected empty replayed histories despite a valid
snapshot and restored metadata. The benchmark shell wrapper had ignored `-c`,
preventing resurrect's `cat` replay command from executing. The corrected wrapper
preserves arguments, with a Nix-only command-execution regression. Full rejected
diagnostics are retained as `36738815708-failed-fidelity-smoke.zip`. Those raw
timings remain rejected diagnostic values, not an accepted result set.

[Run 36739541991](https://github.com/any-0/mux/actions/runs/36739541991)
passed all **44 tests and six real integration gates** using Rust/Cargo 1.93.0
and runtime `af93924bad3553806f3d5bb3d732714dba91f02e`: all three micro variants,
clean mux, baseline tmux's expected persistence loss, and resurrect/continuum
clean save/restart/restore with complete history, sampled ANSI formatting,
layout, cwd, selection and live fresh shells. This is one smoke trial per
variant, with `performance_comparison: false`, not a median/p95 sample set.
Default-period crash integration and full repeated measurements remain pending.
PR #2's final tested handoff advanced to
`6da51cf7a776a81b8be6d8dbf16ea498e7f8385c`; the historical runtime pin selected
that head. Its separate validation is recorded below.


Historical candidate validation: [run 36740002774](https://github.com/any-0/mux/actions/runs/36740002774)
passed **44 Nix tests and all six serial real integration gates** at harness
`9c7f3984792f1d38e51617b9e2a634435c746b98`, runtime
`6da51cf7a776a81b8be6d8dbf16ea498e7f8385c` from the PR #2 branch candidate
`fix/terminal-session-regressions`, **not upstream main**. Full raw evidence is
`results/ci-validation/36740002774-passing-candidate-smoke.zip`, with ZIP
checksums in `checksums.json`. The plugins were actually loaded; the clean
snapshot archive, restored complete tagged histories, sampled ANSI styling,
metadata and fresh shells passed. Six correctness smoke trials contain actual
diagnostic timings, but that smoke dataset contains no accepted performance trials or
median/p95 comparison. The separate repeated dataset above supplies performance results.

Cloud access was rechecked at **2026-09-30 15:55 UTC**. Both the standard Nix
cache and GNU Bash source URLs still return proxy `CONNECT ... 403`; see
`cache-http-recheck.log` and `gnu-source-http-recheck.log`. The corrected native
`.nix#benchmark` invocation again exited 1 on the Bash source dependency
(`development-nix-develop-recheck.log`). No allowlist change was assumed. The
owner/platform action remains a policy-supported native `/nix/store` and locked
dependency closure/access on this selected cloud machine. CI correctness is a
supported route; same-runner micro/clean performance pilot data would need a
separate hosted-CI table, randomized paired trials and recorded runner/load.
It cannot stand in for Julian's selected cloud hardware. No network-policy
bypass, environment move or merge was performed.


Historical merge update: the benchmark branch incorporated upstream main merge
`d19f0dc8ab4d7f4a5d2accb3bbbdac7acd2b33cb` and then pinned that merged revision.
Its runtime and Cargo inputs are identical to the tested PR #2 candidate
`6da51cf7a776a81b8be6d8dbf16ea498e7f8385c`; previous ZIP artifacts remain
labeled with the exact candidate revisions they actually validated. The final
branch runs its own pinned-Nix project checks and benchmark correctness smoke.
No new performance trials or performance statistics are implied by this update.

Graceful stop dispatches shutdown, closes the attached client and then verifies
the isolated server exits. `stop_ms` includes client teardown for every variant.
The server-exit correctness gate still rejects a surviving daemon. Failed merged-main
validation 36742370783 is preserved as a shutdown diagnostic, not performance data.
