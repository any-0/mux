#!/usr/bin/env python3
"""FIFO-gated asynchronous output, reader suspension, and action-owned yanks."""
import argparse
import json
import os
import re
from pathlib import Path
import shlex
import signal
import sys
import time

from input_sessions import file_equals
from run import Session, write_json
from transport import client_transport


def gate(session, path, value, timeout=10):
    deadline = time.monotonic() + timeout
    while not path.exists() or path.read_text() != value:
        session.pump(.005)
        assert time.monotonic() < deadline, f'producer gate {value!r} timed out'
    session.action('producer-ack', expected=value)


def run(binary, directory, shell):
    # clipboard bytes are script-owned, not read from mux's terminal snapshot.
    clipboard = directory / 'clipboard.txt'
    writer = directory / 'clipboard.py'
    directory.mkdir(parents=True)
    writer.write_text('import pathlib,sys\npathlib.Path(sys.argv[1]).write_bytes(sys.stdin.buffer.read())\n')
    config = 'clipboard_command = ' + json.dumps([sys.executable, str(writer), str(clipboard)]) + '\n'
    session = Session(binary, directory, shell, extra_config=config)
    try:
        fifo, ack = directory / 'work/control', directory / 'work/ack'
        os.mkfifo(fifo)
        producer = directory / 'work/producer.py'
        producer.write_text('''import os,pathlib,sys
control,ack=sys.argv[1:]
with open(control) as commands:
 for command in commands:
  n=int(command)
  # Full-screen fresh styled output prevents a repeated identical frame from
  # making a suspended-reader test vacuous. Unicode includes combining/wide.
  rows=[f"{n:04d}:{r:02d} 界 e\\u0301 " + chr(65+(n+r)%26)*100 for r in range(64)]
  payload="\\x1b[H\\x1b[2J\\x1b[1;2;4:3;58:2::17:34:51m"+"\\r\\n".join("\\x1b[38;2;"+str((n+r)%256)+";73;119m"+row for r,row in enumerate(rows))+"\\x1b[0m\\r\\n"
  os.write(1,payload.encode())
  pathlib.Path(ack).write_text(str(n))
''')
        session.input((f'{shlex.quote(sys.executable)} producer.py control ack &\r').encode())
        session.settle()
        control = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
        try:
            # Background output arrives while a real multiline shell editor
            # holds a wrapped command. Explicit redraw establishes a protocol
            # comparison boundary; file effects additionally verify input.
            for n, (rows, cols) in enumerate([(24, 85), (9, 37), (17, 61)]):
                value = f'async-input-{n}-界-e\u0301'
                command = f"printf '%s\\n' '{value}' > async-proof.txtXXX"
                session.input(command.encode())
                session.settle()
                os.write(control, f'{n}\n'.encode())
                gate(session, ack, str(n))
                session.settle()
                session.input(b'\x0c')
                session.checkpoint(f'async-editor-{n}')
                session.resize(rows, cols)
                session.checkpoint(f'async-editor-resized-{n}')
                session.input(b'\x01\x05\x7f\x7f\x7f\r')
                file_equals(session, directory / 'work/async-proof.txt', (value+'\n').encode())
                session.checkpoint(f'async-input-effect-{n}')
            session.resize(48, 125)
            session.checkpoint('before-stall')
            stalled = session.attach('stalled')
            session.checkpoint('both-live')
            os.kill(stalled['process'].pid, signal.SIGSTOP)
            stalled['paused'] = True
            session.action('client-stop', name=stalled['name'])
            started = time.monotonic()
            baseline = client_transport(stalled['process'].pid, session.daemon.pid)
            session.action('transport-before', **baseline)
            n = 3
            def pulse():
                nonlocal n
                old_offset = session.clients[0]['offset']
                os.write(control, f'{n}\n'.encode())
                gate(session, ack, str(n))
                while session.clients[0]['offset'] == old_offset:
                    session.pump(.005)
                    assert time.monotonic()-started < 4, 'active client stalled before transport saturation'
                n += 1
            while True:
                pulse()
                measured = client_transport(stalled['process'].pid, session.daemon.pid)
                session.action('transport-measurement', generation=n-1, **measured)
                if measured['saturated']:
                    break
                assert time.monotonic()-started < 3, 'transport never saturated within suspension budget'
            session.checkpoint('active-at-saturation')
            full = client_transport(stalled['process'].pid, session.daemon.pid)
            assert full['saturated'] and full['receiver']['receive_bytes'] > 0
            # Hold the same full receive queue while distinct new frames and
            # an action-owned input effect continue through the active client.
            for _ in range(8):
                pulse()
            session.input(b"\x0cprintf '%s\\n' 'live-under-pressure' > pressure-proof.txt\r")
            file_equals(session, directory / 'work/pressure-proof.txt', b'live-under-pressure\n')
            session.checkpoint('active-during-stall')
            after = client_transport(stalled['process'].pid, session.daemon.pid)
            assert after['saturated'], 'sender left saturation while reader remained stopped'
            assert after['receiver']['receive_bytes'] == full['receiver']['receive_bytes'], 'stopped receive queue did not plateau'
            session.action('transport-plateau', before=full, after=after,
                           active_bytes=session.clients[0]['offset'], additional_frames=8)
            os.kill(stalled['process'].pid, signal.SIGCONT)
            stalled['paused'] = False
            session.action('client-continue', name=stalled['name'], elapsed=time.monotonic()-started)
            session.checkpoint('stalled-caught-up')
            drain_deadline = time.monotonic()+5
            while True:
                drained = client_transport(stalled['process'].pid, session.daemon.pid)
                if drained['receiver']['receive_bytes'] == 0 and drained['sender']['send_memory'] == 0:
                    break
                session.pump(.005)
                assert time.monotonic() < drain_deadline, 'resumed transport did not drain'
            session.action('transport-drained', **drained)
        finally:
            os.close(control)
            if 'stalled' in locals():
                os.kill(stalled['process'].pid, signal.SIGCONT)
                stalled['paused'] = False
        session.settle()
        # Include long logical lines that rewrap around a short unique target.
        # The selected short line has no width-dependent trailing space policy.
        target = 'COPY-TARGET-界-e\u0301'
        fixture = directory / 'work/copy-fixture.txt'
        style = '\x1b[1;2;3;38;2;18;52;86;48;2;52;86;120;4:3;58;2;171;205;239m'
        fixture.write_text(style + ('LONG-STYLE-' + 'R'*180 + '\n')*30
                           + 'ANCHOR-STYLE-界\n\x1b[0m' + target + '\n' + style
                           + 'ANCHOR-STYLE-界\n' + ('LONG-STYLE-' + 'R'*240 + '\n')*30 + '\x1b[0m')
        session.input(b'cat copy-fixture.txt\r')
        session.settle()
        for n, (rows, cols) in enumerate([(12, 45), (31, 107), (9, 37)]):
            clipboard.unlink(missing_ok=True)
            session.input(b'\x1bw')
            session.settle()
            # Resize inside copy mode without Session.resize's shell Ctrl-L,
            # which means a jump-list command in this mode.
            import fcntl, struct, termios
            session.rows, session.cols = rows, cols
            session.action('resize', rows=rows, cols=cols)
            for client in session.clients:
                client.update(rows=rows, cols=cols)
                client['events'].write(json.dumps({'event':'resize','offset':client['offset'],'rows':rows,'cols':cols,'action_index':session.action_index-1})+'\n')
                fcntl.ioctl(client['fd'], termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
                client['terminal'].resize(rows, cols)
            session.settle()
            session.input(b'gg/COPY-TARGET-\r')
            session.settle()
            # Copy viewport geometry has no portable terminal reflow spec.
            # Preserve a reliable independent invariant instead: unselected
            # fixture cells retain the explicitly assigned SGR attributes even
            # when their long logical line has wrapped/reflowed. Expectations
            # are constants from the fixture, not captured mux/source state.
            cells = session.clients[0]['terminal'].snapshot(session.bar, cols-session.bar)['cells']
            styled = []
            anchors = 0
            for row in cells:
                text = ''.join(cell[0] or ' ' for cell in row)
                if 'ANCHOR-STYLE-' in text:
                    begin = text.index('ANCHOR-STYLE-')
                    styled.extend(row[begin:begin+13])
                    anchors += 1
                for match in re.finditer(r'R{4,}', text):
                    styled.extend(row[match.start():match.end()])
            assert anchors and len(styled) > 20, 'copy viewport did not expose styled fixture anchors and wrapped runs'
            wanted = ['123456','345678',True,True,True,False,3,'abcdef']
            assert all(cell[1:] == wanted for cell in styled), 'copy/reflow changed fixture SGR attributes'
            session.action('copy-style-invariant', checked_cells=len(styled), expected_attributes=wanted)
            session.input(b'0yy')
            file_equals(session, clipboard, (target+'\n').encode())
            session.action('copy-reflow-effect', expected_hex=(target+'\n').encode().hex(), rows=rows, cols=cols)
            session.input(b'\x0c')
            session.checkpoint(f'after-copy-reflow-{n}')
        return {'shell':shell,'passed':True,'checkpoints':session.checkpoints,
                'limits':['SIGSTOP reader; UNIX_DIAG proves sender buffer saturation and receive-queue plateau; internal writer channel occupancy is not measured.',
                          'Copy mode checks action-owned clipboard bytes across resize; copy viewport styles are not emulated.']}
    finally:
        session.close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--shell', choices=['bash','zsh','fish'], required=True)
    args = parser.parse_args()
    write_json(args.output / 'summary.json', run(args.binary.resolve(), args.output.resolve(), args.shell))
