#!/usr/bin/env python3
"""Independently verify a hosted paired dataset before README publication."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import random
import re
import statistics

VARIANTS = ('mux', 'tmux', 'tmux-persistence')


def stats(values):
    ordered = sorted(values)
    return {'n': len(ordered), 'median': statistics.median(ordered),
            'p95': ordered[math.ceil(len(ordered) * .95) - 1]}


def audit(root):
    micro = root / 'paired-micro'
    env = json.loads((micro / 'environment.json').read_text())
    assert env['execution_context'] == 'hosted-ci-paired-performance'
    assert env['source_commit'] == 'd6dd228054231e77772bd17a412d8f0d07871835'
    assert env['nixpkgs'] == '2fc6539b481e1d2569f25f8799236694180c0993'
    rows, count, scroll = (env['args'][k] for k in ('rows', 'trials', 'scroll_samples'))
    assert (rows, count, scroll) == (10000, 30, 20)
    samples = [json.loads(p.read_text()) for p in micro.glob('*/sample.json')]
    assert len(samples) == 3 * (count + 1)
    expected = '\n'.join(f'ROW{i:08d} ' + 'x' * 68 for i in range(rows))
    expected_hash = hashlib.sha256(expected.encode()).hexdigest()
    keyed = {(s['trial'], s['variant']): s for s in samples}
    assert len(keyed) == len(samples)
    rng = random.Random(20260930)
    last_end = 0
    for number in range(count + 1):
        order = list(VARIANTS)
        rng.shuffle(order)
        for variant in order:
            s = keyed[number, variant]
            assert s['correct'], (number, variant, s.get('error'))
            assert s['history_gate'] == {'rows': rows, 'columns': 100, 'pane_rows': 40,
                                          'sha256': expected_hash, 'expected_sha256': expected_hash}
            directory = micro / f'{number:03d}-{variant}'
            history = (directory / 'history.txt').read_text()
            found = re.findall(r'ROW\d{8} x{68}', history)
            assert '\n'.join(found) == expected and history.count('ROW') == rows
            inputs = {event['input_ns']: event['hex'] for event in json.loads((directory / 'input.json').read_text())}
            assert s['started_monotonic_ns'] > last_end
            last_end = s['ended_monotonic_ns']
            assert len(s['scroll_events']) == len(s['scroll_ms']) == scroll
            for event, latency in zip(s['scroll_events'], s['scroll_ms']):
                assert len(event['before']) == len(event['observed']) == 40
                assert event['expected'] == event['observed'] == [n - 20 for n in event['before']]
                assert latency == (event['decoded_viewport_ns'] - event['input_ns']) / 1e6
                assert latency > 0
                assert inputs[event['input_ns']] == '1b5b357e'
            assert s['pss_kib'] == sum(p['Pss'] for p in s['processes'])
            assert s['rss_kib'] == sum(p['Rss'] for p in s['processes'])
    summary = json.loads((micro / 'summary.json').read_text())
    for variant in VARIANTS:
        selected = [keyed[n, variant] for n in range(1, count + 1)]
        for metric, measured in summary[variant].items():
            values = [v for s in selected for v in (s[metric] if isinstance(s[metric], list) else [s[metric]])]
            assert measured == stats(values), (variant, metric)
    # Per-process event medians and within-block differences avoid implying
    # that the pooled 600 scroll events are 600 independent launches.
    paired = {}
    for variant in VARIANTS:
        medians = [statistics.median(keyed[n, variant]['scroll_ms']) for n in range(1, count + 1)]
        differences = [statistics.median(keyed[n, variant]['scroll_ms']) -
                       statistics.median(keyed[n, 'tmux']['scroll_ms']) for n in range(1, count + 1)]
        paired[variant] = {'trial_median_scroll_ms': stats(medians),
                           'paired_trial_median_difference_from_tmux_ms': stats(differences),
                           'paired_pss_difference_from_tmux_kib': stats([
                               keyed[n, variant]['pss_kib'] - keyed[n, 'tmux']['pss_kib']
                               for n in range(1, count + 1)])}
    clean = root / 'paired-clean'
    clean_samples = [json.loads(p.read_text()) for p in clean.glob('*/sample.json')]
    assert len(clean_samples) == 3 * (count + 1)
    clean_env = json.loads((clean / 'environment.json').read_text())
    assert clean_env['source_commit'] == env['source_commit']
    assert clean_env['execution_context'] == env['execution_context']
    failures = []
    for s in clean_samples:
        if not s['correct']:
            failures.append({'trial': s['trial'], 'variant': s['variant'], 'error': s.get('error')})
        elif s['variant'] != 'tmux':
            assert s['metadata_correct'] and s['formatting_correct'] and s['lost_rows'] == 0
            assert all(g['full_history_correct'] and g['wrap_correct'] for g in s['after_fidelity'].values())
            assert s['fresh_shells_confirmed'] == ['w1p1', 'w1p2', 'w2p1']
    clean_summary = json.loads((clean / 'summary.json').read_text())
    for variant in VARIANTS:
        group = [s for s in clean_samples if s['variant'] == variant and s['trial'] > 0]
        assert len(group) == count and sorted(s['trial'] for s in group) == list(range(1, count + 1))
        summary = clean_summary['clean/0.00/' + variant]
        assert summary['failed_trials'] == sum(not s['correct'] for s in group)
        if summary['failed_trials']:
            assert 'timings_blocked' in summary
        elif variant != 'tmux':
            for metric, values in summary['timings'].items():
                assert values == stats([s[metric] for s in group if s.get(metric) is not None])
    return {'micro_audit': 'passed', 'trials_per_variant': count, 'events_per_variant': count * scroll,
            'warmups_excluded': 3, 'paired': paired, 'clean_failures': failures,
            'clean_timing_comparison_accepted': not failures}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('dataset', type=Path)
    args = parser.parse_args()
    print(json.dumps(audit(args.dataset), indent=2))
