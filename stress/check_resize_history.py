#!/usr/bin/env python3
"""Execute the new regression and faithful PTY witness on exact pre-fix source."""
import argparse
import hashlib
from pathlib import Path
import shutil
import subprocess
import sys
from run import ROOT,write_json

BASE='01eaa380a7b46e0e68546a555614d50a25b87106'


def check(output):
    output.mkdir(parents=True,exist_ok=False)
    checkout=output/'baseline-source'
    target=output.parent/'resize-history-baseline-target'
    subprocess.run(['git','worktree','add','--detach',str(checkout),BASE],cwd=ROOT,check=True)
    try:
        # This file differs from the baseline only by the new regression test.
        shutil.copyfile(ROOT/'src/server/tests.rs',checkout/'src/server/tests.rs')
        test='server::tests::shrinking_a_pane_keeps_output_above_the_idle_cursor_in_history'
        with (output/'baseline-regression.log').open('wb') as log:
            result=subprocess.run(['cargo','test','--locked','--target-dir',str(target),test,'--','--exact','--nocapture'],cwd=checkout,stdout=log,stderr=subprocess.STDOUT,timeout=180)
        text=(output/'baseline-regression.log').read_text()
        assert result.returncode!=0 and 'test result: FAILED. 0 passed; 1 failed' in text and 'left: ["first", "second"]' in text, 'baseline must reach the intended six-line preservation assertion'
        subprocess.run(['cargo','build','--locked','--target-dir',str(target)],cwd=checkout,check=True,timeout=180)
        binary=target/'debug/mux'
        with (output/'baseline-pty.log').open('wb') as log:
            subprocess.run([sys.executable,str(ROOT/'stress/resize_history.py'),'--binary',str(binary),'--output',str(output/'baseline-pty'),'--expect-baseline-loss'],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=90)
        write_json(output/'baseline-provenance.json',dict(source_commit=BASE,binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),test_result='expected six-line preservation failure',pty_result='30 -> 18 -> 18'))
    finally:
        subprocess.run(['git','worktree','remove','--force',str(checkout)],cwd=ROOT,check=True)
    with (output/'fixed-pty.log').open('wb') as log:
        subprocess.run([sys.executable,str(ROOT/'stress/resize_history.py'),'--binary',str(ROOT/'target/debug/mux'),'--output',str(output/'fixed-pty')],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=90)


if __name__=='__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('--output',type=Path,required=True)
    check(parser.parse_args().output.resolve())
