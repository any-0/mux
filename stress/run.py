#!/usr/bin/env python3
"""Adversarial real-shell sessions, with a recorded independent cell oracle."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import random
import select
import shutil
import shlex
import struct
import subprocess
import termios
import time
import traceback

from oracle import Terminal, viewport

ROOT = Path(__file__).resolve().parent.parent


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


class Session:
    def __init__(self, binary, directory, shell, proxied=True):
        self.root = directory
        self.binary = str(binary)
        self.shell = shell
        self.rows, self.cols, self.bar = 24, 85, 5
        self.action_file = (directory / 'actions.jsonl').open('w', buffering=1)
        for name in ('home', 'runtime', 'state', 'capture', 'work'):
            (directory / name).mkdir(parents=True)
        (directory / 'runtime').chmod(0o700)
        self.env = {k: v for k, v in os.environ.items() if k not in ('MUX', 'MUX_PANE')}
        self.env.update(HOME=str(directory / 'home'), XDG_RUNTIME_DIR=str(directory / 'runtime'),
                        XDG_STATE_HOME=str(directory / 'state'), XDG_CONFIG_HOME=str(directory / 'home/.config'),
                        TERM='xterm-256color', COLORTERM='truecolor', LC_ALL='C.UTF-8', TZ='UTC',
                        SHELL=str(ROOT / 'stress/proxy.py') if proxied else shutil.which(shell),
                        STRESS_REAL_SHELL=shutil.which(shell), STRESS_CAPTURE=str(directory / 'capture'),
                        STRESS_RC=str(directory / 'home/.bashrc'), ZDOTDIR=str(directory / 'home'))
        self.profile()
        theme = directory / 'theme.toml'
        theme.write_text('variant = "dark"\n[palette]\nbackground = "#010203"\nsecondary = "#113355"\nsurface_raised = "#223344"\nsurface = "#334455"\nmuted = "#778899"\naccent = "#446688"\n')
        (directory / 'config.toml').write_text(f'mouse = true\ndefault_cursor_shape = "underline"\ntheme = "{theme}"\n')
        self.stderr = (directory / 'daemon.stderr').open('wb')
        self.daemon = subprocess.Popen([self.binary, '__server', str(directory / 'runtime/mux.sock')],
                                       env=self.env, cwd=directory / 'work', stderr=self.stderr)
        deadline = time.monotonic() + 10
        while not (directory / 'runtime/mux.sock').exists():
            assert self.daemon.poll() is None, 'daemon exited before bind'
            assert time.monotonic() < deadline, 'daemon bind timed out'
            time.sleep(.01)
        self.clients = []
        self.attach('client')
        self.capture_offset = 0
        self.capture_pending = ''
        self.expected = Terminal(self.rows, self.cols - self.bar, default_cursor_shape='underline')
        self.capture_path = None
        self.capture_excluded = set()
        self.checkpoints = 0
        self.proxied = proxied
        try:
            self.wait_text('READY>')
        except Exception:
            self.close()
            raise

    def profile(self):
        # Fixed path/time labels, real embedded LF, styles inside zero-width
        # delimiters. cwd is exercised by cd but not displayed nondeterministically.
        bash = "HISTFILE=/dev/null\nPROMPT_COMMAND=\nPS1='\\[\\e[36m\\]user@isolated\\[\\e[0m\\]\\n[fixed-cwd 12:34]\\nREADY> '\n"
        zsh = "HISTFILE=/dev/null\nsetopt PROMPT_SUBST\nPROMPT=$'%F{cyan}user@isolated%f\\n[fixed-cwd 12:34]\\nREADY> '\nRPROMPT=''\n"
        fish = "function fish_greeting; end\nfunction fish_prompt\nset_color cyan\nprintf 'user@isolated'\nset_color normal\nprintf '\\n[fixed-cwd 12:34]\\nREADY> '\nend\n"
        (self.root / 'home/.bashrc').write_text(bash)
        (self.root / 'home/.zshrc').write_text(zsh)
        config = self.root / 'home/.config/fish'
        config.mkdir(parents=True)
        (config / 'config.fish').write_text(fish)

    def action(self, kind, **values):
        self.action_file.write(json.dumps({'index': getattr(self, 'action_index', 0), 'kind': kind, **values}) + '\n')
        self.action_index = getattr(self, 'action_index', 0) + 1

    def attach(self, name, rows=None, cols=None):
        rows, cols = rows or self.rows, cols or self.cols
        master, slave = pty.openpty()
        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
        process = subprocess.Popen([self.binary, '--config', str(self.root / 'config.toml'), '--session', 'stress'],
                                   env=self.env, cwd=self.root / 'work', stdin=slave, stdout=slave, stderr=slave,
                                   preexec_fn=controlling_terminal)
        os.close(slave)
        client = {'name': name, 'fd': master, 'process': process, 'terminal': Terminal(rows, cols),
                  'rows': rows, 'cols': cols,
                  'raw': (self.root / f'{name}.ansi').open('wb'),
                  'events': (self.root / f'{name}.events.jsonl').open('w', buffering=1), 'offset': 0}
        client['events'].write(json.dumps({'event': 'resize', 'offset': 0, 'rows': rows, 'cols': cols, 'action_index': getattr(self, 'action_index', 0)}) + '\n')
        self.clients.append(client)
        self.action('attach', name=name, rows=rows, cols=cols)
        return client

    def pump(self, wait=.02):
        ready, _, _ = select.select([c['fd'] for c in self.clients], [], [], wait)
        for client in self.clients:
            if client['fd'] not in ready:
                continue
            data = os.read(client['fd'], 65536)
            assert data, 'client EOF'
            client['raw'].write(data)
            client['raw'].flush()
            client['offset'] += len(data)
            client['terminal'].feed(data)
        if self.proxied:
            if self.capture_path is None:
                paths = [p for p in (self.root / 'capture').glob('*.jsonl') if p.name not in self.capture_excluded]
                if paths:
                    self.capture_path = paths[0]
            if self.capture_path:
                with self.capture_path.open() as stream:
                    stream.seek(self.capture_offset)
                    self.capture_pending += stream.read()
                    self.capture_offset = stream.tell()
                lines = self.capture_pending.split('\n')
                self.capture_pending = lines.pop()
                for line in lines:
                    event = json.loads(line)
                    if event['event'] == 'resize':
                        self.expected.resize(event['rows'], event['cols'])
                    elif event['event'] == 'output':
                        self.expected.feed(bytes.fromhex(event['hex']))
        return bool(ready)

    def wait_text(self, text, timeout=15):
        deadline = time.monotonic() + timeout
        while True:
            self.pump()
            screen = self.clients[0]['terminal'].screen
            # Cell-based gate; raw typed command cannot satisfy a style/cursor checkpoint.
            if any(text in ''.join(screen.buffer[y][x].data for x in range(self.bar, screen.columns))
                   for y in range(screen.lines)):
                return
            assert time.monotonic() < deadline, f'timed out waiting for {text!r}'

    def input(self, data):
        self.action('input', hex=data.hex())
        os.write(self.clients[0]['fd'], data)

    def command(self, *args):
        self.action('mux-command', argv=list(args))
        env = self.env | {'MUX': str(self.root / 'runtime/mux.sock')}
        result = subprocess.run([self.binary, *args], env=env, capture_output=True, timeout=10)
        assert result.returncode == 0, result.stderr.decode(errors='replace')

    def query(self, name):
        env = self.env | {'MUX': str(self.root / 'runtime/mux.sock')}
        result = subprocess.run([self.binary, name, '--json'], env=env, capture_output=True, timeout=10)
        assert result.returncode == 0, result.stderr.decode(errors='replace')
        value = json.loads(result.stdout)
        self.action('query-observed', name=name, value=value)
        return value

    def resize(self, rows, cols):
        self.rows, self.cols = rows, cols
        self.action('resize', rows=rows, cols=cols)
        for client in self.clients:
            client.update(rows=rows, cols=cols)
            client['events'].write(json.dumps({'event': 'resize', 'offset': client['offset'], 'rows': rows, 'cols': cols, 'action_index': self.action_index - 1}) + '\n')
            fcntl.ioctl(client['fd'], termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
            client['terminal'].resize(rows, cols)
        # No undocumented reflow rule is inferred from mux. Ctrl-L asks each
        # real line editor to repaint at the new width before comparing cells.
        self.settle()
        self.input(b'\x0c')

    def resize_client(self, client, rows, cols):
        self.action('resize-client', name=client['name'], rows=rows, cols=cols)
        client.update(rows=rows, cols=cols)
        client['events'].write(json.dumps({'event': 'resize', 'offset': client['offset'],
                                          'rows': rows, 'cols': cols,
                                          'action_index': self.action_index - 1}) + '\n')
        fcntl.ioctl(client['fd'], termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
        client['terminal'].resize(rows, cols)
        self.settle()
        assert self.expected.screen.lines == rows and self.expected.screen.columns == cols - self.bar, 'last resize did not own shared PTY size'
        self.input(b'\x0c')

    def settle(self):
        deadline = time.monotonic() + 5
        quiet = time.monotonic()
        while time.monotonic() - quiet < .25:
            if self.pump():
                quiet = time.monotonic()
            assert time.monotonic() < deadline, 'output never settled'

    def checkpoint(self, name):
        self.settle()
        expected = self.expected.snapshot()
        self.action('checkpoint', name=name, capture_file=self.capture_path.name,
                    capture_offset=self.capture_offset, bar=self.bar, rows=self.rows, cols=self.cols,
                    clients=[{'name': c['name'], 'offset': c['offset'], 'rows': c['rows'], 'cols': c['cols']} for c in self.clients])
        errors = []
        for index, client in enumerate(self.clients):
            actual = client['terminal'].snapshot(self.bar, client['cols'] - self.bar)
            wanted_view = viewport(expected, client['rows'], client['cols'] - self.bar)
            if actual != wanted_view:
                differences = []
                for y, (wanted_row, got_row) in enumerate(zip(wanted_view['cells'], actual['cells'])):
                    for x, (wanted, got) in enumerate(zip(wanted_row, got_row)):
                        if wanted != got:
                            differences.append({'row': y, 'col': x, 'expected': wanted, 'actual': got})
                errors.append({'client': index, 'cells': differences[:50], 'total_cell_differences': len(differences),
                               'expected_cursor': wanted_view['cursor'], 'actual_cursor': actual['cursor'],
                               'expected_hidden': expected['hidden'], 'actual_hidden': actual['hidden'],
                               'expected_shape': expected['cursor_shape'], 'actual_shape': actual['cursor_shape']})
                write_json(self.root / f'{name}-client{index}-actual.json', actual)
        write_json(self.root / f'{name}-expected.json', expected)
        self.checkpoints += 1
        if errors:
            write_json(self.root / f'{name}-diff.json', errors)
            raise AssertionError(f'{name}: independent cell/cursor/attribute mismatch: {errors[:1]}')

    def sidebar(self, count, active):
        # Scenario-owned window count and active index, never query mux for expected state.
        self.settle()
        capacity = max(0, (self.rows - 2) // 3)
        visible = min(count, capacity)
        start = max(0, min(active - visible // 2, count - visible)) if count > visible else 0
        top = 1 + (self.rows - 2 - visible * 3) // 2
        digits = len(str(count))
        label_width = digits + 2
        expected = {top + offset * 3: '•' if start + offset == active else str(start + offset + 1)
                    for offset in range(visible)}
        for client in self.clients:
            screen = client['terminal'].screen
            actual = {y: ''.join(screen.buffer[y][x].data for x in range(label_width)).strip()
                      for y in range(1, self.rows - 1)}
            actual = {y: text for y, text in actual.items() if text and text != '·'
                      and (text == '•' or text.isdecimal())}
            assert actual == expected, f'sidebar action model: expected {expected}, actual {actual}'
            for row, label in expected.items():
                background = '113355' if label == '•' else '223344'
                for y in (row, row + 1):
                    for x in range(label_width):
                        cell = screen.buffer[y][x]
                        assert cell.bg == background, (f'sidebar icon tile background at {y},{x}: '
                                                       f'expected {background}, actual {cell.bg}')
                        assert not cell.underscore and not cell.italics and not cell.bold, 'pane SGR leaked into sidebar'
        self.action('sidebar-check', count=count, active=active, expected=expected)

    def restart(self, crash=False):
        self.action('restart', crash=crash)
        # Give the documented asynchronous state writer time to checkpoint;
        # crash testing here targets durable completed output, not an undefined
        # last-few-milliseconds loss budget.
        deadline = time.monotonic() + 1.5
        while time.monotonic() < deadline:
            self.pump(.02)
        evidence = self.root / ('pre-crash-journals' if crash else 'pre-graceful-journals')
        evidence.mkdir()
        for path in (self.root / 'state/mux').glob('*.ansi'):
            shutil.copy2(path, evidence / path.name)
        self.capture_excluded.update(p.name for p in (self.root / 'capture').glob('*.jsonl'))
        for client in self.clients:
            client['process'].terminate()
            client['process'].wait(timeout=5)
            os.close(client['fd'])
            client['raw'].close()
            client['events'].close()
        self.clients.clear()
        if crash:
            self.daemon.kill()
        else:
            self.command('kill-server')
        self.daemon.wait(timeout=10)
        stopped = self.root / ('stopped-crash-journals' if crash else 'stopped-graceful-journals')
        stopped.mkdir()
        for path in (self.root / 'state/mux').glob('*.ansi'):
            shutil.copy2(path, stopped / path.name)
        socket = self.root / 'runtime/mux.sock'
        self.daemon = subprocess.Popen([self.binary, '__server', str(socket)],
                                       env=self.env, cwd=self.root / 'work', stderr=self.stderr)
        # A killed daemon leaves a socket pathname; attach retries connect, but
        # first require this process to remain alive and finish startup.
        time.sleep(.15)
        assert self.daemon.poll() is None, 'restarted daemon exited'
        self.capture_path = None
        self.capture_offset = 0
        self.capture_pending = ''
        self.attach('crash-restored' if crash else 'graceful-restored')
        self.wait_text('READY>')
        self.settle()

    def close(self):
        for client in getattr(self, 'clients', []):
            client['process'].terminate()
            try:
                client['process'].wait(timeout=3)
            except subprocess.TimeoutExpired:
                client['process'].kill()
                client['process'].wait()
            os.close(client['fd'])
            client['raw'].close()
            client['events'].close()
        if hasattr(self, 'daemon'):
            self.daemon.terminate()
            try:
                self.daemon.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.daemon.kill()
                self.daemon.wait()
        self.action_file.close()
        self.stderr.close()


def run_profile(binary, directory, shell, cycles, seed):
    started = time.monotonic()
    session = Session(binary, directory, shell)
    rng = random.Random(seed)
    try:
        session.checkpoint('initial-multiline-prompt')
        # Prompt expansion is owned by each real shell. Values change after
        # commands and cd; the oracle consumes captured bytes, never a prompt
        # template or mux's current screen. No wall clock enters replay.
        dynamic = {
            'bash': r'''STRESS_PROMPT_N=0; PROMPT_COMMAND='STRESS_PROMPT_N=$((STRESS_PROMPT_N+1))'; PS1='\[\e[36m\]user@isolated\[\e[0m\]\n[\w command ${STRESS_PROMPT_N}]\nREADY> ' ''',
            'zsh': r'''STRESS_PROMPT_N=0; function stress_precmd() { (( STRESS_PROMPT_N += 1 )); }; precmd_functions=(stress_precmd); PROMPT=$'%F{cyan}user@isolated%f\n[%~ command ${STRESS_PROMPT_N}]\nREADY> ' ''',
            'fish': r'''set -g STRESS_PROMPT_N 0; function fish_prompt; set -g STRESS_PROMPT_N (math $STRESS_PROMPT_N + 1); set_color cyan; printf 'user@isolated'; set_color normal; printf '\n[%s command %s]\nREADY> ' (pwd) $STRESS_PROMPT_N; end''',
        }
        session.input(dynamic[shell].encode() + b'\r')
        session.checkpoint('dynamic-multiline-prompt')
        # Large cat/head fixture includes UTF-8, long logical rows, blank rows,
        # color and all underline variants. Inputs do not contain output markers.
        lines = []
        for n in range(2400):
            style = n % 6
            lines.append(f'\x1b[1;2;3;4:{style};58:2::13:97:211;38;2;40;180;90mROW{n:05} 界 e\u0301 '
                         + 'x' * (n % 121) + '\x1b[22;23;24;59;39m plain\n')
        (directory / 'work/large.txt').write_text(''.join(lines))
        (directory / 'work/sample.txt').write_text('alpha\n界 e\u0301\n\nlast\n')
        nested = directory / 'work/dynamic-cwd-界-long-prompt-boundary'
        nested.mkdir()
        for name in ('sample.txt', 'large.txt'):
            (nested / name).symlink_to(Path('..') / name)
        for n in range(cycles):
            assert time.monotonic() - started < 1800, 'profile exceeded 30-minute action budget'
            if n % 8 == 0:
                session.input(b'cd ' + (b'..' if n % 16 else nested.name.encode()) + b'\r')
                session.checkpoint(f'{n:03}-dynamic-cwd-prompt')
            # Keep newline-containing prompts alive throughout many resizes,
            # wrapped edits, interrupts, redraws and heavy output bursts.
            session.input(b'cat sample.txt\r')
            session.settle()
            session.checkpoint(f'{n:03}-cat')
            session.input(b'head -n 37 large.txt\r')
            session.settle()
            session.checkpoint(f'{n:03}-head-styles')
            if n % 4 == 0:
                session.input(b'cat large.txt\r')
                session.settle()
                session.checkpoint(f'{n:03}-large-wrap')
            session.input(('echo ' + 'wrapped-edit-' * 12).encode())
            session.settle()
            session.input(b'\x01\x06\x06\x05\x7f\x7f')
            session.checkpoint(f'{n:03}-edited-cursor')
            rows, cols = rng.choice([(9, 37), (17, 61), (31, 107), (12, 45), (24, 85)])
            session.resize(rows, cols)
            session.settle()
            session.checkpoint(f'{n:03}-resize-redraw')
            session.input(b'\x03')
            session.settle()
            session.checkpoint(f'{n:03}-interrupt-prompt')
            session.sidebar(1, 0)
        session.input(('cd ' + shlex.quote(str(directory / 'work')) + '\r').encode())
        session.checkpoint('dynamic-cwd-return')
        # Vim is a real application: alternate screen, Unicode buffer, edits,
        # resize while cursor is in the buffer, then restoration of shell cells.
        session.input(b'vim -Nu NONE -n sample.txt\r')
        session.wait_text('alpha')
        session.input(b':set noruler noshowcmd\rgg0')
        session.settle()
        session.checkpoint('vim-alternate')
        session.input(b'Goedited in vim\x1b')
        session.settle()
        session.checkpoint('vim-edited')
        session.resize(20, 73)
        session.input(b'\x1b:redraw!\r')
        session.settle()
        session.checkpoint('vim-resized')
        session.input(b':q!\r')
        session.settle()
        session.input(b'\x0c')
        session.checkpoint('vim-exit-prompt')
        # Equal-size second client: independent output streams must agree with
        # the same pane source; stale diffs cannot borrow the first client state.
        session.attach('second-client')
        session.settle()
        session.checkpoint('second-client-attach')
        session.input(b'cat sample.txt\r')
        session.settle()
        session.checkpoint('two-client-output')
        third = session.attach('unequal-client', rows=31, cols=107)
        session.settle()
        session.input(b'\x0c')
        session.checkpoint('unequal-larger-attach')
        for client, rows, cols in [(session.clients[0], 9, 37), (third, 17, 61),
                                  (session.clients[1], 24, 85), (third, 12, 45)]:
            session.resize_client(client, rows, cols)
            session.input(('echo ' + 'shared-viewport-' * 10).encode())
            session.checkpoint(f'unequal-{client["name"]}-{rows}x{cols}-edit')
            session.input(b'\x03')
            session.checkpoint(f'unequal-{client["name"]}-{rows}x{cols}-interrupt')
        session.resize(20, 73)
        session.checkpoint('unequal-clients-return-equal')
        # A running foreground command has no idle prompt to remove. The
        # independent source screen survives the restart; fresh shell bytes
        # from the new recorder are fed on top, exactly as protocol dictates.
        recovery = (b'\x1bc\x1b[?2004l\x1b[1;2;3;4:3;58;5;45mRECOVERY-PAYLOAD '
                    + '界 e\u0301'.encode() + b'\x1b[0m\r\n')
        (directory / 'work/recovery.ansi').write_bytes(recovery)
        for crash in (False, True):
            session.input(b'cat recovery.ansi; sleep 1000\r')
            session.wait_text('RECOVERY-PAYLOAD')
            session.checkpoint('pre-crash' if crash else 'pre-graceful-stop')
            session.restart(crash=crash)
            session.checkpoint('after-crash-restore' if crash else 'after-graceful-restore')
            session.sidebar(1, 0)
        for count in range(2, 13):
            session.command('new-window')
            session.bar = len(str(count)) + 4
            session.wait_text('READY>')
            session.sidebar(count, count - 1)
        for active in [0, 8, 3, 7, 1, 5]:
            session.command('select-window', str(active + 1))
            session.sidebar(12, active)
        return {'shell': shell, 'cycles': cycles, 'checkpoints': session.checkpoints,
                'actions': session.action_index, 'elapsed_seconds': round(time.monotonic() - started, 3),
                'budget_seconds': 1800, 'large_output_logical_rows': ((cycles + 3) // 4) * 2400,
                'passed': True}
    finally:
        session.close()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--cycles', type=int, default=40)
    parser.add_argument('--seed', type=int, default=76431)
    parser.add_argument('--shells', nargs='+', default=['bash', 'zsh', 'fish'])
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    import hashlib
    write_json(args.output / 'manifest.json', {
        'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
        'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
        'nixpkgs_revision': os.environ.get('STRESS_NIXPKGS_REV'),
        'rust_version': os.environ.get('STRESS_RUST_VERSION'),
        'seed': args.seed, 'cycles': args.cycles, 'shells': args.shells,
        'cell_fields': ['text', 'fg', 'bg', 'bold', 'dim', 'italic', 'inverse', 'underline_style', 'underline_color'],
        'cursor_fields': ['row', 'column', 'visibility', 'shape'],
    })
    results = []
    for shell in args.shells:
        directory = args.output / shell
        directory.mkdir()
        try:
            results.append(run_profile(args.binary.resolve(), directory, shell, args.cycles, args.seed))
        except Exception:
            error = traceback.format_exc()
            (directory / 'failure.txt').write_text(error)
            print(error, flush=True)
            results.append({'shell': shell, 'passed': False, 'error': error})
        write_json(args.output / 'summary.json', {'seed': args.seed, 'results': results})
    return 0 if all(r['passed'] for r in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
