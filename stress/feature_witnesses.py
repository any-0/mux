#!/usr/bin/env python3
"""Quick independently observable cases for deliberate mutation sensitivity."""
import argparse
from pathlib import Path
from run import Session,write_json
from pane_checks import focus,split
from output_workloads import large_fixture,output_command
from sync_sessions import synchronized


def run(binary,directory,case):
    session=Session(binary,directory,'bash',proxied=case in ('output','sync'))
    try:
        if case=='focus':focus(session,1,0)
        elif case=='split':split(session)
        elif case=='sync':synchronized(session)
        else:
            body=large_fixture()
            (directory/'work/large.txt').write_bytes(body)
            output_command(session,'cat large.txt',body,'large',0)
            session.checkpoint('output-complete')
        write_json(directory/'summary.json',dict(case=case,passed=True))
    finally:
        session.close()


if __name__=='__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--case',choices=['focus','split','output','sync'],required=True)
    args=parser.parse_args()
    args.output.mkdir(parents=True,exist_ok=False)
    run(args.binary.resolve(),args.output,args.case)
