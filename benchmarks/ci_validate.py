#!/usr/bin/env python3
"""Hosted-CI correctness smoke: diagnostics only, no performance aggregates."""
import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
from types import SimpleNamespace

from run import ROOT, SOURCE_ROOT, SOURCE_COMMIT, command, trial
from recovery import recovery_trial


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--output', required=True, type=Path)
    a = p.parse_args()
    a.output = a.output.resolve()
    a.output.mkdir(parents=True, exist_ok=False)
    if command(['git', '-C', str(SOURCE_ROOT), 'rev-parse', 'HEAD']) != SOURCE_COMMIT:
        p.error('runtime revision differs from pinned PR #2 revision')
    env = {'kind': 'hosted-ci-correctness-validation', 'performance_comparison': False,
           'source_commit': SOURCE_COMMIT, 'harness_commit': command(['git', '-C', str(ROOT), 'rev-parse', 'HEAD']),
           'nixpkgs_rev': os.environ['BENCH_NIXPKGS_REV'],
           'lock': json.loads((ROOT / '.nix/flake.lock').read_text()),
           'versions': {tool: command([tool, '-V' if tool == 'tmux' else '--version'])
                        for tool in ('rustc', 'cargo', 'python3', 'tmux', 'bash')},
           'uname': platform.uname()._asdict(),
           'runner': {key: os.environ.get(key) for key in ('RUNNER_NAME', 'RUNNER_OS', 'RUNNER_ARCH', 'ImageOS', 'ImageVersion', 'GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT')},
           'cpuinfo': Path('/proc/cpuinfo').read_text(), 'meminfo': Path('/proc/meminfo').read_text(),
           'mounts': Path('/proc/mounts').read_text(), 'cgroup': Path('/proc/self/cgroup').read_text()}
    (a.output / 'environment.json').write_text(json.dumps(env, indent=2))
    with (a.output / 'build.log').open('w') as log:
        subprocess.run(['cargo', 'build', '--locked', '--release'], cwd=SOURCE_ROOT,
                       stdout=log, stderr=subprocess.STDOUT, check=True)
    # All compilation has ended before actual interaction; variants execute
    # serially on this same runner. Trial zero is diagnostic, not a sample set.
    micro = a.output / 'micro-smoke'
    clean = a.output / 'clean-smoke'
    micro.mkdir()
    clean.mkdir()
    samples = []
    for variant in ('mux', 'tmux', 'tmux-persistence'):
        print('starting micro', variant, flush=True)
        samples.append(trial(variant, 0, micro, SimpleNamespace(rows=300, scroll_samples=2, idle_seconds=0.2)))
        print('micro', variant, samples[-1]['correct'], samples[-1].get('error'), flush=True)
    for variant in ('mux', 'tmux', 'tmux-persistence'):
        print('starting clean', variant, flush=True)
        samples.append(recovery_trial(variant, 0, 'clean', 0.0, clean))
        print('clean', variant, samples[-1]['correct'], samples[-1].get('error'), flush=True)
    gates = {'kind': 'hosted-ci-correctness-validation', 'performance_comparison': False,
             'passing': all(s['correct'] for s in samples),
             'gates': [{'variant': s['variant'], 'phase': s.get('mode', 'micro'),
                        'correct': s['correct'], 'error': s.get('error')} for s in samples]}
    (a.output / 'correctness.json').write_text(json.dumps(gates, indent=2))
    if not gates['passing']:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
