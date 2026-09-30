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

<!-- interactive-results:start -->
### Expanded interactive measurements (hosted CI, 2026-09-30)

[Run 36762906986](https://github.com/any-0/mux/actions/runs/36762906986)
completed the 360-trial sweep. Independent paired fixture/input/resource audits
accept **240 trials**: 20 per variant in each of four groups (1×1 and 3×2,
idle/busy), plus excluded warm-ups. Each trial covers 45 endpoints across roughly
20 operation families. The two 6×4 groups are **rejected** because native pane
numbering put tagged content in different grid slots; their raw samples and audit
failures are retained, and corrected logical-slot fixtures are being rerun.

Runtime: `d6dd228054231e77772bd17a412d8f0d07871835`. Actual executed harness:
`efc5c1551adf0fc24f55e9c15bd444658442efde` (PR merge of branch `ee916e0`).
Project Nix pin `2fc6539b481e1d2569f25f8799236694180c0993`, Rust 1.93.0,
tmux 3.6a and the same pinned Bash/Python/plugins documented below. The single
hosted VM reported AMD EPYC 9V74, four logical CPUs, 16,373,444 KiB RAM, image
`20260920.314.1`. This is a different runner from the earlier scroll/clean run;
do not interpret differences between those datasets as workload-only effects.

Variants ran sequentially in shuffled paired blocks. The layout notation counts
seeded windows × panes per window; each has an additional identical background
window, and mutations use a temporary scratch window. Content area is 100×40,
retained tagged history is 1,000 records per seeded pane, cap 20,000. Native chrome
and bootstrap command history differ. Busy adds the same 50 Hz, five-line in-place
ANSI producer. Full tagged histories and pane dimensions are independently checked.
The two accepted scales use full-width panes; wider grids require the pending fix.

All table entries are **median / nearest-rank p95**, 20 independent process trials
per cell. Interaction latency stops at the decoded correct viewport/cursor/style,
with metadata checked afterward. `populated attach` launches a client against an
existing populated daemon; it is not cold daemon startup or restoration. Native
rename/search editors and deletion confirmations are prepared before timing their
commit. Join-pane is CLI dispatch→visible viewport; yank is atomic clipboard-file
receipt; detach is process exit. These distinct endpoints are labeled separately.
Clipboard receipt uses up to 50 ms controller polling in this dataset; it is an
observed upper bound, not a precise clipboard-completion or rendered-copy latency.
Zoom geometry differs because mux hides its sidebar while tmux retains its status
line. The PTY decoder/harness is part of measured latency; no physical pixels,
exclusive hardware, maximum throughput or universal ranking is claimed.

Initial resident memory includes daemon, attached client, all shells and busy
producer when present; it is not peak memory. RSS and every operation result are
in the complete tables and audits below.

| Seeded layout / load | mux PSS (MiB) | tmux PSS (MiB) | stack PSS (MiB) |
| --- | ---: | ---: | ---: |
| 1×1-idle | 10.127 / 10.194 | 9.643 / 9.643 | 9.654 / 9.662 |
| 1×1-busy | 16.691 / 16.740 | 16.188 / 16.191 | 16.199 / 16.203 |
| 3×2-idle | 16.669 / 16.694 | 15.874 / 15.882 | 15.888 / 15.898 |
| 3×2-busy | 23.167 / 23.210 | 22.361 / 22.365 | 22.373 / 22.373 |

New three-second profile CPU/storage fields, bell navigation, additional search/selection,
and mux-only tree/theme/root UI cases are implemented but **not measured in these
accepted groups**. Existing finite-output throughput/CPU/storage and clean recovery
results above remain separate. Restored-layout scale coverage and real default-period
crash recovery remain unmeasured.

#### w1-idle

| Operation (ms) | mux | tmux | tmux + persistence |
| --- | ---: | ---: | ---: |
| Populated attach (existing daemon) | 16.975 / 23.032 | 15.387 / 16.305 | 14.224 / 17.216 |
| create window | 11.715 / 11.961 | 8.659 / 8.953 | 7.920 / 8.022 |
| switch window | 3.556 / 3.675 | 3.778 / 4.031 | 3.913 / 4.764 |
| switch window back | 3.536 / 3.695 | 3.837 / 4.147 | 3.975 / 4.176 |
| Split top/bottom | 11.825 / 18.061 | 8.444 / 9.323 | 8.511 / 9.635 |
| focus vertical | 9.376 / 9.423 | 0.931 / 0.976 | 0.947 / 1.064 |
| focus vertical back | 9.366 / 9.425 | 0.877 / 0.918 | 0.867 / 0.922 |
| resize vertical | 9.319 / 9.425 | 2.280 / 2.324 | 2.265 / 2.345 |
| zoom vertical | 8.493 / 8.820 | 4.729 / 4.878 | 4.735 / 5.003 |
| unzoom vertical | 10.995 / 11.337 | 4.356 / 4.543 | 4.351 / 4.616 |
| delete pane vertical | 54.132 / 55.349 | 3.774 / 3.945 | 3.769 / 4.745 |
| Split left/right | 15.483 / 18.121 | 8.981 / 9.191 | 8.987 / 9.268 |
| focus horizontal | 4.970 / 9.562 | 1.033 / 1.071 | 1.044 / 1.069 |
| focus horizontal back | 4.964 / 9.496 | 0.949 / 0.997 | 0.941 / 1.007 |
| resize horizontal | 9.037 / 11.217 | 3.365 / 3.449 | 3.384 / 4.379 |
| zoom horizontal | 9.134 / 9.416 | 4.763 / 4.833 | 4.763 / 5.066 |
| unzoom horizontal | 10.803 / 11.097 | 5.370 / 5.464 | 5.318 / 5.658 |
| delete pane horizontal | 57.910 / 59.233 | 3.809 / 3.932 | 3.832 / 3.987 |
| split for break | 11.818 / 18.072 | 8.863 / 9.011 | 8.334 / 8.985 |
| break pane | 6.180 / 13.439 | 5.573 / 6.603 | 5.585 / 5.962 |
| Join pane (CLI→viewport) | 6.603 / 7.110 | 7.481 / 7.610 | 7.480 / 8.452 |
| delete window | 54.461 / 54.672 | 3.796 / 3.988 | 3.802 / 3.995 |
| reorder window left | 9.522 / 9.924 | 2.391 / 2.431 | 2.311 / 3.170 |
| reorder window right | 5.395 / 9.593 | 2.370 / 2.398 | 2.368 / 2.911 |
| open window rename | 10.527 / 10.797 | 1.958 / 2.042 | 1.966 / 2.111 |
| commit window rename | 10.680 / 10.817 | 3.193 / 3.299 | 3.200 / 4.089 |
| open session rename | 6.437 / 10.997 | 1.956 / 2.009 | 1.942 / 2.089 |
| commit session rename | 10.648 / 10.958 | 3.134 / 3.213 | 3.144 / 4.273 |
| create session | 11.600 / 11.683 | 8.668 / 9.327 | 8.782 / 9.311 |
| switch session | 15.109 / 15.862 | 2.294 / 2.712 | 2.563 / 3.748 |
| switch session back | 17.073 / 17.727 | 4.151 / 4.694 | 4.448 / 4.628 |
| delete session | 59.085 / 65.047 | 2.989 / 3.140 | 3.054 / 3.159 |
| terminal resize viewport | 7.852 / 36.010 | 4.466 / 4.663 | 4.469 / 4.795 |
| enter copy mode | 1.052 / 1.095 | 4.312 / 4.531 | 4.152 / 5.571 |
| history search backward commit | 14.814 / 16.943 | 10.850 / 11.145 | 10.658 / 11.193 |
| history top | 6.296 / 10.246 | 2.625 / 2.991 | 2.537 / 3.004 |
| copy cursor right | 0.684 / 0.719 | 0.972 / 1.044 | 0.941 / 1.062 |
| copy cursor left | 0.694 / 0.741 | 0.941 / 1.022 | 0.934 / 1.084 |
| copy big word forward | 1.276 / 1.309 | 1.020 / 1.075 | 0.992 / 1.109 |
| Yank line (clipboard receipt upper bound) | 9.612 / 9.901 | 54.402 / 54.793 | 54.420 / 54.573 |
| history bottom | 3.223 / 3.436 | 4.542 / 4.706 | 4.261 / 4.616 |
| exit copy mode | 0.668 / 0.714 | 3.886 / 4.011 | 3.699 / 3.985 |
| second client attach | 17.516 / 22.625 | 16.179 / 20.321 | 15.913 / 21.675 |
| Detach (process exit) | 1.106 / 1.120 | 1.107 / 1.126 | 1.115 / 1.349 |
| reattach | 18.382 / 22.484 | 15.301 / 22.282 | 19.403 / 21.944 |
| Initial PSS (MiB) | 10.127 / 10.194 | 9.643 / 9.643 | 9.654 / 9.662 |
| Initial RSS (MiB) | 19.770 / 19.836 | 19.777 / 19.777 | 19.789 / 19.797 |

#### w1-busy

| Operation (ms) | mux | tmux | tmux + persistence |
| --- | ---: | ---: | ---: |
| Populated attach (existing daemon) | 19.607 / 21.265 | 18.031 / 19.450 | 18.199 / 20.223 |
| create window | 13.057 / 16.843 | 8.784 / 9.122 | 7.982 / 8.646 |
| switch window | 3.660 / 3.777 | 3.851 / 4.068 | 3.777 / 4.095 |
| switch window back | 3.652 / 3.865 | 3.875 / 4.112 | 3.858 / 4.769 |
| Split top/bottom | 11.810 / 11.970 | 8.500 / 9.952 | 9.131 / 9.461 |
| focus vertical | 3.694 / 9.341 | 0.906 / 0.951 | 0.925 / 0.972 |
| focus vertical back | 9.390 / 9.827 | 0.867 / 0.914 | 0.872 / 0.951 |
| resize vertical | 8.881 / 9.687 | 2.277 / 2.341 | 2.277 / 3.370 |
| zoom vertical | 8.499 / 8.932 | 4.568 / 5.065 | 4.745 / 4.876 |
| unzoom vertical | 11.144 / 11.519 | 4.213 / 4.634 | 4.375 / 5.329 |
| delete pane vertical | 54.142 / 54.371 | 3.723 / 3.948 | 3.716 / 3.896 |
| Split left/right | 15.825 / 18.184 | 9.013 / 9.212 | 9.021 / 9.229 |
| focus horizontal | 4.732 / 9.535 | 0.998 / 1.040 | 0.973 / 1.044 |
| focus horizontal back | 8.412 / 9.531 | 0.935 / 0.980 | 0.933 / 0.972 |
| resize horizontal | 11.195 / 11.895 | 3.399 / 3.479 | 3.403 / 3.539 |
| zoom horizontal | 9.186 / 13.380 | 4.753 / 4.938 | 4.713 / 5.044 |
| unzoom horizontal | 10.885 / 12.986 | 5.230 / 5.683 | 5.226 / 5.898 |
| delete pane horizontal | 58.145 / 59.632 | 3.813 / 3.922 | 3.809 / 3.909 |
| split for break | 11.879 / 17.612 | 8.853 / 9.043 | 8.873 / 9.063 |
| break pane | 13.475 / 13.754 | 5.555 / 5.711 | 5.517 / 6.363 |
| Join pane (CLI→viewport) | 6.967 / 9.671 | 7.569 / 7.766 | 7.535 / 7.950 |
| delete window | 54.728 / 56.018 | 3.767 / 4.004 | 3.801 / 5.047 |
| reorder window left | 1.378 / 9.551 | 2.196 / 2.407 | 2.349 / 3.210 |
| reorder window right | 5.502 / 9.557 | 2.172 / 2.426 | 2.280 / 3.098 |
| open window rename | 8.536 / 10.918 | 1.923 / 2.058 | 1.941 / 2.083 |
| commit window rename | 10.647 / 10.874 | 3.016 / 3.262 | 3.039 / 3.990 |
| open session rename | 3.241 / 10.663 | 1.928 / 2.114 | 1.932 / 2.050 |
| commit session rename | 3.843 / 10.788 | 3.008 / 3.292 | 3.017 / 3.324 |
| create session | 11.624 / 17.624 | 8.672 / 9.265 | 8.712 / 9.177 |
| switch session | 15.355 / 17.480 | 2.356 / 2.697 | 2.461 / 2.774 |
| switch session back | 12.650 / 18.710 | 4.444 / 4.827 | 4.449 / 4.730 |
| delete session | 64.859 / 65.094 | 3.036 / 3.135 | 3.087 / 3.481 |
| terminal resize viewport | 8.156 / 13.691 | 4.478 / 4.569 | 4.482 / 4.841 |
| enter copy mode | 6.422 / 7.377 | 4.113 / 4.462 | 4.122 / 5.468 |
| history search backward commit | 15.691 / 16.939 | 10.680 / 11.248 | 10.595 / 11.150 |
| history top | 7.852 / 10.190 | 2.539 / 3.043 | 2.537 / 2.991 |
| copy cursor right | 0.693 / 6.763 | 0.940 / 1.005 | 0.940 / 1.010 |
| copy cursor left | 2.231 / 3.918 | 0.927 / 1.022 | 0.977 / 1.057 |
| copy big word forward | 2.809 / 3.945 | 0.970 / 1.048 | 0.989 / 1.067 |
| Yank line (clipboard receipt upper bound) | 10.121 / 16.348 | 54.417 / 54.554 | 54.438 / 54.541 |
| history bottom | 3.399 / 6.307 | 4.541 / 4.824 | 4.562 / 4.833 |
| exit copy mode | 0.654 / 1.751 | 3.897 / 4.036 | 3.932 / 4.216 |
| second client attach | 20.665 / 26.575 | 18.318 / 20.031 | 17.151 / 23.045 |
| Detach (process exit) | 1.110 / 1.129 | 1.110 / 1.132 | 1.113 / 3.215 |
| reattach | 24.861 / 30.917 | 17.446 / 18.461 | 20.794 / 24.862 |
| Initial PSS (MiB) | 16.691 / 16.740 | 16.188 / 16.191 | 16.199 / 16.203 |
| Initial RSS (MiB) | 31.025 / 31.074 | 31.078 / 31.082 | 31.090 / 31.094 |

#### w3-idle

| Operation (ms) | mux | tmux | tmux + persistence |
| --- | ---: | ---: | ---: |
| Populated attach (existing daemon) | 23.897 / 30.621 | 21.760 / 23.977 | 21.456 / 23.809 |
| create window | 12.096 / 12.325 | 8.963 / 9.244 | 8.363 / 8.942 |
| switch window | 4.679 / 5.269 | 4.772 / 5.209 | 4.827 / 6.282 |
| switch window back | 3.791 / 3.928 | 3.874 / 4.147 | 3.934 / 4.222 |
| Split top/bottom | 18.046 / 18.243 | 8.611 / 9.377 | 8.764 / 9.715 |
| focus vertical | 9.410 / 9.469 | 1.028 / 1.076 | 1.035 / 2.248 |
| focus vertical back | 9.396 / 9.466 | 0.973 / 1.016 | 0.950 / 1.032 |
| resize vertical | 9.314 / 9.379 | 2.384 / 2.502 | 2.358 / 2.483 |
| zoom vertical | 8.565 / 8.950 | 4.659 / 4.921 | 4.762 / 5.979 |
| unzoom vertical | 11.395 / 12.570 | 4.199 / 4.511 | 4.190 / 4.477 |
| delete pane vertical | 54.224 / 54.569 | 3.886 / 3.984 | 3.871 / 3.976 |
| Split left/right | 18.118 / 18.224 | 9.067 / 9.386 | 9.206 / 9.627 |
| focus horizontal | 9.486 / 9.588 | 1.119 / 1.147 | 1.119 / 1.241 |
| focus horizontal back | 0.543 / 9.572 | 1.021 / 1.073 | 1.022 / 1.099 |
| resize horizontal | 10.760 / 12.536 | 3.496 / 3.602 | 3.521 / 4.484 |
| zoom horizontal | 9.344 / 11.988 | 4.678 / 4.891 | 4.766 / 5.875 |
| unzoom horizontal | 10.990 / 11.228 | 5.170 / 5.539 | 5.314 / 5.741 |
| delete pane horizontal | 58.402 / 60.142 | 3.934 / 4.021 | 3.994 / 5.122 |
| split for break | 17.972 / 18.212 | 8.468 / 9.044 | 8.950 / 9.382 |
| break pane | 13.781 / 13.969 | 5.481 / 5.830 | 5.474 / 5.812 |
| Join pane (CLI→viewport) | 7.119 / 7.867 | 7.641 / 7.974 | 7.605 / 8.562 |
| delete window | 54.894 / 56.904 | 3.972 / 5.191 | 3.993 / 4.251 |
| reorder window left | 0.811 / 9.673 | 2.281 / 2.505 | 2.451 / 3.814 |
| reorder window right | 9.557 / 9.620 | 2.433 / 2.498 | 2.245 / 3.469 |
| open window rename | 10.579 / 10.738 | 1.966 / 2.035 | 1.978 / 2.056 |
| commit window rename | 10.726 / 10.900 | 3.252 / 3.324 | 3.200 / 3.361 |
| open session rename | 10.538 / 10.628 | 1.966 / 2.035 | 1.972 / 2.040 |
| commit session rename | 10.697 / 11.060 | 3.197 / 3.258 | 3.187 / 4.297 |
| create session | 12.070 / 12.299 | 8.725 / 9.437 | 8.854 / 9.363 |
| switch session | 15.235 / 15.906 | 2.427 / 2.777 | 2.452 / 4.122 |
| switch session back | 19.887 / 21.209 | 4.202 / 4.595 | 4.248 / 4.693 |
| delete session | 65.179 / 66.625 | 3.220 / 3.341 | 3.269 / 3.383 |
| terminal resize viewport | 12.301 / 19.016 | 4.499 / 4.623 | 4.536 / 5.717 |
| enter copy mode | 1.043 / 1.094 | 4.521 / 4.605 | 4.190 / 4.699 |
| history search backward commit | 16.184 / 17.805 | 10.909 / 11.226 | 10.767 / 12.665 |
| history top | 6.330 / 10.297 | 2.538 / 3.025 | 2.603 / 3.135 |
| copy cursor right | 0.699 / 0.744 | 0.977 / 1.118 | 0.995 / 1.093 |
| copy cursor left | 0.691 / 0.733 | 0.969 / 1.099 | 1.002 / 1.084 |
| copy big word forward | 1.278 / 1.336 | 1.009 / 1.158 | 1.011 / 1.115 |
| Yank line (clipboard receipt upper bound) | 9.707 / 10.452 | 54.670 / 54.802 | 54.759 / 57.084 |
| history bottom | 3.302 / 4.090 | 4.622 / 4.754 | 4.439 / 4.855 |
| exit copy mode | 0.671 / 0.725 | 3.970 / 4.058 | 3.786 / 4.166 |
| second client attach | 23.122 / 27.903 | 20.299 / 20.965 | 18.869 / 24.176 |
| Detach (process exit) | 1.108 / 1.113 | 1.114 / 1.119 | 1.118 / 3.216 |
| reattach | 26.394 / 32.668 | 19.926 / 26.882 | 23.452 / 25.862 |
| Initial PSS (MiB) | 16.669 / 16.694 | 15.874 / 15.882 | 15.888 / 15.898 |
| Initial RSS (MiB) | 45.682 / 45.707 | 45.371 / 45.379 | 45.385 / 51.383 |

#### w3-busy

| Operation (ms) | mux | tmux | tmux + persistence |
| --- | ---: | ---: | ---: |
| Populated attach (existing daemon) | 25.403 / 32.275 | 20.063 / 23.661 | 18.936 / 28.210 |
| create window | 11.989 / 16.531 | 8.401 / 8.983 | 8.094 / 8.805 |
| switch window | 5.699 / 9.750 | 4.810 / 5.232 | 4.756 / 5.203 |
| switch window back | 3.828 / 11.041 | 4.069 / 4.222 | 3.886 / 5.436 |
| Split top/bottom | 18.061 / 18.481 | 8.590 / 9.372 | 8.539 / 9.325 |
| focus vertical | 3.585 / 9.456 | 0.988 / 1.026 | 0.988 / 1.052 |
| focus vertical back | 9.004 / 9.464 | 0.908 / 0.973 | 0.916 / 0.973 |
| resize vertical | 7.400 / 9.457 | 2.325 / 2.409 | 2.351 / 2.408 |
| zoom vertical | 11.453 / 12.484 | 4.537 / 4.886 | 4.771 / 5.965 |
| unzoom vertical | 11.376 / 11.567 | 4.105 / 4.461 | 4.286 / 4.491 |
| delete pane vertical | 55.079 / 55.589 | 3.810 / 3.877 | 3.855 / 3.963 |
| Split left/right | 16.270 / 17.909 | 9.055 / 9.264 | 9.078 / 9.280 |
| focus horizontal | 4.975 / 9.572 | 1.068 / 1.117 | 1.065 / 1.104 |
| focus horizontal back | 6.881 / 9.536 | 0.999 / 1.061 | 0.998 / 1.056 |
| resize horizontal | 9.035 / 11.403 | 3.491 / 3.563 | 3.473 / 3.595 |
| zoom horizontal | 9.445 / 13.716 | 4.755 / 4.928 | 4.790 / 4.877 |
| unzoom horizontal | 11.067 / 14.443 | 5.363 / 5.548 | 5.319 / 5.581 |
| delete pane horizontal | 58.552 / 59.830 | 3.910 / 4.017 | 3.903 / 4.197 |
| split for break | 12.200 / 18.185 | 8.954 / 9.084 | 8.716 / 9.029 |
| break pane | 13.657 / 13.912 | 5.465 / 5.748 | 5.632 / 6.226 |
| Join pane (CLI→viewport) | 8.378 / 13.141 | 7.551 / 7.731 | 7.566 / 7.781 |
| delete window | 54.817 / 56.150 | 3.930 / 4.091 | 4.029 / 5.447 |
| reorder window left | 7.363 / 9.602 | 2.235 / 2.499 | 2.442 / 2.477 |
| reorder window right | 1.746 / 9.562 | 2.397 / 2.493 | 2.201 / 2.450 |
| open window rename | 9.465 / 10.630 | 1.964 / 2.049 | 1.959 / 2.115 |
| commit window rename | 10.660 / 10.975 | 3.227 / 3.276 | 3.236 / 4.673 |
| open session rename | 9.960 / 10.594 | 1.959 / 2.013 | 1.961 / 2.140 |
| commit session rename | 10.239 / 10.984 | 3.159 / 3.288 | 3.181 / 3.422 |
| create session | 12.055 / 16.289 | 8.761 / 9.344 | 9.023 / 9.438 |
| switch session | 15.470 / 16.134 | 2.361 / 2.777 | 2.352 / 2.863 |
| switch session back | 20.076 / 20.864 | 4.212 / 4.568 | 4.187 / 5.691 |
| delete session | 58.596 / 65.354 | 3.169 / 3.310 | 3.195 / 3.880 |
| terminal resize viewport | 13.998 / 21.086 | 4.491 / 4.566 | 4.525 / 4.811 |
| enter copy mode | 5.040 / 8.498 | 4.173 / 4.607 | 4.158 / 5.800 |
| history search backward commit | 14.656 / 16.782 | 11.067 / 11.307 | 10.618 / 11.432 |
| history top | 6.133 / 10.237 | 2.947 / 3.020 | 2.479 / 3.006 |
| copy cursor right | 8.588 / 9.017 | 0.970 / 1.064 | 0.930 / 1.009 |
| copy cursor left | 1.744 / 2.024 | 0.937 / 1.045 | 0.928 / 1.002 |
| copy big word forward | 1.929 / 2.212 | 0.983 / 1.095 | 0.976 / 1.089 |
| Yank line (clipboard receipt upper bound) | 9.736 / 10.202 | 54.627 / 54.733 | 54.645 / 54.756 |
| history bottom | 3.324 / 4.311 | 4.618 / 4.811 | 4.325 / 4.714 |
| exit copy mode | 0.627 / 0.713 | 3.935 / 4.053 | 3.754 / 4.119 |
| second client attach | 21.905 / 26.827 | 18.715 / 21.170 | 18.804 / 22.042 |
| Detach (process exit) | 1.108 / 1.127 | 1.110 / 1.118 | 1.116 / 1.125 |
| reattach | 25.731 / 30.986 | 18.333 / 26.404 | 21.564 / 29.677 |
| Initial PSS (MiB) | 23.167 / 23.210 | 22.361 / 22.365 | 22.373 / 22.373 |
| Initial RSS (MiB) | 56.926 / 56.969 | 56.668 / 56.672 | 56.680 / 56.680 |

#### Retained broad-suite evidence

[Manifest](results/hosted-ci/36762906986/manifest.json) lists raw-shard digests and
blocked groups. The hosted repack/audit run is [36767080733](https://github.com/any-0/mux/actions/runs/36767080733).
All 378 process samples (including 18 warm-ups), diagnostic preflights, environments,
commands, actual PTY input/ANSI, full tagged histories and failure evidence are retained
in git in complete group shards; no failed trial is removed from a group.

- [w1-idle raw ZIP](results/hosted-ci/36762906986/w1-idle-raw.zip) · [independent audit](results/hosted-ci/36762906986/w1-idle-audit.json)
- [w1-busy raw ZIP](results/hosted-ci/36762906986/w1-busy-raw.zip) · [independent audit](results/hosted-ci/36762906986/w1-busy-audit.json)
- [w3-idle raw ZIP](results/hosted-ci/36762906986/w3-idle-raw.zip) · [independent audit](results/hosted-ci/36762906986/w3-idle-audit.json)
- [w3-busy raw ZIP](results/hosted-ci/36762906986/w3-busy-raw.zip) · [independent audit](results/hosted-ci/36762906986/w3-busy-audit.json)
- [w6-idle raw ZIP](results/hosted-ci/36762906986/w6-idle-raw.zip) · [blocked audit](results/hosted-ci/36762906986/w6-idle-audit-failure.txt)
- [w6-busy raw ZIP](results/hosted-ci/36762906986/w6-busy-raw.zip) · [blocked audit](results/hosted-ci/36762906986/w6-busy-audit-failure.txt)
- [diagnostic-preflight raw ZIP](results/hosted-ci/36762906986/diagnostic-preflight-raw.zip)

Re-audit a complete group (example):

```sh
python3 -m zipfile -e benchmarks/results/hosted-ci/36762906986/w1-idle-raw.zip /tmp/mux-interactive-group
./scripts/benchmark-nix python3 benchmarks/audit_interactive.py /tmp/mux-interactive-group/interactive-performance --windows 1 --load idle
```
<!-- interactive-results:end -->
