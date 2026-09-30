#!/usr/bin/env python3
"""Unproxied foreground jobs and action-derived sidebar state across resizes."""
import argparse
from pathlib import Path
import time
from run import Session, write_json

SHELL_ICONS = {'bash': '$', 'zsh': '❯', 'fish': '·'}
VIM_ICON = '\ue01f\ue020\ue021'


def icon(session, expected):
    # One window: the three-row tile is centered in the space below the mode
    # tile. Its process line is one row below its window label.
    y = 2 + (session.rows - 5) // 2
    deadline = time.monotonic() + 5
    while True:
        session.pump()
        screen = session.clients[0]['terminal'].screen
        actual = ''.join(screen.buffer[y][x].data for x in range(3)).strip()
        if actual == expected:
            session.action('foreground-icon', expected=expected, row=y)
            return
        assert time.monotonic() < deadline, f'foreground icon expected {expected!r}, got {actual!r}'


def run(binary, directory, shell):
    session = Session(binary, directory, shell, proxied=False)
    try:
        (directory / 'work/sample.txt').write_text('sidebar-vim-buffer\n界 e\u0301\n')
        icon(session, SHELL_ICONS[shell])
        session.input(b'vim -Nu NONE -n sample.txt\r')
        icon(session, VIM_ICON)
        session.input(b'\x1b:q!\r')
        icon(session, SHELL_ICONS[shell])
        session.input(b'cat\r')
        icon(session, '·')
        session.input(b'\x04')
        icon(session, SHELL_ICONS[shell])
        session.input(b'vim -Nu NONE -n sample.txt\r')
        icon(session, VIM_ICON)
        session.input(b'\x1b\x1a')  # suspend the actual foreground job
        icon(session, SHELL_ICONS[shell])
        session.input(b'fg\r')
        icon(session, VIM_ICON)
        session.input(b'\x1b:q!\r')
        icon(session, SHELL_ICONS[shell])
        session.input(b'sleep 2 &\r')
        icon(session, SHELL_ICONS[shell])
        for count in range(2, 13):
            session.command('new-window')
            session.bar = len(str(count)) + 4
            session.sidebar(count, count - 1)
        for rows, cols in [(6, 37), (4, 20), (3, 12), (2, 8), (1, 8), (9, 45), (31, 107), (12, 61)]:
            session.resize(rows, cols)
            session.sidebar(12, 11)
        for active in [0, 8, 3, 7, 1, 5]:
            session.command('select-window', str(active + 1))
            session.sidebar(12, active)
        # Focus mode removes the sidebar; exit must restore the same model.
        session.input(b'\x1bf')
        session.settle()
        session.input(b'\x1bf')
        session.sidebar(12, 5)
        return {'shell': shell, 'passed': True}
    finally:
        session.close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--shell', choices=list(SHELL_ICONS), required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    write_json(args.output / 'summary.json', run(args.binary.resolve(), args.output, args.shell))
