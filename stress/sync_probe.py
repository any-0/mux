#!/usr/bin/env python3
"""FIFO-gated synchronized batch, with real DECRQM acknowledgement barrier."""
import os
from pathlib import Path
import select
import sys
import termios
import time
import tty

root = Path(sys.argv[1])
old = termios.tcgetattr(0)
tty.setraw(0)
fifo = os.open(root/'sync.fifo', os.O_RDWR)


def response(stage):
    os.write(1,b'\x1b[?2026$p')
    data = b''
    deadline = time.monotonic()+5
    while not data.endswith(b'$y'):
        assert time.monotonic() < deadline, 'fixture DECRQM response missing'
        ready,_,_ = select.select([0],[],[],.1)
        if ready:
            data += os.read(0,4096)
    temporary=root/'sync-response.tmp'
    temporary.write_bytes(data)
    temporary.replace(root/('sync-'+stage+'.response'))


try:
    os.write(1,b'\x1bc\x1b[2;3H\x1b[1;38;2;19;97;211mBASE\x1b[0m\x1b[4;6H\x1b[4 q')
    (root/'sync-base.ready').touch()
    while True:
        command=os.read(fifo,1)
        if command==b'B':
            os.write(1,b'\x1b[?2026h\x1b[2J\x1b[3;7H\x1b[1;2;3;4:3;58:2::13:97:211;38;2;90;170;40mNEXT\x1b[0m\x1b[7;11H\x1b[6 q')
            response('begin')
        elif command==b'R':
            os.write(1,b'\x1b[?2026h')
            response('repeat')
        elif command==b'E':
            os.write(1,b'\x1b[?2026l')
            response('end')
        elif command==b'X':
            break
finally:
    termios.tcsetattr(0,termios.TCSANOW,old)
