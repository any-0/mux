#!/usr/bin/env python3
"""Render the complete accepted follow-up sweep from independently audited raw ZIPs."""
import argparse
import hashlib
import json
from pathlib import Path
import tempfile
import zipfile

from audit_interactive import audit
from audit_mux_ui import audit as audit_ui

VARIANTS = ('mux', 'tmux', 'tmux-persistence')
GROUPS = ('w1-idle', 'w1-busy', 'w3-idle', 'w3-busy', 'w6-idle', 'w6-busy')


def cell(value, divisor=1):
    return f"{value['median']/divisor:.3f} / {value['p95']/divisor:.3f}"


def table(comparisons, group, operations):
    w, load = group.split('-')
    panes = {'w1': 1, 'w3': 2, 'w6': 4}[w]
    rows = ['| Operation (ms) | mux | tmux | tmux + persistence |',
            '| --- | ---: | ---: | ---: |']
    for name in operations:
        label = name.replace('_', ' ')
        if name == 'populated_attach_startup':
            label = 'Populated client attach (existing daemon)'
        elif name == 'join_pane_command_to_viewport':
            label = 'Join pane (CLI dispatch → viewport)'
        elif name == 'yank_line_clipboard_receipt':
            label = 'Yank line (clipboard receipt upper bound)'
        elif name == 'detach_client_exit':
            label = 'Detach (process exit)'
        values = [cell(comparisons[f'{w}-p{panes}/{load}/{v}']['operations'][name]) for v in VARIANTS]
        rows.append('| ' + ' | '.join([label] + values) + ' |')
    return '\n'.join(rows)


