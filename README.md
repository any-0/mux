# mux

`mux` is a small standalone terminal multiplexer built for this repository's
specific tmux workflow. A background daemon owns real PTYs, so shells keep
running when an attached terminal disconnects. It does not invoke, wrap,
configure, or replace tmux.

The interface has a narrow numbered strip on the left. Each window takes two
rows: its number is on the first row and the centered icon for the active
pane's foreground job is on the second. Only each
label and its one-cell horizontal padding are colored; the window groups are
centered vertically, and their width adapts when the window count gains or
loses digits. A continuous vertical separator divides the strip from the
terminal or pane layout. The active window keeps its colored tab but leaves
its number blank and shows a dot. Focus mode and the session tree
hide the strip and use the full terminal.

Program icons use exact executable names reported by the operating system.
Filenames, arguments, and terminal titles do not affect detection. The foreground
job's root process takes precedence over its helpers; pipelines prefer their
group leader, then the remaining root with the lowest PID. Stopped and exited
processes are excluded. Scripts identify as their interpreter. Unknown executables
show `·`. The name-to-icon list lives in `src/server/process.rs`; Nix's
`.<name>-wrapped` executable names use the same entry as `<name>`.
Icons refresh every 250 ms even in quiet panes, and unchanged icons cause no
repaint.

The separator uses the active window color and becomes a left-pointing `┤` on
the active row without coloring the terminal background. One blank vertical
column separates that line from the content on its right. A terminal bell sends
an enchanted pink-and-orchid shimmer across both rows of the window label. Its highlight
moves smoothly along the horizontal axis. The active window plays the shimmer
once; unseen windows rest on the bright magenta-and-white end of that cycle
between passes, then repeat the animation until selected. Bells from other
sessions use the same cycle in a three-cell alert at the top left, showing `!`
for one notification or the notification count for several. Neither window nor
cross-session alerts ever look like an ordinary inactive label while a bell
remains pending. `bell_style` turns the animation down to a steady label or off
entirely.
The strip uses the inspected tmux Dusk colors. Vim mode leaves the window colors
alone and uses `accent` behind them.

Split panes are divided by single lines that join where they meet, so a divider
ending against another shows `┤`, `├`, `┬`, `┴`, or `┼` rather than one line
breaking the other.

Every color mux paints is exact, and a client that reports no 24-bit color
support is sent the nearest entry of the 256-color palette instead. Terminals
are asked through `COLORTERM`, or a `TERM` that names direct color.

Every screen is painted into a cell buffer and compared against the frame the
client is already showing, so only the cells that actually changed are sent and
a repeated frame costs nothing. Repaints are coalesced into at most one frame
every 8 ms, and the daemon blocks until an event or the next process sample,
bell animation, or expiring message is due.

## Documentation

- [`docs/cli.md`](docs/cli.md) — every command, argument, and alias
- [`docs/scripting.md`](docs/scripting.md) — the `MUX` and `MUX_PANE` variables, queries, and the clipboard command
- [`docs/ssh.md`](docs/ssh.md) — lost connections, `mux auto`, and the clipboard through SSH
- [`docs/architecture.md`](docs/architecture.md) — the daemon, the socket, the state, and the source files

## Build and run

### Hosted-CI benchmarks

The table below covers the initial single-pane workload and clean recovery.
The broader [supported-operation inventory and coverage matrix](benchmarks/COVERAGE.md)
records 240 independently accepted core trials below. Additional cases and the
corrected four-pane grid are undergoing validation; diagnostics are not performance results.

