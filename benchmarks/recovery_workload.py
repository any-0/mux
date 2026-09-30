#!/usr/bin/env python3
"""Tagged UTF-8/ANSI history fixture shared by all recovery variants."""
import argparse
import os


def record(label, index):
    return f'REC{label}:{index:08d}|α漢' + 'X' * 40


def emit(label, start, count, probe=False):
    for index in range(start, start + count):
        # Color intentionally belongs to the terminal contents, not shell config.
        os.write(1, ('\x1b[31m' + record(label, index) + '\x1b[0m\r\n').encode())
    if probe:
        os.write(1, (f'WRAP{label}|' + 'Z' * 120 + '\r\n').encode())
    os.write(1, (f'REC_DONE_{label}_{start + count}\r\n').encode())


if __name__ == '__main__':
    p = argparse.ArgumentParser()
    p.add_argument('label')
    p.add_argument('--start', type=int, default=0)
    p.add_argument('--count', type=int, default=200)
    p.add_argument('--probe', action='store_true')
    a = p.parse_args()
    if not a.label.isalnum() or a.start < 0 or a.count < 1:
        p.error('invalid fixture range or label')
    emit(a.label, a.start, a.count, a.probe)
