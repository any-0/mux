"""Action-owned full-frame and file-effect checks, independent of mux metadata."""
import json
import os
from pathlib import Path
import shlex
import time
from run import ROOT, write_json


def await_file(session, path, expected=None):
    deadline = time.monotonic()+5
    while True:
        session.pump()
        if path.exists():
            value = json.loads(path.read_text())
            if expected is None or value == expected:
                return value
        assert time.monotonic() < deadline, f'pane input routing/file barrier: {path.name} expected {expected!r}'


def launch(session, identity):
    base = session.root/'work'/('probe-'+identity)
    command = f'python3 {shlex.quote(str(ROOT/"stress/pane_probe.py"))} {identity} {shlex.quote(str(base))}'
    session.input(command.encode()+b'\r')
    await_file(session, base.with_suffix('.geometry.json'))
    await_file(session, base.with_suffix('.input.json'), '')
    session.settle()
    return base


def cell(identity, column):
    # Literal fixture contract: three independently specified formatting runs.
    styles = ((False, False, False, 0, 'default'),
              (True, True, False, 3, '0d61d3'),
              (False, False, True, 2, 'default'))
    bold, faint, italic, underline, color = styles[column % 3]
    return [identity, '1361d3', '1f2f3f', bold, faint, italic, False, underline, color]


def rectangle(session, identity, left, width, active=True):
    actual = session.clients[0]['terminal'].snapshot(left, width)
    expected = [[cell(identity, x) for x in range(width)] for _ in range(session.rows)]
    write_json(session.root/f'pane-{identity}-{left}-{width}-actual.json', actual)
    assert actual['cells'] == expected, f'pane visible rectangle {identity}: cells/styles differ at width {width}'
    geometry = await_file(session, (session.root/'work'/('probe-'+identity)).with_suffix('.geometry.json'))
    assert geometry == dict(rows=session.rows, cols=width), f'pane {identity} PTY geometry differs from visible rectangle'
    if active:
        assert actual['cursor'] == [1,2] and not actual['hidden'] and actual['cursor_shape']=='underline', f'pane {identity} cursor differs'
    session.action('pane-rectangle', identity=identity, left=left, width=width, rows=session.rows,
                   cols=session.cols, active=active, client=session.clients[0]['name'],
                   client_offset=session.clients[0]['offset'])


def focus(session, count, active):
    base = launch(session, 'F')
    session.input(b'\x1bf')
    session.settle()
    rectangle(session, 'F', 0, session.cols)
    session.input(b'\x1ba')
    await_file(session, base.with_suffix('.input.json'), b'\x1ba'.hex())
    rectangle(session, 'F', 0, session.cols)
    session.input(b'\x1bf')
    session.settle()
    session.sidebar(count, active)
    rectangle(session, 'F', session.bar, session.cols-session.bar)
    session.input(b'\x04')
    session.settle()


def split(session):
    a = launch(session, 'A')
    session.command('split-window', '-h')
    b = launch(session, 'B')
    snapshot = session.clients[0]['terminal'].snapshot()
    row = snapshot['cells'][0]
    a_columns = [x for x in range(session.bar,session.cols) if row[x][0]=='A']
    b_columns = [x for x in range(session.bar,session.cols) if row[x][0]=='B']
    assert a_columns and b_columns, 'split visible rectangles: both PTY contents must appear'
    left_width, right_width = len(a_columns), len(b_columns)
    assert a_columns == list(range(session.bar, session.bar+left_width)) and b_columns == list(range(session.cols-right_width,session.cols)), 'split rectangles must be contiguous left/right'
    assert left_width+right_width == session.cols-session.bar-1 and abs(left_width-right_width)<=1, 'split must divide available space evenly with one separator'
    rectangle(session, 'A', session.bar, left_width, active=False)
    rectangle(session, 'B', session.cols-right_width, right_width)
    # Deliberate routing mutations are armed only after both fixtures started.
    mutation = os.environ.get('STRESS_ROUTE_MUTATION_FILE')
    if mutation:
        Path(mutation).touch()
    session.input(b'right-unique')
    await_file(session, b.with_suffix('.input.json'), b'right-unique'.hex())
    assert json.loads(a.with_suffix('.input.json').read_text()) == '', 'pane input routing leaked right input into left pane'
    session.command('select-pane','-L')
    session.input(b'left-unique')
    await_file(session, a.with_suffix('.input.json'), b'left-unique'.hex())
    assert json.loads(b.with_suffix('.input.json').read_text()) == b'right-unique'.hex(), 'pane input routing leaked left input into right pane'
    session.settle()
    rectangle(session, 'A', session.bar, left_width)
    session.command('select-pane','-R')
    session.command('kill-pane')
    session.settle()
    rectangle(session, 'A', session.bar, session.cols-session.bar)
    session.input(b'\x04')
    session.settle()
