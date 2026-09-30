#!/usr/bin/env python3
"""Linear deterministic checkpoint replay without mux, shells, clocks, or PTYs."""
import argparse
import json
from pathlib import Path
from oracle import Terminal


class PaneReplay:
    def __init__(self):
        self.terminal = Terminal(24, 80)
        self.offsets = {}

    def advance(self, path, offset):
        previous = self.offsets.get(path, 0)
        with path.open('rb') as stream:
            stream.seek(previous)
            data = stream.read(offset - previous)
        self.offsets[path] = offset
        for line in data.splitlines():
            event = json.loads(line)
            if event['event'] == 'resize':
                self.terminal.resize(event['rows'], event['cols'])
            elif event['event'] == 'output':
                self.terminal.feed(bytes.fromhex(event['hex']))
        return self.terminal.snapshot()


class ClientReplay:
    def __init__(self, root, name):
        self.terminal = Terminal(24, 85)
        self.data = (root / f'{name}.ansi').read_bytes()
        self.events = [json.loads(line) for line in (root / f'{name}.events.jsonl').read_text().splitlines()]
        self.event_index = 0
        self.position = 0

    def advance(self, checkpoint, offset):
        while self.event_index < len(self.events):
            event = self.events[self.event_index]
            if event['offset'] > offset or event.get('action_index', -1) >= checkpoint['index']:
                break
            self.terminal.feed(self.data[self.position:event['offset']])
            self.position = event['offset']
            self.terminal.resize(event['rows'], event['cols'])
            self.event_index += 1
        self.terminal.feed(self.data[self.position:offset])
        self.position = offset
        return self.terminal.snapshot(checkpoint['bar'], checkpoint['cols'] - checkpoint['bar'])


def replay(root):
    results, clients = [], {}
    pane = PaneReplay()
    for line in (root / 'actions.jsonl').read_text().splitlines():
        event = json.loads(line)
        if event['kind'] != 'checkpoint':
            continue
        expected = pane.advance(root / 'capture' / event['capture_file'], event['capture_offset'])
        for client in event['clients']:
            name = client['name']
            if name not in clients:
                clients[name] = ClientReplay(root, name)
            actual = clients[name].advance(event, client['offset'])
            results.append({'checkpoint': event['name'], 'client': name, 'passed': actual == expected})
    return results


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('profile_directory', type=Path)
    args = parser.parse_args()
    results = replay(args.profile_directory)
    print(json.dumps(results, indent=2))
    raise SystemExit(0 if results and all(r['passed'] for r in results) else 1)
