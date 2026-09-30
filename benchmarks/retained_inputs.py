#!/usr/bin/env python3
"""Allow retained samples only when the measurement workflow is unchanged.

Only the explicit reuse decision, re-audit step, and their step conditions are
ignored. A changed runner, command, trial count, environment or action pin forces
fresh measurements. Runtime/workload/toolchain file diffs are checked by CI too.
"""
from pathlib import Path
import re
import subprocess
import sys

WORKFLOW = '.github/workflows/benchmark-interactive.yml'


def normalized(text):
    for name in ('Require fresh measurements when benchmark inputs change',
                 'Re-audit retained complete measurements after documentation changes'):
        text = re.sub(r'(?m)^      - name: '+re.escape(name)+r'\n(?:^(?!      - ).*\n)*', '', text)
    text = re.sub(r"(?m)^        if: steps\.evidence\.outputs\.needed == 'true'\n", '', text)
    text = re.sub(r"(?m)^        if: steps\.evidence\.outputs\.needed == 'true' && \((.*)\)$", r'        if: \1', text)
    return text


def unchanged(revision):
    original = subprocess.check_output(['git', 'show', revision+':'+WORKFLOW], text=True)
    return normalized(original) == normalized(Path(WORKFLOW).read_text())


if __name__ == '__main__':
    raise SystemExit(0 if unchanged(sys.argv[1]) else 1)
