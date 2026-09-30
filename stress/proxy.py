#!/usr/bin/env python3
"""Transparent shell PTY recorder. Inner bytes, never mux state, feed oracle."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import sys
import termios
import tty

root = Path(os.environ['STRESS_CAPTURE'])
root.mkdir(parents=True, exist_ok=True)
master, slave = pty.openpty()
log = (root / f'{os.getpid()}.jsonl').open('w', buffering=1)

def record(event, **data):
    log.write(json.dumps({'event': event, **data}) + '\n')

def resize(*_):
    size = fcntl.ioctl(0, termios.TIOCGWINSZ, b'\0' * 8)
    fcntl.ioctl(master, termios.TIOCSWINSZ, size)
    import struct
    rows, cols, _, _ = struct.unpack('HHHH', size)
    record('resize', rows=rows, cols=cols)

resize()
pid = os.fork()
if pid == 0:
    os.setsid()
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
    for fd in (0, 1, 2):
        os.dup2(slave, fd)
    os.close(master)
    os.close(slave)
    shell = os.environ['STRESS_REAL_SHELL']
    arguments = {'bash': ['--noprofile', '--rcfile', os.environ['STRESS_RC'], '-i'],
                 'zsh': ['-i'], 'fish': ['--interactive']}[Path(shell).name]
    os.execv(shell, [shell, *arguments])
os.close(slave)
# Signal handlers must not write the buffered capture log: SIGWINCH can
# interrupt record() and Python rejects the reentrant TextIO write. Wake the
# ordinary event loop instead, preserving one serial order for resize/output.
signal_read, signal_write = os.pipe2(os.O_NONBLOCK | os.O_CLOEXEC)
signal.set_wakeup_fd(signal_write)
signal.signal(signal.SIGWINCH, lambda *_: None)
def stop(signum, _frame):
    raise SystemExit(128 + signum)

for signum in (signal.SIGHUP, signal.SIGTERM, signal.SIGINT):
    signal.signal(signum, stop)
old = termios.tcgetattr(0)
tty.setraw(0)
try:
    while True:
        ready, _, _ = select.select([0, master, signal_read], [], [])
        if signal_read in ready:
            pending = os.read(signal_read, 4096)
            if signal.SIGWINCH in pending:
                resize()
        if master in ready:
            try:
                data = os.read(master, 65536)
            except OSError:
                break
            if not data:
                break
            record('output', hex=data.hex())
            view = memoryview(data)
            while view:
                view = view[os.write(1, view):]
        if 0 in ready:
            data = os.read(0, 65536)
            if not data:
                break
            record('input', hex=data.hex())
            view = memoryview(data)
            while view:
                view = view[os.write(master, view):]
finally:
    signal.set_wakeup_fd(-1)
    os.close(signal_read)
    os.close(signal_write)
    termios.tcsetattr(0, termios.TCSANOW, old)
    try:
        os.killpg(pid, signal.SIGHUP)
    except ProcessLookupError:
        pass
    os.waitpid(pid, 0)
