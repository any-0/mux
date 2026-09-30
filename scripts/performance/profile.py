#!/usr/bin/env python3
"""Diagnostic run of the unchanged pinned interactive workload.

Profilers perturb timing. These samples are evidence about paths, never a
before/after performance comparison. Only mux is profiled; gates are unchanged.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

p = argparse.ArgumentParser()
p.add_argument('--harness', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--windows', type=int, choices=(1, 3, 6), required=True)
p.add_argument('--mode', choices=('perf', 'strace'), required=True)
p.add_argument('--load', choices=('idle', 'busy'), required=True)
a = p.parse_args()
sys.path.insert(0, str(a.harness.resolve() / 'benchmarks'))
import interactive

a.output = a.output.resolve()
a.output.mkdir(parents=True, exist_ok=False)
profilers = []
original = interactive.Interactive.action


def action(self, *args, **kwargs):
    if not profilers:
        metadata = {'server_pid': self.server, 'client_pid': self.client.process.pid,
                    'source_commit': interactive.SOURCE_COMMIT,
                    'harness_commit': subprocess.check_output(
                        ['git', '-C', str(a.harness), 'rev-parse', 'HEAD'], text=True).strip(),
                    'diagnostic_only': True, 'mode': a.mode,
                    'realtime_ns': time.time_ns(), 'monotonic_ns': time.perf_counter_ns()}
        (a.output / 'profile-environment.json').write_text(json.dumps(metadata, indent=2))
        perf = os.environ['PERF_BINARY']
        cmd = (['sudo', perf, 'record', '-F', '199', '-g', '--call-graph', 'dwarf',
                '-o', str(a.output / 'perf.data'),
                '-p', f'{self.server},{self.client.process.pid}'] if a.mode == 'perf' else
               ['sudo', 'strace', '-f', '-ttt', '-T', '-o', str(a.output / 'strace.log'),
                '-p', str(self.server), '-p', str(self.client.process.pid)])
        log = (a.output / (a.mode + '.log')).open('w')
        profilers.append((subprocess.Popen(cmd, stdout=log, stderr=log), log))
    return original(self, *args, **kwargs)


interactive.Interactive.action = action
try:
    sample = interactive.exercise('mux', 0, a.windows, {1: 1, 3: 2, 6: 4}[a.windows],
                                  a.load, a.output, 1000)
    (a.output / 'result.json').write_text(json.dumps(sample, indent=2))
    if not sample['correct']:
        raise SystemExit('Unchanged workload correctness gate failed')
finally:
    for proc, log in profilers:
        if proc.poll() is None:
            subprocess.run(['sudo', 'kill', '-INT', str(proc.pid)], check=False)
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        log.close()
    subprocess.run(['sudo', 'chown', '-R', str(os.getuid()), str(a.output)], check=True)
