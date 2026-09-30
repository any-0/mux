#!/usr/bin/env python3
"""Retain cold samples only when workload/toolchain and measurement workflow match."""
from pathlib import Path
import re
import subprocess
import sys
from retained_inputs import flake_unchanged

WORKFLOW='.github/workflows/benchmark-cold-startup.yml'


def normalized(text):
    for name in ('Reuse audited cold startup when inputs match', 'Re-audit retained cold startup'):
        text=re.sub(r'(?m)^      - name: '+re.escape(name)+r'\n(?:^(?!      - ).*\n)*','',text)
    text=re.sub(r"(?m)^        if: steps\.evidence\.outputs\.needed == 'true'\n",'',text)
    text=re.sub(r"(?m)^        if: steps\.evidence\.outputs\.needed == 'true' && \((.*)\)$",r'        if: \1',text)
    return text


def unchanged(revision):
    original=subprocess.check_output(['git','show',revision+':'+WORKFLOW],text=True)
    return normalized(original)==normalized(Path(WORKFLOW).read_text()) and flake_unchanged(revision)

if __name__=='__main__':raise SystemExit(0 if unchanged(sys.argv[1]) else 1)
