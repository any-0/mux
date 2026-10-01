#!/usr/bin/env python3
"""Correlate raw syscall timestamps with diagnostic visible-result intervals."""
import argparse
from collections import Counter
import json
from pathlib import Path
import re

p = argparse.ArgumentParser()
p.add_argument('directory', type=Path)
a = p.parse_args()
metadata = json.loads((a.directory / 'profile-environment.json').read_text())
result = json.loads((a.directory / 'result.json').read_text())
assert metadata['diagnostic_only'] and metadata['mode'] == 'strace'
clock_offset = (metadata['realtime_ns'] - metadata['monotonic_ns']) / 1e9
trace = []
counts = Counter()
for line in (a.directory / 'strace.log').read_text().splitlines():
    m = re.match(r'\s*(\d+)\s+(\d+\.\d+)\s+(.*)', line)
    if m:
        pid, timestamp, call = int(m[1]), float(m[2]), m[3]
        trace.append((pid, timestamp, call))
        name = re.match(r'(\w+)\(', call)
        if name:
            counts[name[1]] += 1
segments = {}
for measurement in result['measurements']:
    if measurement['name'] not in ('create_window', 'split_vertical', 'switch_session',
                                   'history_search_backward_commit', 'history_search_forward_commit'):
        continue
    start = measurement['input_ns'] / 1e9 + clock_offset
    end = measurement['decoded_ns'] / 1e9 + clock_offset
    segments[measurement['name']] = [dict(pid=pid, from_input_ms=round((timestamp-start)*1000, 3), call=call)
        for pid, timestamp, call in trace if start-.0005 <= timestamp <= end+.0005]
print(json.dumps({'diagnostic_only': True, 'clock_correlation': 'single realtime/monotonic pair; allow clock-read skew',
                  'source_commit': metadata['source_commit'], 'syscall_counts': dict(counts),
                  'segments': segments}, indent=2))
