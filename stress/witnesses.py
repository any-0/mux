#!/usr/bin/env python3
"""Small real-PTY reproductions distilled from stress failures."""
import argparse
import json
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


def sidebar(binary, directory):
    session = Session(binary, directory, 'bash', proxied=False)
    try:
        for count in range(2, 11):
            session.command('new-window')
            session.bar = len(str(count)) + 4
            session.settle()
        session.sidebar(10, 9)
    finally:
        session.close()


def intended_intensity_failure(directory):
    path = directory/'witness-styled-intensity-diff.json'
    if not path.exists():
        return False
    errors = json.loads(path.read_text())
    if len(errors) != 1 or errors[0]['total_cell_differences'] != 4:
        return False
    error = errors[0]
    for field in ('cursor','hidden','shape'):
        if error.get('expected_'+field) != error.get('actual_'+field):
            return False
    cells = error['cells']
    if len(cells) != 4:
        return False
    for x, (letter, cell) in enumerate(zip('BOTH',cells)):
        expected = [letter,'default','default',True,True,False,False,3,'00d7ff']
        actual = list(expected)
        actual[3] = False
        if cell != {'row':0,'col':x,'expected':expected,'actual':actual}:
            return False
    return True


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--case', choices=['intensity', 'sidebar'], default='intensity')
    parser.add_argument('--expect-baseline-failure', action='store_true')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    failed = False
    try:
        {'intensity': intensity, 'sidebar': sidebar}[args.case](args.binary.resolve(), args.output)
    except AssertionError as error:
        # A setup timeout, EOF or unrelated harness issue is not evidence.
        intended = intended_intensity_failure(args.output) if args.case == 'intensity' else 'sidebar icon tile background' in str(error)
        if not intended:
            raise
        failed = True
        write_json(args.output / 'witness-result.json', {'failed': True, 'error': str(error)})
        print(error)
    assert failed == args.expect_baseline_failure, 'witness outcome did not match requested baseline/fixed expectation'