Newer separate datasets: [360 paired interactive trials and 20 mux-only trials](#complete-expanded-interactive-sweep-hosted-ci-2026-09-30), and [180 cold restored-layout/fresh-provisioning startup trials](#cold-saved-layout-scale-startup-hosted-ci-2026-09-30). Their workloads/endpoints differ from the historical microbenchmark below; results are not pooled.

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

[Full results, Nix pins, methods and raw samples](benchmarks/README.md) include
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

Build without installing anything:

```sh
cargo build --release
./target/release/mux
```

Linux and macOS are supported, including Apple Silicon Macs.

The first client starts the daemon and `Session 1`. Closing or losing that
client detaches it; running `mux` again reattaches while the PTYs continue to
run. A named session can be selected or created from the command line:

```sh
./target/release/mux --session work
```

The socket is `$XDG_RUNTIME_DIR/mux.sock`, or `/tmp/mux-$UID/mux.sock` when no
runtime directory is set. Directories mux picks for itself are created private
to the user and refused if something else already owns them. One daemon at a
time owns the state directory, held with a lock file, so two clients starting
at the same moment cannot end up with two daemons restoring the same sessions.
Stop the daemon and its running processes explicitly with:

```sh
./target/release/mux kill-server
```

The implemented interactive operations are also available as tmux-style
commands. When run inside mux they act on the originating pane and its attached
client:

```sh
mux choose-tree
mux detach
mux new-window
mux new-session -s work
mux rename-session coding
mux rename-window logs     # no name restores the program's title
mux split-window           # top/bottom
mux split-window -h        # left/right
mux select-pane -L
mux resize-pane -L 5       # move the nearest divider five cells
mux focus-mode             # toggle focus mode; also mux resize-pane -Z
mux break-pane             # the active pane gets a window of its own
mux join-pane -h -t 2      # and goes back into window 2
mux swap-window -t 1
mux select-window -t 2
mux vim-mode
mux kill-pane
mux kill-session
```

`mux set-session-root` and `mux jump-to-bell` expose the two mux-specific
operations. `mux stop` remains an alias for `mux kill-server`.

Read-only commands print to standard output and need no attached client, so
they can feed a prompt or a picker:

```sh
mux list-sessions   # or mux ls
mux list-windows
mux list-panes
```

Install or replace `~/.local/bin/mux` with:

```sh
cargo build --release
install -m 755 target/release/mux ~/.local/bin/mux
```

Apart from that explicit install step, no binaries or configuration files are
installed; only the durable runtime state described below is written.


<!-- interactive-followup:start -->
### Complete expanded interactive sweep (hosted CI, 2026-09-30)

[Run 36774371342](https://github.com/any-0/mux/actions/runs/36774371342): **360 accepted paired
trials**, 20 per variant in each of six groups, **53 endpoints per trial**;
18 warm-ups excluded. A separate **20-trial, 12-endpoint mux-only UI suite** also
passed (one warm-up excluded). These are real attached-PTY measurements, not
diagnostic smoke timings. All trials, including warm-ups, passed visible-result
gates. Paired groups pass independent fixture/input/resource audits; mux-only UI
passes complete input/timestamp/visible-transition/resource audits. The corrected logical pane
mapping makes all four-pane content/geometry slots equal across variants.

Measured mux: `d6dd228054231e77772bd17a412d8f0d07871835`. Executed harness:
`a9134207e3c3130994dcdd919ffcaf18025a74a8` (branch candidate `406ae9a514ab7e16084f2e623324592c2f491f9a`).
Project Nix pin `2fc6539b481e1d2569f25f8799236694180c0993`, Rust/Cargo 1.93.0, tmux 3.6a,
Bash 5.3.9, Python 3.13.12. Resurrect/continuum pins and loaded-plugin checks are
documented above. No PR #4 code is mixed into this runtime. All groups ran
sequentially on the same hosted VM; variant order is shuffled within each block.
Each declared group resets the documented shuffle seed. Exact runner hardware,
image, commands, dependency versions and raw input/output are retained in each ZIP.
The VM reported AMD EPYC 7763 64-Core Processor, 4 logical CPUs, 16373452 KiB RAM,
runner image `20260920.314.1`.

Seeded layouts are 1×1, 3×2 and 6×4 windows×panes, plus one equal background
window; mutations use one scratch window and create a second session. Content
area is 100×40 cells, 1,000 low-entropy ASCII tagged records per seeded pane, retained-history cap
20,000. Outer PTYs are mux 105×40 and tmux 100×41 to compensate for the
five-column sidebar versus one-row status line; this is a content-matched rather
than equal-outer-rectangle experiment. Idle and busy fixtures match; busy adds the same 50 Hz, five-line ANSI
producer. Native chrome/bootstrap command history differ. Memory includes the
daemon, attached client, shells and producer, and is sampled before mutations.

Every value is **median / nearest-rank p95**, 20 fresh process trials per cell.
Build/filesystem caches are warm; 20 trials give a coarse tail estimate.
Latency stops at decoded correct viewport/cursor/selection style, with metadata
checked afterward. Populated startup means client attach to an existing daemon,
not cold populated restart. Native editors/confirmations are prepared untimed;
character selection times two selected cells, not an invisible selection start.
Join measures CLI dispatch→viewport, detach process exit, and yank an atomic
clipboard-file receipt with up to 50 ms polling. These endpoints are not equated
with rendering. Zoom/sidebar geometry and native editor policies differ.

CPU is a separate ≥3-second process-tree tick observation, percentage of one CPU,
and misses exited helpers. PSS/RSS are resident snapshots, not peak usage.
Observed state-directory storage is not equal durability: mux journals continuously;
the stack has not reached its real 15-minute scheduled save in these short trials.
Controller/decoder cost is included, pixels and exclusive physical hardware are
not measured. Hosted VM data are not Julian's hardware and do not establish a
universal ranking. Earlier accepted scroll/throughput/clean-recovery and core
datasets below remain separate; do not pool across VMs or changed endpoints.
These historical runs did not measure cold restored-layout scale startup; see the separate cold-startup follow-up. Default-period crash recovery remains unmeasured.

#### Initial PSS (MiB)

| Seeded layout / load | mux | tmux | tmux + persistence |
| --- | ---: | ---: | ---: |
| 1×1 idle | 10.138 / 10.187 | 9.643 / 9.643 | 9.654 / 9.682 |
| 1×1 busy | 16.705 / 16.748 | 16.188 / 16.191 | 16.199 / 16.250 |
| 3×2 idle | 16.663 / 16.698 | 15.870 / 15.878 | 15.890 / 15.929 |
| 3×2 busy | 23.171 / 23.198 | 22.361 / 22.365 | 22.361 / 22.377 |
| 6×4 idle | 37.638 / 37.740 | 37.401 / 37.409 | 37.415 / 37.421 |
| 6×4 busy | 44.182 / 44.240 | 43.839 / 43.854 | 43.856 / 43.870 |

#### Profile CPU (% of one CPU; lower bound)

| Seeded layout / load | mux | tmux | tmux + persistence |
| --- | ---: | ---: | ---: |
| 1×1 idle | 2.315 / 2.319 | 0.000 / 0.000 | 0.331 / 0.662 |
| 1×1 busy | 4.133 / 4.632 | 0.827 / 1.323 | 0.993 / 1.325 |
| 3×2 idle | 7.607 / 7.928 | 0.000 / 0.000 | 0.331 / 0.662 |
| 3×2 busy | 9.592 / 10.242 | 0.662 / 1.323 | 1.323 / 1.651 |
| 6×4 idle | 28.532 / 30.920 | 0.000 / 0.330 | 0.330 / 0.661 |
| 6×4 busy | 30.801 / 32.344 | 0.991 / 1.319 | 1.650 / 1.982 |

<details>
<summary>1×1 idle: all 53 endpoints</summary>

| Operation (ms) | mux | tmux | tmux + persistence |
| --- | ---: | ---: | ---: |
| break pane | 6.488 / 6.772 | 5.681 / 6.043 | 5.881 / 6.642 |
| cancel character selection | 0.742 / 0.849 | 1.057 / 1.195 | 1.066 / 1.214 |
| commit session rename | 11.045 / 11.249 | 3.213 / 3.511 | 3.373 / 3.484 |
| commit window rename | 10.959 / 11.167 | 3.302 / 3.561 | 3.420 / 3.766 |
| copy big word forward | 1.318 / 1.549 | 1.142 / 1.296 | 1.132 / 1.262 |
| copy cursor left | 0.736 / 0.841 | 1.093 / 1.193 | 1.031 / 1.215 |
| copy cursor right | 0.727 / 0.869 | 1.060 / 1.166 | 1.069 / 1.180 |
| copy find character commit | 0.811 / 0.837 | 3.627 / 3.775 | 3.757 / 5.321 |
| create session | 11.795 / 12.299 | 8.596 / 9.280 | 8.749 / 9.298 |
| create window | 11.644 / 12.055 | 8.449 / 8.934 | 7.685 / 8.410 |
| delete pane horizontal | 58.365 / 60.841 | 4.021 / 4.341 | 4.035 / 4.974 |
| delete pane vertical | 54.698 / 56.737 | 3.893 / 4.126 | 3.925 / 4.098 |
| delete session | 57.856 / 59.948 | 3.162 / 3.431 | 3.183 / 4.425 |
| delete window | 54.981 / 55.741 | 3.981 / 4.236 | 3.997 / 5.225 |
| Detach (process exit) | 1.118 / 1.135 | 1.120 / 1.129 | 1.130 / 3.253 |
| enter copy mode | 1.136 / 1.338 | 4.641 / 4.922 | 4.539 / 5.263 |
| exit copy mode | 1.022 / 1.169 | 3.819 / 4.067 | 4.047 / 4.245 |
| extend character selection | 0.753 / 0.863 | 0.324 / 0.365 | 0.321 / 0.372 |
| focus horizontal | 0.539 / 9.668 | 1.071 / 1.183 | 1.067 / 1.130 |
| focus horizontal back | 0.532 / 9.931 | 1.043 / 1.117 | 1.041 / 1.157 |
| focus vertical | 0.556 / 9.744 | 0.958 / 1.080 | 0.952 / 2.087 |
| focus vertical back | 0.567 / 9.645 | 0.935 / 1.062 | 0.938 / 1.077 |
| history bottom | 3.457 / 3.786 | 3.600 / 4.210 | 4.062 / 4.341 |
| history search backward commit | 15.212 / 17.235 | 11.359 / 11.717 | 11.364 / 11.850 |
| history search forward commit | 14.823 / 16.497 | 9.951 / 10.185 | 10.043 / 10.706 |
| history search next match | 2.455 / 2.756 | 2.327 / 2.806 | 2.495 / 2.935 |
| history search previous match | 2.205 / 2.682 | 2.189 / 2.800 | 2.453 / 2.923 |
| history top | 2.440 / 2.774 | 2.903 / 3.310 | 2.602 / 3.209 |
| Join pane (CLI dispatch → viewport) | 7.224 / 7.565 | 7.656 / 8.520 | 7.842 / 8.596 |
| navigate pending bell | 4.920 / 13.356 | 4.637 / 4.915 | 4.756 / 5.276 |
| open session rename | 2.405 / 10.922 | 2.052 / 2.130 | 2.078 / 2.174 |
| open window rename | 2.417 / 10.995 | 2.075 / 2.209 | 2.063 / 2.148 |
| Populated client attach (existing daemon) | 17.936 / 25.672 | 16.492 / 21.367 | 16.878 / 21.245 |
| reattach | 19.451 / 24.802 | 15.529 / 18.708 | 21.933 / 26.041 |
| reorder window left | 0.827 / 10.232 | 2.533 / 2.644 | 2.607 / 2.678 |
| reorder window right | 0.849 / 9.862 | 2.516 / 2.665 | 2.573 / 3.349 |
| resize horizontal | 9.322 / 9.921 | 3.583 / 3.808 | 3.598 / 3.768 |
| resize vertical | 2.010 / 9.456 | 2.391 / 2.575 | 2.398 / 2.532 |
| second client attach | 18.837 / 23.185 | 16.306 / 17.800 | 16.636 / 19.120 |
| select two characters | 0.761 / 0.860 | 1.243 / 1.289 | 1.178 / 1.370 |
| split for break | 11.960 / 12.821 | 8.029 / 9.105 | 8.166 / 8.962 |
| split horizontal | 15.599 / 15.955 | 8.574 / 9.413 | 8.661 / 9.075 |
| split vertical | 11.900 / 12.292 | 8.189 / 9.231 | 8.189 / 9.723 |
| switch session | 4.892 / 5.449 | 2.720 / 2.951 | 2.510 / 2.962 |
| switch session back | 4.810 / 5.090 | 4.549 / 4.820 | 4.336 / 4.838 |
| switch window | 3.806 / 4.000 | 4.049 / 4.233 | 4.001 / 5.062 |
| switch window back | 3.841 / 4.148 | 4.070 / 4.399 | 4.054 / 5.174 |
| terminal resize viewport | 8.680 / 14.396 | 4.843 / 4.999 | 4.760 / 5.415 |
| unzoom horizontal | 11.261 / 12.619 | 5.679 / 6.117 | 5.724 / 5.942 |
| unzoom vertical | 10.532 / 11.172 | 4.513 / 4.846 | 4.483 / 4.912 |
| Yank line (clipboard receipt upper bound) | 9.401 / 9.591 | 54.451 / 54.680 | 54.439 / 54.762 |
| zoom horizontal | 9.664 / 10.144 | 4.985 / 5.342 | 5.024 / 5.780 |
| zoom vertical | 8.879 / 9.323 | 4.878 / 5.058 | 4.992 / 5.829 |

</details>

<details>
<summary>RSS and observed state storage</summary>

#### Initial RSS (MiB)

| Seeded layout / load | mux | tmux | tmux + persistence |
| --- | ---: | ---: | ---: |
| 1×1 idle | 19.779 / 19.828 | 19.777 / 19.777 | 19.789 / 19.816 |
| 1×1 busy | 31.039 / 31.082 | 31.078 / 31.082 | 31.090 / 31.141 |
| 3×2 idle | 45.666 / 45.711 | 45.367 / 45.375 | 45.387 / 45.426 |
| 3×2 busy | 56.930 / 56.957 | 56.668 / 56.672 | 56.668 / 56.684 |
| 6×4 idle | 137.037 / 137.145 | 137.273 / 137.281 | 137.285 / 137.293 |
| 6×4 busy | 148.340 / 148.398 | 148.555 / 148.570 | 148.572 / 148.586 |

#### Observed state logical bytes (different durability)

| Seeded layout / load | mux | tmux | tmux + persistence |
| --- | ---: | ---: | ---: |
| 1×1 idle | 62359.000 / 62684.000 | 0.000 / 0.000 | 0.000 / 0.000 |
| 1×1 busy | 120509.000 / 120992.000 | 0.000 / 0.000 | 0.000 / 0.000 |
| 3×2 idle | 205817.500 / 206415.000 | 0.000 / 0.000 | 0.000 / 0.000 |
| 3×2 busy | 264231.000 / 264640.000 | 0.000 / 0.000 | 0.000 / 0.000 |
| 6×4 idle | 722297.500 / 724339.000 | 0.000 / 0.000 | 0.000 / 0.000 |
| 6×4 busy | 781002.500 / 782644.000 | 0.000 / 0.000 | 0.000 / 0.000 |

#### Observed state allocated bytes (different durability)

| Seeded layout / load | mux | tmux | tmux + persistence |
| --- | ---: | ---: | ---: |
| 1×1 idle | 77824.000 / 81920.000 | 0.000 / 0.000 | 0.000 / 0.000 |
| 1×1 busy | 135168.000 / 139264.000 | 0.000 / 0.000 | 0.000 / 0.000 |
| 3×2 idle | 227328.000 / 237568.000 | 0.000 / 0.000 | 0.000 / 0.000 |
| 3×2 busy | 286720.000 / 290816.000 | 0.000 / 0.000 | 0.000 / 0.000 |
| 6×4 idle | 763904.000 / 782336.000 | 0.000 / 0.000 | 0.000 / 0.000 |
| 6×4 busy | 823296.000 / 835584.000 | 0.000 / 0.000 | 0.000 / 0.000 |

</details>

[All six groups, 53 endpoints, RSS/storage and mux-only UI tables](benchmarks/README.md#complete-expanded-interactive-sweep-hosted-ci-2026-09-30), [raw manifest](benchmarks/results/hosted-ci/36774371342/manifest.json), and [coverage inventory](benchmarks/COVERAGE.md).
<!-- interactive-followup:end -->

<!-- interactive-results:start -->
### Expanded interactive measurements (hosted CI, 2026-09-30)

[Run 36762906986](https://github.com/any-0/mux/actions/runs/36762906986)
completed the 360-trial sweep. Independent paired fixture/input/resource audits
accept **240 trials**: 20 per variant in each of four groups (1×1 and 3×2,
idle/busy), plus excluded warm-ups. Each trial covers 45 endpoints across roughly
20 operation families. The two 6×4 groups are **rejected** because native pane
numbering put tagged content in different grid slots; their raw samples and audit
failures are retained. The corrected follow-up sweep above accepts all six groups.

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
The two accepted historical scales use full-width panes. Corrected wider-grid
results are in the separate follow-up above; these original large groups remain rejected.

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
results above remain separate. Those groups did not measure cold restored-layout scale
startup; see the separate follow-up below. Real default-period crash recovery remains unmeasured.

<details>
<summary>All 45 core endpoints: 1×1 idle</summary>

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

</details>

[Manifest](benchmarks/results/hosted-ci/36762906986/manifest.json), [complete tables](benchmarks/README.md#expanded-interactive-measurements-hosted-ci-2026-09-30), and [inventory](benchmarks/COVERAGE.md) preserve exact coverage and remaining gaps.
<!-- interactive-results:end -->

## Persistence and recovery

### Testing and upgrades

Run `./scripts/test-nix` to enter the project’s pinned `.nix` development shell
and run formatting, unit tests, vendored terminal-parser tests, and strict Clippy
checks. CI uses the same pinned shell. `./scripts/test-container` remains
available as a separate Docker runner. See [regression coverage](docs/regression-coverage.md).

The client/daemon protocol has an explicit version. A mismatched running daemon
must be stopped with its matching binary before attaching with the new version.
Stopping the daemon ends running pane programs; saved layout and history are
restored into fresh shells. Installing a binary does not restart a daemon.

### Saved state

Mux keeps durable state in `$XDG_STATE_HOME/mux`, or `$HOME/.local/state/mux`
when `XDG_STATE_HOME` is unset. This is event-driven rather than timer-based:

- Session names, roots, selected windows, pane layouts, active panes, the last
  selected pane across all sessions and terminal tabs, pane dimensions, and
  last observed working directories are committed atomically whenever they
  change.
- Every PTY output chunk and resize is appended to a framed pane journal.
  The writer flushes buffered records after at most 8 ms of activity and requests
  a disk sync every second while dirty, and on clean shutdown. Output still in
  transit or awaiting a completed sync can be lost on machine failure; there is
  no fixed byte bound. A partially written final record is discarded on recovery.
- Journals initially become eligible for compaction at 4 MiB, retaining up to
  20,000 terminal rows with their formatting. The threshold grows when the
  compacted content itself is large. Replacement and disk syncing run on the
  journal worker, in order with subsequent output.
- Starting the daemon rebuilds every session, window, and pane, replays the
  journals to restore scrollback and terminal formatting, then starts a fresh
  shell in each pane's last observed directory. A default attach returns to the
  last selected pane. If the journal ended at an untouched prompt, the new
  prompt replaces it instead of adding a duplicate below it.

Layout saves are coalesced while idle and submitted at least every 250 ms during
continuous activity. The state writer keeps only the latest pending layout,
with a 50 ms debounce and a one-second maximum debounce period. Storage delays
can extend completion times. Write failures are reported without ending shells.

PTY input runs on a separate bounded writer. PTY output keeps a per-pane 2 MiB
reservation through its journal write, so slow storage applies backpressure to
the producing pane. Scrollback backing space is reused once expired rows and
their snapshots release it.

State remains after `mux stop`, an unexpected daemon exit, logout, or reboot.
Process memory cannot be reconstructed: commands that were running when the
daemon ended are replaced by a fresh interactive shell, while their terminal
output remains in the pane's scrollback.

For the inspected Zsh setup, mux uses a small runtime-only startup file while a
shell is loading. The shell sees `MUX` while sourcing `.zshrc`, so its automatic
mux attachment does not recurse.

## Default bindings

Normal mode:

| Key | Behavior |
| --- | --- |
| `Alt-a` | Enter leader mode for one command |
| `Alt-s` | Open the session tree |
| `Alt-c` | Open the theme picker |
| `Alt-t` | Create a window at the session root |
| `Alt-Shift-t` | Create and switch to a new session, rooted at the current shell directory |
| `Alt-Shift-r` | Set the current session root to the current shell directory |
| `Alt-1` … `Alt-9` | Select window 1 … 9 |
| `Alt-w` | Enter scrollback/copy mode, called Vim mode |
| `Alt-d` | Enter Vim mode already asking which character to jump to |
| `Alt-f` | Enter focus mode: hide mux's sidebar, show the active pane, and pass every other key directly to it; press `Alt-f` again to leave |

Leader mode lists its available commands in a bordered popup along the bottom of
the screen, from the moment leader is pressed:

| Key after `Alt-a` | Behavior |
| --- | --- |
| `$` | Rename the current session in a centered editor; arrows, `Home`, `End`, `Backspace`, and `Delete` edit, `Enter` accepts, `Escape` cancels, and `Ctrl-u` clears |
| `,` | Name the current window; an empty name gives it back to the program's title |
| `-` | Split the active pane top/bottom |
| `\|` | Split the active pane left/right |
| `!` | Move the active pane into a window of its own |
| `<`, `>` | Move the current window one place along the strip |
| `b` | Jump to the first pending bell, including its session and pane |
| `x` | Kill the active pane after confirmation |
| `d` | Detach this client while its sessions keep running |
| `Alt-a` | Send the leader key to the active pane |
| Arrow keys | Focus the pane in that direction |
| `Ctrl` + arrow keys | Move the divider next to the active pane; leader stays held so this repeats |

The session tree takes over the full terminal, with a compact panel on the left
and a live, event-driven pane preview on the right. It opens folded and initially
shows sessions only.
`j`/`Down` and `k`/`Up` move; `l`/`Right` unfolds; `h`/`Left` folds; `Space`
toggles a session; `Enter` chooses; and `Escape` closes the tree. Expanding a
session shows its root path, windows, and panes. A selected session previews all
of its windows in a live tiled overview; selecting a window or pane shows that
specific terminal. The overview keeps short previews top-aligned and draws
separators between window tiles. Each preview title bar uses the same active,
inactive, or animated bell background as that session's window strip. `x`
kills the selected item's session after confirmation.
The preview header also shows the selected session root. `Alt-a` opens leader
mode from the tree too; `$` then renames the selected session and `,` the
selected window.
Preview contents update directly as PTY output arrives, without a polling
interval, and each crop follows the newest content or cursor row instead of an
empty physical screen bottom. The tree initially selects the current session.
Visible rows 1 through 9 are selected directly with `1` through `9`, row 10
uses `0`, and rows 11 through 35 use `Alt-b` through `Alt-z`. The shortcut is
shown on each row.

The theme picker is a dialog rather than a screen: a card centred over the panes
it was opened from, only as big as it needs to be, so the work underneath stays
in view. The installed themes run along the top as a strip of tabs, each wearing
a few of its own colours. Every colour in the card comes from the highlighted
theme rather than the one in use. Moving along the strip previews that theme
across mux's own UI, including the sidebar and dividers behind the dialog.

Every colour defined by `~/nix/dotfiles/themes/palettes.nix` is shown as an
explicit labelled swatch, using the same role names as that source. This
includes the background and surface roles as well as text, accents, status,
selection, and diff colours.

`h`/`l`, `Left`/`Right`, `j`/`k`, and `Tab`/`Shift-Tab` all walk the strip, `1`
through `9` jump straight to a theme, `Enter` applies the highlighted one, and
`Escape` or `q` closes the picker. The theme in use is marked with `●`. A strip
too long for one row wraps onto as many as it needs.

Themes come from `$XDG_CONFIG_HOME/theme/themes`, one directory per theme, each
holding a `mux.toml`; a directory without one is not offered.
The theme in use is read from the `current` link beside them. Applying a theme
runs `theme NAME`, which is what switches every program on the machine and sends
mux a `set-theme` of its own, so mux does not recolour itself ahead of the rest
of the desktop. `theme_command` and `theme_directory` change both halves of that
arrangement.

Vim mode accepts counts and provides:

| Keys | Behavior |
| --- | --- |
| `h`, `j`, `k`, `l` | Character/row movement |
| `Ctrl-d`, `Ctrl-u` | Half-page down/up |
| `w`, `W`, `e`, `E`, `b`, `B` | Word and WORD movement |
| `0`, `^`, `$` | Start, first nonblank, and end of line |
| `gg`, `G` | First/last line; a count selects that numbered line |
| `f`, `F`, `t`, `T` + character | Find/till on the current line |
| `;`, `,` | Repeat the last find in the same/opposite direction |
| `Space`, character, hint | Label visible matches and jump directly to one; overflow targets use two or more hint keys |
| `Ctrl-o`, `Ctrl-l`, `Tab` | Move to older/newer positions in the pane-local jump list |
| `Alt-1` … `Alt-9` | Switch windows without leaving Vim mode; selecting the current window returns to the previous one |
| `/`, `?` | Forward/backward search; `Enter` accepts and `Escape` cancels |
| `n`, `N` | Repeat the last search in the same/opposite direction |
| `v`, `V`, `Ctrl-v` | Character, line, and block selection |
| `y`, `yy`, `Y` | Yank a selection or motion, whole line(s), or to the end of the line |
| `Escape` | Clear an active selection; a subsequent `Escape` leaves Vim mode |

Counts work with motions, find/search repeats, selections, `yy`, and
yank-with-motion. Yanking runs the configured clipboard command with the
selected text on standard input. Over SSH, mux instead sends an OSC 52 clipboard
write through the attached client so it reaches the local terminal. A yank then
leaves Vim mode.

OSC 52 clipboard writes produced inside a pane are relayed through attached mux
clients, so copying continues to work through nested mux and SSH sessions.

Vim state is pane-local. Switching panes or windows keeps each inactive pane at
its current viewport and restores its cursor, selection, search, and jump state
when it becomes active again.

`/` and `?` highlight every match of the pattern as it is typed, not only the
one the cursor jumps to. The match under the cursor is amber; the others keep
their terminal colors under a muted violet. `Escape` clears the highlight, and a
second `Escape` leaves Vim mode.

Leader help, Vim prompts, confirmations, rename input, and transient status or
error messages all use the same bordered popup instead of replacing a line of
terminal content. Anything that asks a question or takes input — confirmations,
rename input, Vim's search prompt — holds the middle of the screen. Anything
that only reports, meaning the leader help and transient messages such as
`yanked 42 bytes`, sits flush with the bottom rows instead, where it reads as
chrome rather than as an interruption. Transient messages stay for 1.6 seconds,
or until the next key, rather than for exactly one frame.

## Configuration

Configuration is TOML with one table per mode. Each entry maps a key to an
action. Built-in bindings are loaded first; user entries replace the action for
the same key and mode. Use `"unbind"` to remove one explicitly.

```toml
theme = "/home/j/.config/theme/current/mux.toml"
clipboard_command = ["yank"]
theme_command = ["theme"]
theme_directory = "/home/j/.config/theme/themes"
mouse = false
bell_style = "shimmer"
default_cursor_shape = "bar"

[normal]
"Alt-s" = "unbind"
"Alt-x" = "session-tree"
"Alt-q" = "detach"

[leader]
"v" = "split-vertical"

[vim]
"§" = "first-nonblank"
"Ctrl-d" = "half-page-down-center"

[tree]
"1" = "unbind"
"Alt-1" = "tree-select-1"

[themes]
"g" = "theme-select-1"
```

Key names use character keys or `Enter`, `Escape`, `Backspace`, `Tab`, `Up`,
`Down`, `Left`, `Right`, `Home`, `End`, `Delete`, `Insert`, `PageUp`,
`PageDown`, or `F1` through `F12`. Prefix modifiers with `Ctrl-`, `Alt-`, or `Shift-`. Character case
is meaningful: `w` and `W` are distinct.

Available normal actions are `session-tree`, `new-window`, `new-session`,
`set-session-root`, `select-window-1` through `select-window-9`, `enter-vim`,
`focus-mode`, `leader`, and `detach`.

Available leader actions are `rename-session`, `rename-window`,
`split-horizontal`, `split-vertical`, `focus-pane-left`, `focus-pane-down`,
`focus-pane-up`, `focus-pane-right`, `resize-pane-left`, `resize-pane-down`,
`resize-pane-up`, `resize-pane-right`, `break-pane`,
`swap-window-left`, `swap-window-right`, `jump-to-bell`, `kill-pane`, `detach`,
`leader-cancel`, and `theme-picker`.

Available tree actions are `tree-down`, `tree-up`, `tree-choose`, `tree-cancel`,
`tree-expand`, `tree-collapse`, `tree-toggle`, `kill-session`, and
`tree-select-1` through `tree-select-35`.

Available theme actions, in the `[themes]` table, are `theme-next`,
`theme-previous`, `theme-choose`, `theme-cancel`, `theme-picker`, and
`theme-select-1` through `theme-select-9`. `theme-picker` also belongs in normal, leader, or tree mode,
which is where the picker is opened from.

Available Vim actions are `left`, `down`, `up`, `right`, `half-page-down`,
`half-page-up`, `half-page-down-center`, `half-page-up-center`,
`word-forward`, `big-word-forward`, `word-end`, `big-word-end`,
`word-backward`, `big-word-backward`, `line-start`, `first-nonblank`,
`line-end`, `go-top`, `go-bottom`, `find-forward`, `find-backward`,
`till-forward`, `till-backward`, `repeat-find-forward`,
`repeat-find-backward`, `search-forward`, `search-backward`, `repeat-search`,
`repeat-search-reverse`, `jump-character`, `visual`, `visual-line`,
`jump-older`, `jump-newer`,
`visual-block`, `yank`, `yank-to-line-end`, `escape`, and the preset-oriented fixed
motions `up-3`, `down-3`, `up-10`, and `down-10`.

A theme file is the palette and nothing else. It says what its colours are, not
what they are for, because every program on the machine is themed from the same
roles and only mux knows where mux puts them:

```toml
variant = "dark"

[palette]
background = "#241e2d"
foreground = "#ece7f2"
surface = "#2e2739"
surface_raised = "#4a4158"
muted = "#968aa6"
accent = "#9fa8f2"
secondary = "#cba3d2"
success = "#8fd0a0"
warning = "#e3b46b"
danger = "#f28ca0"
selection = "#3f3552"
diff_add = "#2c4434"
diff_delete = "#4d2b38"
diff_change = "#4a4030"
```

Anything left out keeps its built-in value. `variant` decides what mux writes on
a saturated fill — a dark theme uses its own `background`, a light one white —
which is the one thing a palette cannot say in colours alone.

mux maps the palette to its own interface: the window strip idles in
`surface_raised` and takes `secondary` for the current window, with a
`accent` underlay while Vim mode is active; dividers, search
highlights, and pane edges are `surface_raised`; panels and popups sit on
`surface` in `foreground` with `muted` headings; the selected row and a Vim
selection are `selection`; popup borders are `success` and anything asking a
question is `warning`; and a bell rings in `accent`, shimmering towards
`foreground`. Point `theme` at a file to use it, or override roles in the
`[palette]` table of the config itself.

`mouse` is off by default, leaving click-to-select and scrollback to the
terminal emulator. Turning it on hands mux the mouse: clicking a pane focuses
it, clicking the strip selects that window, and the wheel scrolls the pane's
history in Vim mode, leaving it again at the bottom. Programs that ask for
mouse reporting — Vim, `less`, `htop` — receive the events themselves, in the
encoding they requested, and mux stays out of the way.

`theme_command` is what the picker runs to apply a theme, with the theme name
appended; `theme_directory` is where it looks for themes, defaulting to
`$XDG_CONFIG_HOME/theme/themes`. Both point at the `theme` command by default,
which owns the switch for every program on the machine.

`bell_style` is `"shimmer"` by default, the animation described above.
`"steady"` keeps the same colors but stops the motion: a pending bell rests on
the bright end of the cycle and stays there until its window is selected.
Nothing is animating, so the daemon sends no frames at all while a bell waits,
which is worth having over a slow connection or while recording. `"none"` drops
the visual entirely; the bell is still recorded, so `jump-to-bell` still finds
the pane that rang it. The setting is per client, so two terminals attached to
the same session can differ.

`default_cursor_shape` is `"bar"` by default; `"block"` and `"underline"` are
also supported. This per-client setting controls the cursor until an application
requests its own shape. A cursor reset restores the configured default, including
when Neovim exits. Shapes are steady, without blinking. Mux's Vim mode uses a
block. Change the setting and reattach to apply it; shell cursor overrides are
unnecessary.

Unknown tables, keys, actions, invalid mode/action combinations, an unknown
`bell_style`, and empty clipboard or theme commands stop startup with a
contextual error.

`$XDG_CONFIG_HOME/mux/config.toml`, or `$HOME/.config/mux/config.toml` when
`XDG_CONFIG_HOME` is unset, is read automatically when it exists. Nothing is
created there; a missing file simply keeps the built-in bindings. Apply a
different file, which must exist, with:

```sh
./target/release/mux --config path/to/config.toml
```

### Neovim-derived preset

[`config/julian.toml`](config/julian.toml) is ready to use and contains only the
relevant motion changes from the inspected Neovim configuration:

- `§` and `Shift-§` for first nonblank
- swapped `;` and `,` directions
- centered `Ctrl-d` and `Ctrl-u`
- `Ctrl-Left`/`Ctrl-Right` word motions
- three-row Ctrl-arrow and ten-row Shift-arrow vertical motions

Enable it explicitly; it is never installed or copied automatically:

```sh
./target/release/mux --config ./config/julian.toml
```

Copy it to `~/.config/mux/config.toml` to have it applied on every attach.

## Current limitations

- Each split owns an independent PTY. New splits divide their space evenly and
  are moved afterwards with `resize-pane`, one divider at a time; dragging a
  divider with the mouse and general tmux command compatibility remain out of
  scope.
- Windows are named explicitly with `rename-window` or fall back to the title
  their active pane sets. Names appear in the session tree and the previews,
  not in the numbered strip, which stays as wide as its numbers.
- Scrollback keeps up to 20,000 rows per pane. Older rows are encoded in small,
  independently compressed blocks; opening Vim mode decodes only the blocks it
  reads and releases them again afterwards.
- Restored panes start new shells; foreground processes and in-memory
  application state cannot survive the daemon process ending.
- Multiple clients may attach, but a session's selected window is shared and
  the most recent resize sets its PTY size.
- Terminal emulation covers the common VT/xterm behavior supported by the
  `vt100` parser; uncommon control sequences and exotic Vim features such as
  registers, macros, and marks are not implemented.

<!-- cold-startup-results:start -->
### Cold saved-layout scale startup (hosted CI, 2026-09-30)

Actual [run 36784956350](https://github.com/any-0/mux/actions/runs/36784956350): **180 accepted process trials**, 20 per variant/scale; nine warm-ups excluded. All complete-layout, history and fresh live-shell gates passed. Runtime remains `d6dd228054231e77772bd17a412d8f0d07871835`.

| Windows × panes | mux clean restored startup, ms | tmux + persistence clean restored startup, ms | plain tmux fresh provisioning, ms |
| --- | ---: | ---: | ---: |
| 1 × 1 | 47.638 / 52.618 | 492.343 / 500.087 | 467.926 / 470.951 |
| 3 × 2 | 54.587 / 60.868 | 814.062 / 821.960 | 2384.134 / 2391.826 |
| 6 × 4 | 144.959 / 151.890 | 1698.465 / 1712.741 | 9239.081 / 9263.699 |

| Windows × panes | mux endpoint PSS, MiB | stack endpoint PSS, MiB | plain fresh provisioning endpoint PSS, MiB |
| --- | ---: | ---: | ---: |
| 1 × 1 | 8.882 / 9.050 | 8.438 / 8.488 | 8.312 / 8.316 |
| 3 × 2 | 16.339 / 16.368 | 14.611 / 14.629 | 14.641 / 14.645 |
| 6 × 4 | 39.053 / 39.928 | 36.322 / 36.369 | 36.193 / 36.197 |

Endpoint PSS includes daemon/client/shell descendants and live helpers observed just after the timed frame. It is a snapshot, not peak RAM; transient exited helpers are absent. RSS/process records are retained.

Values are median / nearest-rank p95. **Plain tmux persistence is unsupported**; fresh provisioning includes generating the matched history and controller preparation waits, and has no restore-speed ratio.

Paired groups ran sequentially on one AMD EPYC 7763 64-Core Processor hosted VM (`GitHub Actions 1000002138`, image `20260920.314.1`). Executed harness `aec786740953afa50a7064793d9b9769bfb02143`, branch candidate `c47a5301237f496df696720f069cd216242deac1`. Project Nix pin/Rust/tmux/plugins match the earlier benchmark environment; filesystem caches remain warm.

The endpoint is cold client/daemon launch to the first selected window’s complete retained viewport and fresh prompts. Every hidden window’s geometry/cwd/history and fresh shell response are independently verified after timing; this is not a timed tour of all windows. Clean saves are separate from crash recovery. No default-period crash claim is made. Additional motion and differing-size client-contention cases remain unmeasured.


[Raw manifest](benchmarks/results/hosted-ci/36784956350/manifest.json), [reproducible harness](benchmarks/cold_startup.py), and [independent auditor](benchmarks/audit_cold_startup.py).
<!-- cold-startup-results:end -->
