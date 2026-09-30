#!/usr/bin/env python3
"""Identical bounded ASCII workload, generated inside each pane's shell."""
import argparse
import os

p = argparse.ArgumentParser()
p.add_argument('--rows', type=int, default=10000)
a = p.parse_args()
for i in range(a.rows):
    # 80 visible columns: no wraps at the required 100-column pane width.
    text = f'ROW{i:08d} ' + ('x' * 68)
    os.write(1, (text + '\r\n').encode())
os.write(1, b'BENCH_OUTPUT_DONE\r\n')
