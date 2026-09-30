"""Aggregation rules shared with recovery tests; never synthesize samples."""
import math
import statistics


def distribution(values):
    ordered = sorted(values)
    if not ordered:
        return None
    return {'n': len(ordered), 'median': statistics.median(ordered),
            'p95': ordered[math.ceil(0.95 * len(ordered)) - 1]}


def summarize_recovery(samples):
    groups = {}
    for sample in samples:
        if sample['trial'] == 0:
            continue
        key = f"{sample['mode']}/{sample['age_fraction']:.2f}/{sample['variant']}"
        groups.setdefault(key, []).append(sample)
    result = {}
    for key, group in groups.items():
        failures = [s for s in group if not s['correct']]
        summary = {'trials': len(group), 'failed_trials': len(failures),
                   'recovery_supported': group[0]['recovery_supported'],
                   'fidelity_failures': [{'trial': s['trial'], 'error': s.get('error'),
                                         'metadata_correct': s.get('metadata_correct'),
                                         'formatting_correct': s.get('formatting_correct'),
                                         'lost_rows': s.get('lost_rows')} for s in failures],
                   'observed_loss_rows': distribution([s['lost_rows'] for s in group if s.get('lost_rows') is not None]),
                   'observed_loss_tagged_utf8_bytes': distribution([s['lost_tagged_utf8_bytes'] for s in group if s.get('lost_tagged_utf8_bytes') is not None]),
                   'loss_samples_missing': sum(s.get('lost_rows') is None for s in group)}
        if failures:
            summary['timings_blocked'] = 'at least one correctness gate failed; no surviving-trial timing aggregate'
        elif not summary['recovery_supported']:
            summary['restore'] = 'unsupported; no successful-restore time reported'
        else:
            timings = {}
            for metric in ('save_ms', 'save_shutdown_ms', 'stop_ms', 'restart_attach_live_ms',
                           'restore_script_ms', 'verification_ms', 'snapshot_age_at_crash_seconds'):
                values = [s[metric] for s in group if s.get(metric) is not None]
                if values:
                    timings[metric] = distribution(values)
            summary['timings'] = timings
        result[key] = summary
    return result
