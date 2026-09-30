#!/usr/bin/env python3
"""Matched, randomized baseline/candidate blocks on one runner.

Delegates every operation and gate unchanged to a pinned external harness.
Warm-up trial 0 is retained and excluded. No timing uses a profiler.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import statistics
import subprocess
import sys

p = argparse.ArgumentParser()
p.add_argument('--harness', type=Path, required=True)
p.add_argument('--baseline', type=Path, required=True)
p.add_argument('--candidate', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--windows', type=int, choices=(1, 3, 6), required=True)
p.add_argument('--load', choices=('idle', 'busy'), required=True)
p.add_argument('--kind', choices=('interactive', 'micro'), default='interactive')
p.add_argument('--trials', type=int, default=20)
a = p.parse_args()
assert a.trials >= 20
assert os.environ.get('BENCH_NIXPKGS_REV')
a.output = a.output.resolve()
a.output.mkdir(parents=True, exist_ok=False)
roles = {'baseline': a.baseline.resolve(), 'candidate': a.candidate.resolve()}
revisions = {role: subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip()
             for role, source in roles.items()}
manifest = {'revisions': revisions, 'harness_revision': subprocess.check_output(
    ['git', '-C', str(a.harness), 'rev-parse', 'HEAD'], text=True).strip(),
    'nixpkgs': os.environ['BENCH_NIXPKGS_REV'], 'seed': 20260930,
    'warmup': 0, 'history_rows_per_pane': 1000 if a.kind == 'interactive' else 10000,
    'runner': {key: os.environ.get(key) for key in ('RUNNER_NAME', 'ImageVersion', 'GITHUB_RUN_ID')},
    'versions': {tool: subprocess.check_output([tool, '--version'], text=True).strip()
                 for tool in ('rustc', 'cargo', 'python3')},
    'cpuinfo': Path('/proc/cpuinfo').read_text(), 'arguments': vars(a) | {
        key: str(value) for key, value in vars(a).items() if isinstance(value, Path)}}
(a.output / 'environment.json').write_text(json.dumps(manifest, indent=2))
rng = random.Random(20260930)
samples = {role: [] for role in roles}
fixture = None
for trial in range(a.trials + 1):
    order = list(roles)
    rng.shuffle(order)
    for role in order:
        destination = a.output / f'{trial:03d}-{role}'
        subprocess.run([sys.executable, str(Path(__file__).with_name('trial.py')),
            '--harness', str(a.harness), '--source', str(roles[role]),
            '--revision', revisions[role], '--output', str(destination),
            '--windows', str(a.windows), '--load', a.load, '--trial', str(trial), '--kind', a.kind], check=True)
        files = list(destination.rglob('sample.json'))
        assert len(files) == 1
        sample = json.loads(files[0].read_text())
        assert sample['correct']
        if a.kind == 'interactive':
            current = [[sorted((pane.get('benchmark_index', pane['index']), pane['cols'], pane['rows'],
                                 pane['history_sha256']) for pane in window['panes'])]
                       for window in sample['fixture']]
            if fixture is None:
                fixture = current
            assert current == fixture, ('Unequal fixture', trial, role)
            assert sample['history_rows_per_pane'] == 1000
            assert all(m['correct'] for m in sample['measurements'])
        samples[role].append(sample)
        print(trial, role, 'visible gates and complete seed history PASS', flush=True)


def distribution(values):
    values = sorted(values)
    return {'n': len(values), 'min': values[0], 'median': statistics.median(values),
            'p95_nearest_rank': values[__import__('math').ceil(.95 * len(values)) - 1],
            'max': values[-1], 'mean': statistics.mean(values), 'values': values}


summary = {}
for role, rows in samples.items():
    measured = rows[1:]
    if a.kind == 'interactive':
        names = {m['name'] for s in measured for m in s['measurements']}
        summary[role] = {'latency_ms': {name: distribution([
            m['latency_ms'] for s in measured for m in s['measurements'] if m['name'] == name])
            for name in sorted(names)},
            'initial_pss_kib': distribution([s['resource_before']['pss_kib'] for s in measured]),
            'profile_percent_one_cpu_lower_bound': distribution([
                s['profile_cpu']['percent_one_cpu_lower_bound'] for s in measured])}
    else:
        summary[role] = {'latency_ms': {name: distribution([s[name] for s in measured])
            for name in ('startup_ms', 'output_ms')},
            'scroll_trial_median_ms': distribution([statistics.median(s['scroll_ms']) for s in measured]),
            'pss_kib': distribution([s['pss_kib'] for s in measured]),
            'output_cpu_seconds_lower_bound': distribution([s['output_cpu_seconds'] for s in measured]),
            'idle_cpu_percent_lower_bound': distribution([s['idle_cpu_percent'] for s in measured])}
assert summary['baseline']['latency_ms'].keys() == summary['candidate']['latency_ms'].keys()
paired = {}
for name in sorted(summary['baseline']['latency_ms']):
    def latencies(role):
        return [(next(m['latency_ms'] for m in sample['measurements'] if m['name'] == name)
                 if a.kind == 'interactive' else sample[name])
                for sample in samples[role] if sample['trial'] > 0]
    before, after = latencies('baseline'), latencies('candidate')
    deltas = [candidate - baseline for baseline, candidate in zip(before, after)]
    bootstrap_rng = random.Random(20260930)
    bootstrap = sorted(statistics.median(bootstrap_rng.choices(deltas, k=len(deltas)))
                       for _ in range(10000))
    paired[name] = {'candidate_minus_baseline_ms': distribution(deltas),
                    'median_delta_bootstrap_95ci_ms': [bootstrap[249], bootstrap[9749]],
                    'ratio_of_medians': statistics.median(after) / statistics.median(before)}
summary['paired'] = paired
(a.output / 'summary.json').write_text(json.dumps(summary, indent=2))
checksums = {str(path.relative_to(a.output)): hashlib.sha256(path.read_bytes()).hexdigest()
             for path in sorted(a.output.rglob('*')) if path.is_file()}
(a.output / 'sha256.json').write_text(json.dumps(checksums, indent=2))
