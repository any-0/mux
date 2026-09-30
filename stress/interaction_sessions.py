#!/usr/bin/env python3
"""FIFO-gated asynchronous output, reader suspension, and action-owned yanks."""
import argparse
import json
import os
from pathlib import Path
import shlex
import signal
import sys
import time

from input_sessions import file_equals
from run import Session, write_json


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
  rows=[f"{n:04d}:{r:02d} 界 e\\u0301 " + chr(65+(n+r)%26)*55 for r in range(22)]
  payload="\\x1b[H\\x1b[2J\\x1b[1;2;4:3;58:2::17:34:51m"+"\\r\\n".join(rows)+"\\x1b[0m\\r\\n"
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
            command = "printf '%s\\n' 'async-input-界-e\u0301' > async-proof.txtXXX"
            for n, (rows, cols) in enumerate([(24, 85), (9, 37), (17, 61)]):
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
                file_equals(session, directory / 'work/async-proof.txt', 'async-input-界-e\u0301\n'.encode())
                session.checkpoint(f'async-input-effect-{n}')
            session.resize(24, 85)
            session.checkpoint('before-stall')
            stalled = session.attach('stalled')
            session.checkpoint('both-live')
            os.kill(stalled['process'].pid, signal.SIGSTOP)
            stalled['paused'] = True
            session.action('client-stop', name=stalled['name'])
            started = time.monotonic()
            # A bounded burst, shorter than the documented disconnect timeout.
            # SIGSTOP is a real reader suspension; active-client comparisons
            # and producer acknowledgements must continue during the stall.
            for n in range(3, 83):
                os.write(control, f'{n}\n'.encode())
                gate(session, ack, str(n))
            session.checkpoint('active-during-stall')
            os.kill(stalled['process'].pid, signal.SIGCONT)
            stalled['paused'] = False
            session.action('client-continue', name=stalled['name'], elapsed=time.monotonic()-started)
            session.checkpoint('stalled-caught-up')
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
        fixture.write_text(('prefix-' + '界x'*90 + '\n')*30 + target + '\n' + ('suffix-' + 'ab'*120 + '\n')*30)
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
            session.input(b'0yy')
            file_equals(session, clipboard, (target+'\n').encode())
            session.action('copy-reflow-effect', expected_hex=(target+'\n').encode().hex(), rows=rows, cols=cols)
            session.input(b'\x0c')
            session.checkpoint(f'after-copy-reflow-{n}')
        return {'shell':shell,'passed':True,'checkpoints':session.checkpoints,
                'limits':['Suspended reader is proven by SIGSTOP; kernel/server queue occupancy is not measured.',
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
