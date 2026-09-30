#!/usr/bin/env python3
"""Independent raw-data audit, adapted from PR #3's audit_interactive.py.

Checks paired roles instead of the original three multiplexer variants. Timing
endpoints, raw input accounting, history and geometry checks stay the same.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import random
import re
import statistics

p = argparse.ArgumentParser()
p.add_argument('dataset', type=Path)
a = p.parse_args()
root = a.dataset
manifest = json.loads((root / 'environment.json').read_text())
assert manifest['nixpkgs'] == '2fc6539b481e1d2569f25f8799236694180c0993'
assert manifest['harness_revision'] == '406ae9a514ab7e16084f2e623324592c2f491f9a'
assert manifest['revisions']['baseline'] == 'd6dd228054231e77772bd17a412d8f0d07871835'
assert manifest['arguments']['kind'] == 'interactive'
trials = manifest['arguments']['trials']
assert trials >= 20
raw = list(root.glob('*/*/sample.json'))
assert len(raw) == 2 * (trials + 1)
summary = json.loads((root / 'summary.json').read_text())
checksums = json.loads((root / 'sha256.json').read_text())
for name, digest in checksums.items():
    assert hashlib.sha256((root / name).read_bytes()).hexdigest() == digest, name
rng = random.Random(manifest['seed'])
last_end = 0
geometry = names = None
samples = {role: [] for role in ('baseline', 'candidate')}
binaries = {}
for n in range(trials + 1):
    roles = ['baseline', 'candidate']
    rng.shuffle(roles)
    for role in roles:
        directory = root / f'{n:03d}-{role}'
        runtime = json.loads((directory / 'runtime.json').read_text())
        assert runtime['revision'] == manifest['revisions'][role]
        binaries.setdefault(role, runtime['binary_sha256'])
        assert binaries[role] == runtime['binary_sha256']
        paths = list(directory.glob('*/sample.json'))
        assert len(paths) == 1
        sample = json.loads(paths[0].read_text())
        assert sample['correct'] and sample['trial'] == n and sample['variant'] == 'mux'
        assert sample['windows'] == manifest['arguments']['windows']
        assert sample['load'] == manifest['arguments']['load']
        assert sample['started_ns'] > last_end
        last_end = sample['ended_ns']
        assert sample['history_rows_per_pane'] == 1000
        current = [sorted((p.get('benchmark_index', p['index']), p['cols'], p['rows'],
                           p['history_sha256']) for p in w['panes']) for w in sample['fixture']]
        if geometry is None:
            geometry = current
        assert geometry == current
        for window in sample['fixture']:
            for pane in window['panes']:
                slot = pane.get('benchmark_index', pane['index'])
                label = f'W{window["window"]:02d}P{slot:02d}'
                found = re.findall(label + r'-H\d{5} x{12}',
                                   (paths[0].parent / (label + '-history.txt')).read_text())
                assert found == [f'{label}-H{i:05d} ' + 'x'*12 for i in range(1000)]
                assert hashlib.sha256('\n'.join(found).encode()).hexdigest() == pane['history_sha256']
                count = sample['panes_per_window']
                position = ((18 if slot <= 2 else 39), (0 if slot % 2 else 50)) if count == 4 else ((18 if slot == 1 else 39), 0) if count == 2 else (39, 0)
                assert tuple(pane['visible_prompt_position']) == position
        observed = [m['name'] for m in sample['measurements']]
        if names is None:
            names = observed
        assert observed == names
        inputs = {}
        for path in paths[0].parent.glob('*/input.json'):
            inputs.update({e['input_ns']: e['hex'] for e in json.loads(path.read_text())})
        for measurement in sample['measurements']:
            assert measurement['correct']
            assert measurement['latency_ms'] == (measurement['decoded_ns']-measurement['input_ns'])/1e6 > 0
            assert sample['started_ns'] <= measurement['input_ns'] < measurement['decoded_ns'] <= sample['ended_ns']
            if 'input_hex' in measurement:
                assert inputs[measurement['input_ns']] == measurement['input_hex']
        for resource in [sample['resource_before']] + [m['post_resource'] for m in sample['measurements'] if 'post_resource' in m]:
            assert resource['pss_kib'] == sum(p['Pss'] for p in resource['processes'])
            assert resource['rss_kib'] == sum(p['Rss'] for p in resource['processes'])
        cpu = sample['profile_cpu']
        assert cpu['window_seconds'] >= 3
        assert cpu['percent_one_cpu_lower_bound'] == 100*(cpu['ticks_after']-cpu['ticks_before'])/cpu['clock_ticks_per_second']/cpu['window_seconds']
        if n:
            samples[role].append(sample)
for role in samples:
    for name in names:
        values = sorted(next(m['latency_ms'] for m in s['measurements'] if m['name'] == name) for s in samples[role])
        reported = summary[role]['latency_ms'][name]
        assert reported['values'] == values and reported['n'] == trials
        assert reported['median'] == statistics.median(values)
        assert reported['p95_nearest_rank'] == values[math.ceil(.95*trials)-1]
print(json.dumps({'audit': 'passed', 'measured_trials': 2*trials,
                  'warmups_retained_and_excluded': 2, 'operations_per_trial': len(names),
                  'binary_sha256': binaries}, indent=2))
