#!/usr/bin/env python3
"""Small real-PTY reproductions distilled from stress failures."""
import argparse
from pathlib import Path
from run import Session, write_json


def intensity(binary, directory):
    session = Session(binary, directory, 'bash')
    try:
        # Marker is in file output only, never in command input.
        (directory / 'work/intensity.ansi').write_bytes(
            b'\x1bc\x1b[1;2;4:3;58;5;45mBOTH\x1b[22mNORMAL\x1b[0m\r\n')
        session.input(b'cat intensity.ansi\r')
        session.settle()
        session.checkpoint('witness-styled-intensity')
    finally:
        session.close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--expect-baseline-failure', action='store_true')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    failed = False
    try:
        intensity(args.binary.resolve(), args.output)
    except AssertionError as error:
        # A setup timeout, EOF or unrelated harness issue is not evidence.
        if 'witness-styled-intensity: independent cell/cursor/attribute mismatch' not in str(error):
            raise
        failed = True
        write_json(args.output / 'witness-result.json', {'failed': True, 'error': str(error)})
        print(error)
    assert failed == args.expect_baseline_failure, 'witness outcome did not match requested baseline/fixed expectation'
