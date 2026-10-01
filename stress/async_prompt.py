#!/usr/bin/env python3
"""Pinned Pure/zsh-async worker, FIFO completion gates and action-owned Git state."""
import argparse
import hashlib
import os
from pathlib import Path
import shlex
import subprocess
import time

from input_sessions import file_equals
from run import ROOT, Session, write_json


class AsyncSession(Session):
    def profile(self):
        super().profile()
        work = self.root / 'work'
        self.fifo = work / 'async-release'
        os.mkfifo(self.fifo)
        def git(*args):
            subprocess.run(['git', *args], cwd=work, check=True, capture_output=True)
        git('init', '-b', 'branch-alpha')
        git('config', 'user.name', 'Mux Stress')
        git('config', 'user.email', 'stress@example.invalid')
        (work / '.gitignore').write_text('*\n!.gitignore\n!tracked.txt\n')
        (work / 'tracked.txt').write_text('initial\n')
        git('add', '.gitignore', 'tracked.txt')
        git('commit', '-m', 'deterministic fixture')
        plugin = os.environ['STRESS_ASYNC_PLUGIN']
        assert Path(plugin).is_file(), 'pinned Pure async library missing'
        (self.root / 'async-library.zsh').write_bytes(Path(plugin).read_bytes())
        write_json(self.root / 'manifest.json', {
            'source_commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
            'binary_sha256':hashlib.sha256(Path(self.binary).read_bytes()).hexdigest(),
            'nixpkgs_revision':os.environ['STRESS_NIXPKGS_REV'],
            'pure_version':os.environ['STRESS_PURE_VERSION'],
            'async_sha256':hashlib.sha256(Path(plugin).read_bytes()).hexdigest()})
        # Functions are defined before worker fork, as required by zsh-async.
        # The job performs real Git queries after the externally controlled
        # FIFO release. Callback state includes its action generation.
        profile = r'''
source "$STRESS_ASYNC_PLUGIN"
async_init
typeset -gi stress_generation=0
stress_git_job() {
  local cwd=$1 generation=$2 release branch dirty
  IFS= read -r release < "$cwd/async-release"
  [[ $release == $generation ]] || return 19
  branch=$(git -C "$cwd" symbolic-ref --short HEAD) || return
  dirty=0
  [[ -n $(git -C "$cwd" status --porcelain --untracked-files=no) ]] && dirty=1
  print -r -- "G:$generation B:$branch D:$dirty"
}
stress_git_callback() {
  [[ $1 == stress_git_job && $2 == 0 ]] || {
    print -r -- "$*" > "$HOME/async-error"
    return
  }
  PROMPT=$'%F{cyan}user@async%f\n'"%F{yellow}${3}%f"$'\nREADY> '
  zle reset-prompt
  print -rn -- "$3" > "$HOME/async-completed.tmp"
  command mv "$HOME/async-completed.tmp" "$HOME/async-completed"
}
async_start_worker stress_prompt -u
async_register_callback stress_prompt stress_git_callback
precmd() {
  (( ++stress_generation ))
  async_job stress_prompt stress_git_job "$PWD" "$stress_generation"
}
'''
        (self.root / 'home/.zshrc').write_text((self.root / 'home/.zshrc').read_text()+profile)


def run(binary, directory):
    directory.mkdir(parents=True)
    session = AsyncSession(binary, directory, 'zsh')
    try:
        completed = directory / 'home/async-completed'
        for generation, (rows, cols, branch, dirty) in enumerate([
                (24,85,'branch-alpha',0), (9,37,'branch-beta',1),
                (17,61,'branch-long-界-name',0), (12,45,'branch-alpha',1)], 1):
            # External fixture actions change actual Git state while the worker
            # waits. Expected state comes from these actions, not Git/mux queries.
            work = directory / 'work'
            subprocess.run(['git','checkout','-B',branch], cwd=work, check=True, capture_output=True)
            if dirty:
                (work / 'tracked.txt').write_text(f'changed-{generation}\n')
            else:
                subprocess.run(['git','checkout','--','tracked.txt'], cwd=work, check=True, capture_output=True)
            value = f'async-command-{generation}-界-e\u0301'
            command = f"printf '%s\\n' '{value}' > async-input.txtXXX"
            session.input(command.encode())
            session.settle()
            session.resize(rows,cols)
            session.settle()
            # A FIFO write releases precisely one worker generation. No timer
            # substitutes for job completion or prompt callback completion.
            deadline = time.monotonic()+10
            while True:
                try:
                    descriptor = os.open(session.fifo, os.O_WRONLY | os.O_NONBLOCK)
                    break
                except OSError as error:
                    import errno
                    if error.errno != errno.ENXIO:
                        raise
                    session.pump(.005)
                    assert time.monotonic() < deadline, 'async worker never opened release FIFO'
            try:
                os.write(descriptor, f'{generation}\n'.encode())
            finally:
                os.close(descriptor)
            expected = f'G:{generation} B:{branch} D:{dirty}'
            file_equals(session, completed, expected.encode())
            error = directory / 'home/async-error'
            assert not error.exists(), error.read_text() if error.exists() else ''
            session.action('async-git-state', generation=generation, branch=branch, dirty=dirty, expected=expected)
            session.checkpoint(f'async-plugin-redraw-{generation}')
            # Verify the actual prompt marker independently of source replay.
            text = '\n'.join(''.join(c[0] for c in row) for row in session.clients[0]['terminal'].snapshot(session.bar,cols-session.bar)['cells'])
            assert f'G:{generation}' in text and f'D:{dirty}' in text, 'completed state not rendered in prompt'
            session.input(b'\x01\x05\x7f\x7f\x7f\r')
            file_equals(session, work / 'async-input.txt', (value+'\n').encode())
            session.checkpoint(f'async-plugin-input-{generation}')
        return {'passed':True,'plugin':'Pure bundled zsh-async',
                'pure_version':os.environ['STRESS_PURE_VERSION'],'checkpoints':session.checkpoints}
    finally:
        session.close()


if __name__ == '__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    args=parser.parse_args()
    write_json(args.output/'summary.json',run(args.binary.resolve(),args.output.resolve()))
