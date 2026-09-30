#!/usr/bin/env python3
"""Real foreground PTY fixture: repaint on SIGWINCH and record routed bytes."""
import json
import os
from pathlib import Path
import select
import signal
import sys
import termios
import tty

identity, destination = sys.argv[1:]
root = Path(destination)
old = termios.tcgetattr(0)
tty.setraw(0)
reader, writer = os.pipe()
os.set_blocking(writer, False)
signal.set_wakeup_fd(writer)
signal.signal(signal.SIGWINCH, lambda *_: None)
received = b''


def save(name, value):
    temporary = root.with_suffix('.'+name+'.tmp')
    temporary.write_text(json.dumps(value))
    temporary.replace(root.with_suffix('.'+name+'.json'))


def repaint():
    cols, rows = os.get_terminal_size(0)
    payload = '\x1b[?7l\x1b[2J\x1b[0m'
    for y in range(rows):
        payload += f'\x1b[{y+1};1H'
        for x in range(cols):
            payload += ('\x1b[0;38;2;19;97;211;48;2;31;47;63m',
                        '\x1b[0;1;2;4:3;58:2::13:97:211;38;2;19;97;211;48;2;31;47;63m',
                        '\x1b[0;3;4:2;38;2;19;97;211;48;2;31;47;63m')[x % 3] + identity
    payload += '\x1b[0m\x1b[?25h\x1b[4 q\x1b[2;3H'
    os.write(1, payload.encode())
    save('geometry', dict(rows=rows, cols=cols))


try:
    repaint()
    save('input', received.hex())
    while True:
        ready, _, _ = select.select([0, reader], [], [])
        if reader in ready:
            os.read(reader, 4096)
            repaint()
        if 0 in ready:
            data = os.read(0, 4096)
            if b'\x04' in data:
                break
            received += data
            save('input', received.hex())
finally:
    termios.tcsetattr(0, termios.TCSANOW, old)
    os.write(1, b'\x1b[0m\x1b[?7h\x1b[2J\x1b[H')
