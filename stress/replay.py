#!/usr/bin/env python3
"""Replay saved checkpoints without mux, shells, wall-clock timing, or PTYs."""
import argparse
import json
from pathlib import Path
from oracle import Terminal


def pane_terminal(path, offset):
    terminal = Terminal(24, 80)
    for line in path.read_bytes()[:offset].splitlines():
        event = json.loads(line)
        if event['event'] == 'resize':
            terminal.resize(event['rows'], event['cols'])
        elif event['event'] == 'output':
            terminal.feed(bytes.fromhex(event['hex']))
    return terminal


def client_terminal(root, name, offset):
    terminal = Terminal(24, 85)
    data = (root / f'{name}.ansi').read_bytes()
    position = 0
    for line in (root / f'{name}.events.jsonl').read_text().splitlines():
        event = json.loads(line)
        if event['offset'] > offset:
            break
        terminal.feed(data[position:event['offset']])
        position = event['offset']
        terminal.resize(event['rows'], event['cols'])
    terminal.feed(data[position:offset])
    return terminal


def replay(root):
    results = []
    for line in (root / 'actions.jsonl').read_text().splitlines():
        event = json.loads(line)
        if event['kind'] != 'checkpoint':
            continue
        expected = pane_terminal(root / 'capture' / event['capture_file'], event['capture_offset']).snapshot()
        for client in event['clients']:
            actual = client_terminal(root, client['name'], client['offset']).snapshot(event['bar'], event['cols'] - event['bar'])
            results.append({'checkpoint': event['name'], 'client': client['name'], 'passed': actual == expected})
    return results


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('profile_directory', type=Path)
    args = parser.parse_args()
    results = replay(args.profile_directory)
    print(json.dumps(results, indent=2))
    raise SystemExit(0 if results and all(r['passed'] for r in results) else 1)
