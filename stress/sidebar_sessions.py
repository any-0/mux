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


def mode(session, glyph, background, foreground):
    session.settle()
    screen = session.clients[0]['terminal'].screen
    actual = ''.join(screen.buffer[0][x].data for x in range(3))
    assert actual == glyph, f'mode glyph expected {glyph!r}, got {actual!r}'
    cell = screen.buffer[0][1]
    assert (cell.fg, cell.bg) == (foreground, background), 'mode colors differ from declared profile palette'
    session.action('mode-check', glyph=glyph, foreground=foreground, background=background)


def metadata(session, expected):
    actual = session.query('list-sessions')
    projected = {s['name']: (s['windows'], s['panes'], s['attached']) for s in actual}
    assert projected == expected, f'action-derived session metadata: expected {expected}, got {projected}'
    session.action('session-model-check', expected=expected)


def run(binary, directory, shell):
    session = Session(binary, directory, shell, proxied=False)
    try:
        (directory / 'work/sample.txt').write_text('sidebar-vim-buffer\n界 e\u0301\n')
        icon(session, SHELL_ICONS[shell])
        session.input(b'vim -Nu NONE -n sample.txt\r')
        icon(session, VIM_ICON)
        session.input(b'\x1b:q!\r')
        icon(session, SHELL_ICONS[shell])
        session.input(b'cat -vT\r')
        icon(session, '·')
        # Fish and cat share the same icon. Require output only cat -T can
        # produce before EOF, otherwise a stale fish icon permits EOF to
        # overtake process startup and leaves the following Vim command in cat.
        session.input(b'\t\r')
        session.wait_text('^I')
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
        metadata(session, {'stress': (12, 12, True)})
        session.command('new-session', '-s', 'auxiliary')
        session.command('rename-session', 'aux-renamed')
        session.command('rename-window', 'named-window')
        session.bar = 5
        session.sidebar(1, 0)
        metadata(session, {'stress': (12, 12, False), 'aux-renamed': (1, 1, True)})
        windows = session.query('list-windows')
        assert [(w['name'], w['active'], w['panes']) for w in windows] == [('named-window', True, 1)]
        session.command('split-window', '-h')
        metadata(session, {'stress': (12, 12, False), 'aux-renamed': (1, 2, True)})
        panes = session.query('list-panes')
        assert [(p['index'], p['active']) for p in panes] == [(1, False), (2, True)]
        session.command('kill-pane')
        metadata(session, {'stress': (12, 12, False), 'aux-renamed': (1, 1, True)})
        session.command('choose-tree')
        session.wait_text('aux-renamed')
        session.input(b'\x1b')
        session.sidebar(1, 0)
        mode(session, ' ● ', '334455', '778899')
        session.input(b'\x1ba')
        mode(session, ' ● ', '113355', '010203')
        session.input(b'\x1b')
        mode(session, ' ● ', '334455', '778899')
        session.command('vim-mode')
        mode(session, ' ● ', '446688', '010203')
        session.input(b'\x1b')
        mode(session, ' ● ', '334455', '778899')
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
