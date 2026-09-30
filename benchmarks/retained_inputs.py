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


# PR4 adds this independent shell; benchmark/default definitions and lock stay exact.
STRESS_SHELL = """        stress = pkgs.mkShell {
          STRESS_NIXPKGS_REV = nixpkgs.rev;
          STRESS_RUST_VERSION = pkgs.rustc.version;
          STRESS_ASYNC_PLUGIN = "${pkgs.pure-prompt}/share/zsh/site-functions/async";
          STRESS_PURE_VERSION = pkgs.pure-prompt.version;
          packages = with pkgs; [ cargo rustc rustfmt clippy bash zsh fish vim
            coreutils git util-linux pure-prompt
            (python3.withPackages (p: [ p.pyte ])) ];
        };
"""


def flake_unchanged(revision):
    original = subprocess.check_output(['git', 'show', revision+':.nix/flake.nix'], text=True)
    current = Path('.nix/flake.nix').read_text()
    return current == original or (current.count(STRESS_SHELL) == 1 and
                                    current.replace(STRESS_SHELL, '', 1) == original)


def unchanged(revision):
    original = subprocess.check_output(['git', 'show', revision+':'+WORKFLOW], text=True)
    return (normalized(original) == normalized(Path(WORKFLOW).read_text())
            and flake_unchanged(revision))


if __name__ == '__main__':
    raise SystemExit(0 if unchanged(sys.argv[1]) else 1)
