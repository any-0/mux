#!/usr/bin/env python3
"""Action-owned logical-history witness: real PTY resize, no repaint command."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import shlex
import struct
import sys
import termios
import time
from run import ROOT,Session,write_json


def run(binary,directory,expect_loss=False):
    directory.mkdir(parents=True,exist_ok=False)
    clipboard=directory/'clipboard.txt'
    writer=directory/'clipboard.py'
    writer.write_text('import pathlib,sys\npathlib.Path(sys.argv[1]).write_bytes(sys.stdin.buffer.read())\n')
    config='clipboard_command = '+json.dumps([sys.executable,str(writer),str(clipboard)])+'\n'
    session=Session(binary,directory,'bash',extra_config=config)
    # Independent script-owned lines, kept short enough for all tested widths.
    expected=[f'RESIZE-{i:02d}-界-e\u0301' for i in range(30)]
    producer=directory/'work/history-producer.py'
    producer.write_text('import os,pathlib,sys\nroot=pathlib.Path(sys.argv[1])\nos.write(1,("\\x1bc\\x1b[1;2;4:3;38;2;19;97;211m"+"\\r\\n".join('+repr(expected)+')+"\\x1b[0m").encode())\n(root/"history.ready").touch()\nwith open(root/"history.fifo") as gate: gate.read()\n')
    os.mkfifo(directory/'work/history.fifo')
    try:
        session.input(f'python3 history-producer.py {shlex.quote(str(directory/"work"))}'.encode()+b'\r')
        deadline=time.monotonic()+5
        while not (directory/'work/history.ready').exists():
            session.pump()
            assert time.monotonic()<deadline,'history producer not ready'
        session.settle()
        results=[]
        def yank(stage,wanted):
            clipboard.unlink(missing_ok=True)
            session.input(b'\x1bw')
            session.settle()
            session.input(b'ggVGy')
            deadline=time.monotonic()+5
            while not clipboard.exists():
                session.pump()
                assert time.monotonic()<deadline,'history clipboard completion missing'
            session.wait_text(f'yanked {len(clipboard.read_bytes())} bytes')
            data=clipboard.read_text()
            # NFC canonicalization is a Unicode representation equivalence.
            import unicodedata
            observed=re.findall(r'RESIZE-\d{2}-界-é',unicodedata.normalize('NFC',data))
            normalized=[unicodedata.normalize('NFC',x) for x in expected]
            target=normalized if not wanted else normalized[:18]
            (directory/(stage+'-clipboard.txt')).write_text(data)
            result=dict(stage=stage,rows=session.rows,cols=session.cols,observed_lines=observed,
                        expected_lines=target,client_offset=session.clients[0]['offset'])
            results.append(result)
            session.action('resize-history-invariant',**result)
            write_json(directory/'summary.json',dict(expect_baseline_loss=expect_loss,results=results))
            assert observed==target,f'resize-history {stage}: action-owned 30 lines expected {len(target)}, got {len(observed)}: {observed}'
            session.input(b'\x1b')
            session.settle()
        def resize(rows,cols):
            session.rows,session.cols=rows,cols
            session.action('resize',rows=rows,cols=cols,no_repaint=True)
            for client in session.clients:
                client.update(rows=rows,cols=cols)
                client['events'].write(json.dumps(dict(event='resize',offset=client['offset'],rows=rows,cols=cols,action_index=session.action_index-1))+'\n')
                fcntl.ioctl(client['fd'],termios.TIOCSWINSZ,struct.pack('HHHH',rows,cols,0,0))
                client['terminal'].resize(rows,cols)
            session.settle()
        yank('before',False)
        resize(12,85)
        yank('shrink',expect_loss)
        resize(24,85)
        yank('expand',expect_loss)
        if not expect_loss:
            resize(12,37)
            yank('both-axes',False)
            session.restart()
            # Restored shell prompts add rows; the action-owned markers must
            # still appear once in original order in retained history.
            yank('restart',False)
        return dict(passed=True,expect_baseline_loss=expect_loss,results=results)
    finally:
        session.close()


if __name__=='__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--expect-baseline-loss',action='store_true')
    args=parser.parse_args()
    write_json(args.output/'summary.json',run(args.binary.resolve(),args.output.resolve(),args.expect_baseline_loss))
