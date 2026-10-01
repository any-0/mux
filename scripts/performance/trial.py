#!/usr/bin/env python3
"""Execute one unchanged PR #3 workload with an explicitly verified runtime."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys

p = argparse.ArgumentParser()
p.add_argument('--harness', type=Path, required=True)
p.add_argument('--source', type=Path, required=True)
p.add_argument('--revision', required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--trial', type=int, required=True)
p.add_argument('--windows', type=int, choices=(1, 3, 6), required=True)
p.add_argument('--load', choices=('idle', 'busy'), required=True)
p.add_argument('--kind', choices=('interactive', 'micro', 'recovery'), default='interactive')
p.add_argument('--variant', choices=('mux', 'tmux'), default='mux')
a = p.parse_args()
assert os.environ.get('BENCH_NIXPKGS_REV'), 'Use the pinned benchmark Nix shell'
for tool in ('rustc', 'cargo', 'python3', 'tmux', 'bash'):
    assert str(Path(shutil.which(tool)).resolve()).startswith('/nix/store/'), tool
assert not subprocess.check_output(['git', '-C', str(a.harness), 'diff', 'HEAD', '--', 'benchmarks'])
source = a.source.resolve()
assert subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip() == a.revision
assert not subprocess.check_output(['git', '-C', str(source), 'diff', 'HEAD', '--',
                                   'src', 'vendor', 'Cargo.toml', 'Cargo.lock'])
os.environ['BENCH_MUX_SOURCE_DIR'] = str(source)
sys.path.insert(0, str(a.harness.resolve() / 'benchmarks'))
import interactive

a.output = a.output.resolve()
a.output.mkdir(parents=True, exist_ok=False)
if a.kind == 'interactive':
    sample = interactive.exercise(a.variant, a.trial, a.windows,
                                  {1: 1, 3: 2, 6: 4}[a.windows], a.load, a.output, 1000)
elif a.kind == 'micro':
    from run import trial
    sample = trial(a.variant, a.trial, a.output,
                   argparse.Namespace(rows=10000, scroll_samples=50, idle_seconds=3))
else:
    from recovery import recovery_trial
    sample = recovery_trial(a.variant, a.trial, 'clean', 0.0, a.output)
(a.output / 'runtime.json').write_text(json.dumps({'revision': a.revision,
    'binary_sha256': __import__('hashlib').sha256((source / 'target/release/mux').read_bytes()).hexdigest(),
    'nixpkgs': os.environ['BENCH_NIXPKGS_REV']}, indent=2))
if not sample['correct']:
    raise SystemExit(sample.get('error', 'Correctness gate failed'))