def render(dataset):
    manifest = json.loads((dataset/'manifest.json').read_text())
    assert manifest['status'] == 'completed', 'Do not render an incomplete or failed sweep as complete'
    comparisons = {}
    environments = []
    with tempfile.TemporaryDirectory(prefix='mux-render-audit-') as tmp:
        for group in (*GROUPS, 'mux-ui'):
            archive = dataset/f'{group}-raw.zip'
            assert hashlib.sha256(archive.read_bytes()).hexdigest() == manifest['artifacts'][group]['sha256']
            root = Path(tmp)/group
            with zipfile.ZipFile(archive) as z:
                z.extractall(root)
            root = root/('mux-ui-performance' if group == 'mux-ui' else f'interactive-{group}')
            result = json.loads(json.dumps(audit_ui(root) if group == 'mux-ui' else audit(root)))
            assert result == json.loads((dataset/f'{group}-audit.json').read_text())
            assert result == json.loads((root/'audit.json').read_text())
            environments.append(json.loads((root/'environment.json').read_text()))
            if group == 'mux-ui':
                ui = result
            else:
                assert result['measured_trials'] == 60 and result['operations_per_trial'] == 53
                comparisons.update(result['comparisons'])
    assert len({e['source_commit'] for e in environments}) == 1
    assert len({e['harness_commit'] for e in environments}) == 1
    assert len({e['runner']['GITHUB_RUN_ID'] for e in environments}) == 1
    assert len({e['runner']['RUNNER_NAME'] for e in environments}) == 1
    assert len({e['runner']['ImageVersion'] for e in environments}) == 1
    env = environments[0]
    run = env['runner']['GITHUB_RUN_ID']
    cpu = next(line.split(':', 1)[1].strip() for line in env['cpuinfo'].splitlines() if line.startswith('model name'))
    logical_cpus = sum(line.startswith('processor') for line in env['cpuinfo'].splitlines())
    memory_kib = env['meminfo'].splitlines()[0].split()[1]
    intro = f'''### Complete expanded interactive sweep (hosted CI, 2026-09-30)

[Run {run}](https://github.com/any-0/mux/actions/runs/{run}): **360 accepted paired
trials**, 20 per variant in each of six groups, **53 endpoints per trial**;
18 warm-ups excluded. A separate **20-trial, 12-endpoint mux-only UI suite** also
passed (one warm-up excluded). These are real attached-PTY measurements, not
diagnostic smoke timings. All trials, including warm-ups, passed visible-result
gates. Paired groups pass independent fixture/input/resource audits; mux-only UI
passes complete input/timestamp/visible-transition/resource audits. The corrected logical pane
mapping makes all four-pane content/geometry slots equal across variants.

Measured mux: `{env['source_commit']}`. Executed harness:
`{env['harness_commit']}` (branch candidate `{manifest['branch_head']}`).
Project Nix pin `{env['nixpkgs']}`, Rust/Cargo 1.93.0, tmux 3.6a,
Bash 5.3.9, Python 3.13.12. Resurrect/continuum pins and loaded-plugin checks are
documented above. No PR #4 code is mixed into this runtime. All groups ran
sequentially on the same hosted VM; variant order is shuffled within each block.
Each declared group resets the documented shuffle seed. Exact runner hardware,
image, commands, dependency versions and raw input/output are retained in each ZIP.
The VM reported {cpu}, {logical_cpus} logical CPUs, {memory_kib} KiB RAM,
runner image `{env['runner']['ImageVersion']}`.

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
Restored-layout scale coverage and default-period crash recovery remain unmeasured.
'''
    resources = []
    for metric, title, divisor in [('pss_kib', 'Initial PSS (MiB)', 1024),
                                    ('rss_kib', 'Initial RSS (MiB)', 1024),
                                    ('profile_cpu_percent_one_cpu_lower_bound', 'Profile CPU (% of one CPU; lower bound)', 1),
                                    ('observed_state_logical_bytes', 'Observed state logical bytes (different durability)', 1),
                                    ('observed_state_allocated_bytes', 'Observed state allocated bytes (different durability)', 1)]:
        rows = [f'#### {title}', '', '| Seeded layout / load | mux | tmux | tmux + persistence |', '| --- | ---: | ---: | ---: |']
        for group in GROUPS:
            w, load = group.split('-'); panes = {'w1': 1, 'w3': 2, 'w6': 4}[w]
            values = [cell(comparisons[f'{w}-p{panes}/{load}/{v}'][metric], divisor) for v in VARIANTS]
            rows.append('| ' + ' | '.join([f'{w[1:]}×{panes} {load}'] + values) + ' |')
        resources.append('\n'.join(rows))
    first = comparisons['w1-p1/idle/mux']['operations']
    operations = list(first)
    assert len(operations) == 53
    assert all(set(v['operations']) == set(operations) for v in comparisons.values())
    full = intro + '\n' + '\n\n'.join(resources)
    for group in GROUPS:
        full += f'\n\n<details>\n<summary>{group}: all 53 endpoints</summary>\n\n' + table(comparisons, group, operations) + '\n\n</details>'
    ui_table = ['#### Mux-only UI (no tmux comparison)', '', '| Operation (ms) | mux median / p95 |', '| --- | ---: |']
    for name, values in ui['operations'].items():
        ui_table.append(f'| {name.replace("_", " ")} | {cell(values)} |')
    full += '\n\n' + '\n'.join(ui_table)
    full += f'''\n\nRaw ZIPs, independent audits and artifact SHA-256 values:
[manifest](results/hosted-ci/{run}/manifest.json). To regenerate these tables,
run `./scripts/benchmark-nix python3 benchmarks/render_interactive_results.py
benchmarks/results/hosted-ci/{run} --summary /tmp/mux-summary.md --full /tmp/mux-full.md`.
The renderer re-audits every raw trial before emitting any table.
'''
    summary = intro + '\n' + resources[0] + '\n\n' + resources[2]
    summary += '\n\n<details>\n<summary>1×1 idle: all 53 endpoints</summary>\n\n' + table(comparisons, 'w1-idle', operations) + '\n\n</details>\n'
    summary += '\n<details>\n<summary>RSS and observed state storage</summary>\n\n' + '\n\n'.join([resources[1],resources[3],resources[4]]) + '\n\n</details>\n'
    summary += f'\n[All six groups, 53 endpoints, RSS/storage and mux-only UI tables](benchmarks/README.md#complete-expanded-interactive-sweep-hosted-ci-2026-09-30), [raw manifest](benchmarks/results/hosted-ci/{run}/manifest.json), and [coverage inventory](benchmarks/COVERAGE.md).\n'
    return summary, full


if __name__ == '__main__':
    p = argparse.ArgumentParser()
    p.add_argument('dataset', type=Path)
    p.add_argument('--summary', type=Path, required=True)
    p.add_argument('--full', type=Path, required=True)
    args = p.parse_args()
    summary, full = render(args.dataset)
    args.summary.write_text(summary)
    args.full.write_text(full)
