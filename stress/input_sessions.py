#!/usr/bin/env python3
"""Action-derived input effects, independent of pane/client output decoding."""
import argparse
from pathlib import Path
import time
from run import Session, write_json


def file_equals(session, path, expected):
    deadline = time.monotonic() + 10
    while not path.exists() or path.read_bytes() != expected:
        session.pump()
        assert time.monotonic() < deadline, f'input effect expected {expected!r}, got {path.read_bytes() if path.exists() else None!r}'
    session.action('input-effect', file=path.name, expected_hex=expected.hex())


def run(binary, directory, shell):
    session = Session(binary, directory, shell)
    try:
        proof = directory / 'work/input-proof.txt'
        for n, (rows, cols) in enumerate([(24, 85), (9, 37), (17, 61), (12, 45)]):
            # Real editor motion, deletion, UTF-8 and wrap across SIGWINCH.
            # The expected bytes come from this scripted action, not a shell
            # screen, parser dump, or output observed from the implementation.
            value = f'delivered-{n}-界-e\u0301'
            command = f"printf '%s\\n' '{value}' > input-proof.txtXXX"
            session.input(command.encode())
            session.resize(rows, cols)
            session.input(b'\x01\x05\x7f\x7f\x7f\r')
            file_equals(session, proof, (value + '\n').encode())
            session.checkpoint(f'input-edited-{n}')
        payload = ("printf '%s\\n' 'paste-first-界' > paste-proof.txt\n"
                   "printf '%s\\n' 'paste-second-e\u0301' >> paste-proof.txt")
        session.input(b'\x1b[200~' + payload.encode() + b'\x1b[201~')
        session.settle()
        assert not (directory / 'work/paste-proof.txt').exists(), 'bracketed paste executed before explicit Enter'
        session.action('paste-not-executed')
        session.input(b'\r')
        file_equals(session, directory / 'work/paste-proof.txt', 'paste-first-界\npaste-second-e\u0301\n'.encode())
        session.checkpoint('input-multiline-bracketed-paste')
        return {'shell': shell, 'passed': True}
    finally:
        session.close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--shell', choices=['bash', 'zsh', 'fish'], required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    write_json(args.output / 'summary.json', run(args.binary.resolve(), args.output, args.shell))
