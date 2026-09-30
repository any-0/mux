#!/usr/bin/env python3
"""Clean-save and real scheduled-snapshot/process-crash recovery harness.

No power-loss claim. No shortened continuum period. No host benchmark fallback.
Validation fixtures are in test_benchmarks.py; full runs require the Nix shell.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import random
import re
import shlex
import signal
import shutil
import subprocess
import tarfile
import time

from run import ROOT, SOURCE_COMMIT, SOURCE_ROOT, Terminal, command, storage
from recovery_workload import record

INTERVAL_SECONDS = 15 * 60
AGES = (0.0, 0.25, 0.5, 0.75, 0.99)
VARIANTS = ('mux', 'tmux', 'tmux-persistence')
BASE_ROWS = 200


def digest(rows):
    return hashlib.sha256('\n'.join(rows).encode()).hexdigest()


def history_fidelity(text, label, total):
    """Accept a contiguous prefix only; never call gaps/duplicates faithful."""
    found = re.findall(r'REC' + re.escape(label) + r':\d{8}\|α漢X{40}', text)
    candidates = re.findall(r'REC' + re.escape(label) + r':\d{8}\|', text)
    labels = set(re.findall(r'REC(w\d+p\d+):\d{8}\|', text))
    expected = [record(label, i) for i in range(total)]
    prefix = (found == expected[:len(found)] and len(found) <= total
              and len(candidates) == len(found) and labels <= {label})
    return {'prefix_correct': prefix, 'full_history_correct': found == expected,
            'recovered_rows': len(found), 'malformed_tagged_rows': len(candidates) - len(found),
            'lost_rows': total - len(found) if prefix else None,
            'lost_tagged_utf8_bytes': sum(len(row.encode()) for row in expected[len(found):]) if prefix else None,
            'expected_sha256': digest(expected), 'observed_sha256': digest(found),
            'wrap_correct': f'WRAP{label}|' + 'Z' * 120 in text.replace('\n', '').replace('\r', '')}


def snapshot_manifest(directory):
    """Content digests, not just mtimes or a command return status."""
    manifest = {}
    for path in sorted(directory.rglob('*')):
        if path.is_file():
            manifest[str(path.relative_to(directory))] = {
                'bytes': path.stat().st_size, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
    return manifest


def verify_resurrect_snapshot(directory):
    """Require layout records plus complete tagged history in the gzip archive."""
    text = (directory / 'last').read_text()
    pane_keys = [tuple(line.split('\t')[i] for i in (1, 2, 5))
                 for line in text.splitlines() if line.startswith('pane\t')]
    expected = [('bench', '1', '1'), ('bench', '1', '2'), ('bench', '2', '1')]
    if sorted(pane_keys) != expected:
        raise RuntimeError('saved layout has missing/duplicate/unexpected panes')
    window_keys = [tuple(line.split('\t')[i] for i in (1, 2))
                   for line in text.splitlines() if line.startswith('window\t')]
    if sorted(window_keys) != [('bench', '1'), ('bench', '2')]:
        raise RuntimeError('saved layout has missing/duplicate/unexpected windows')
    histories = {}
    with tarfile.open(directory / 'pane_contents.tar.gz', 'r:gz') as archive:
        members = {member.name.removeprefix('./'): member for member in archive.getmembers() if member.isfile()}
        for window, pane in ((1, 1), (1, 2), (2, 1)):
            key = f'pane_contents/pane-bench:{window}.{pane}'
            if key not in members:
                raise RuntimeError('saved archive missing ' + key)
            raw = archive.extractfile(members[key]).read().decode('utf-8')
            plain = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', raw)
            label = f'w{window}p{pane}'
            gate = history_fidelity(plain, label, BASE_ROWS)
            if not gate['full_history_correct'] or not gate['wrap_correct']:
                raise RuntimeError('saved archive history/wrap fidelity failed for ' + label)
            histories[label] = gate
    return histories


def proc_identity(pid):
    try:
        values = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
        return None if values[0] == 'Z' else (pid, values[19])
    except (FileNotFoundError, ProcessLookupError):
        return None


def descendants(root):
    parents = {}
    for path in Path('/proc').glob('[0-9]*'):
        try:
            parents[int(path.name)] = int((path / 'stat').read_text().rsplit(')', 1)[1].split()[1])
        except (OSError, ValueError, IndexError):
            continue
    result = {root}
    while True:
        children = {pid for pid, parent in parents.items() if parent in result}
        if children <= result:
            return {pid: proc_identity(pid) for pid in result}
        result |= children


def signal_owned(identities, sig, exclude=()):
    """PID start-time check prevents signaling a recycled/unrelated process."""
    for pid, identity in identities.items():
        if pid not in exclude and identity is not None and proc_identity(pid) == identity:
            try:
                os.kill(pid, sig)
            except ProcessLookupError:
                pass


def metadata_gate(before, after):
    # Stable pane indices and geometry, not ephemeral OS PIDs or internal IDs.
    return before == after


def nonce_command(nonce):
    split = len(nonce) // 2
    return "printf '%s%s\\n' " + shlex.quote(nonce[:split]) + ' ' + shlex.quote(nonce[split:])


def seed_command(cwd, argv):
    # Send literal printf escapes as keyboard text; actual ESC bytes would be
    # interpreted by readline or the multiplexer before reaching printf.
    return "stty -echo; PS1=''; cd " + shlex.quote(str(cwd)) + r"; printf '\033[2J\033[H'; " + shlex.join(argv)


class Recovery:
    def __init__(self, variant, directory):
        self.variant = variant
        self.directory = directory
        directory.mkdir(parents=True)
        self.runtime = directory / 'runtime'
        self.state = directory / 'state'
        self.runtime.mkdir(mode=0o700)
        self.state.mkdir(mode=0o700)
        self.capture = directory / 'capture.txt'
        self.stamp = directory / 'save-completed.txt'
        self.events = []
        self.client = None
        self.server = None
        self.env = dict(os.environ, XDG_RUNTIME_DIR=str(self.runtime), XDG_STATE_HOME=str(self.state),
                        XDG_CONFIG_HOME=str(directory / 'config'), SHELL=os.environ['BENCH_SHELL'],
                        TERM='xterm-256color', COLORTERM='truecolor', LC_ALL='C.UTF-8',
                        BASH_ENV='/dev/null', ENV='/dev/null', INPUTRC='/dev/null')
        for key in ('MUX', 'MUX_PANE', 'TMUX', 'TMUX_PANE'):
            self.env.pop(key, None)
        self.mux = str(SOURCE_ROOT / 'target/release/mux')
        self.tmux = ['tmux', '-S', str(self.runtime / 'tmux.sock')]
        self.columns = 105 if variant == 'mux' else 100
        self.config = directory / ('mux.toml' if variant == 'mux' else 'tmux.conf')
        if variant == 'mux':
            writer = directory / 'clipboard'
            writer.write_text('#!' + shutil.which('bash') + '\nset -eu\ncat > ' + shlex.quote(str(self.capture) + '.tmp') +
                              '\nmv ' + shlex.quote(str(self.capture) + '.tmp') + ' ' + shlex.quote(str(self.capture)) + '\n')
            writer.chmod(0o700)
            self.config.write_text('clipboard_command = ' + json.dumps([str(writer)]) + '\n')
        else:
            options = ["set -g base-index 1", "setw -g pane-base-index 1", "set -g status on",
                       "set -g status-interval 1", "set -g status-left ''", "set -g status-right ''",
                       "set -g history-limit 20000", "set -g mode-keys vi", "set -g escape-time 0",
                       "setw -g automatic-rename off", "set -g default-shell " + shlex.quote(self.env['SHELL'])]
            if variant == 'tmux-persistence':
                hook = ('date +%s.%N > ' + shlex.quote(str(self.stamp) + '.tmp') +
                        '; mv ' + shlex.quote(str(self.stamp) + '.tmp') + ' ' + shlex.quote(str(self.stamp)))
                options += ["set -g @resurrect-dir " + shlex.quote(str(self.state / 'resurrect')),
                            "set -g @resurrect-capture-pane-contents on", "set -g @resurrect-pane-contents-area full",
                            "set -g @resurrect-processes false", "set -g @continuum-save-interval 15",
                            "set -g @continuum-restore off", "set -g @resurrect-hook-post-save-all " + shlex.quote(hook),
                            "run-shell " + shlex.quote(os.environ['BENCH_RESURRECT']),
                            "run-shell " + shlex.quote(os.environ['BENCH_CONTINUUM'])]
            self.config.write_text('\n'.join(options) + '\n')

    def cli(self, *args):
        argv = ([self.mux] if self.variant == 'mux' else self.tmux) + list(args)
        start = time.perf_counter_ns()
        output = command(argv, self.env)
        self.events.append({'command': argv, 'start_ns': start, 'end_ns': time.perf_counter_ns(), 'output': output})
        return output

    def wait(self, predicate, timeout=30, attached=True):
        deadline = time.monotonic() + timeout
        while not predicate():
            if time.monotonic() >= deadline:
                raise RuntimeError('recovery gate timed out')
            if attached and self.client and self.client.process.poll() is None:
                self.client.pump(0.05)
            else:
                time.sleep(0.02)
        return time.perf_counter_ns()

    def launch(self, restarting=False):
        attach = self.directory / ('restart' if restarting else 'initial')
        attach.mkdir()
        if self.variant == 'mux':
            argv = [self.mux, '--config', str(self.config), '--session', 'bench']
        else:
            argv = self.tmux + ['-f', str(self.config), 'new-session', '-s', 'bootstrap' if restarting else 'bench']
        start = time.perf_counter_ns()
        if not restarting:
            self.initial_start_monotonic = time.monotonic()
        self.events.append({'launch': argv, 'start_ns': start})
        self.client = Terminal(argv, self.env, attach, 40 if self.variant == 'mux' else 41, self.columns)
        self.client.until(lambda: self.client.contains('BENCH_READY>'))
        if self.variant == 'mux':
            matches = []
            for path in Path('/proc').glob('[0-9]*'):
                try:
                    cmd = (path / 'cmdline').read_bytes().split(b'\0')
                    if b'__server' in cmd and str(self.runtime / 'mux.sock').encode() in cmd:
                        matches.append(int(path.name))
                except OSError:
                    continue
            if len(matches) != 1:
                raise RuntimeError('cannot identify isolated mux daemon')
            self.server = matches[0]
        else:
            self.server = int(self.cli('display-message', '-p', '#{pid}'))
        if self.variant == 'tmux-persistence':
            for option in ('@resurrect-save-script-path', '@resurrect-restore-script-path'):
                if not Path(self.cli('show-option', '-gqv', option)).is_file():
                    raise RuntimeError('resurrect plugin not loaded: ' + option)
            if 'continuum_save.sh' not in self.cli('show-option', '-gqv', 'status-right'):
                raise RuntimeError('continuum real scheduler not loaded')
            if self.cli('show-option', '-gqv', '@continuum-save-interval') != '15':
                raise RuntimeError('continuum interval differs from default 15 minutes')
        return start

    def shell(self, text, marker):
        self.events.append({'attached_input': text, 'start_ns': time.perf_counter_ns()})
        self.client.input((text + '\n').encode())
        self.client.until(lambda: self.client.contains(marker), timeout=120)
        self.client.drain()

    def pane_list(self, window):
        if self.variant == 'mux':
            self.cli('select-window', str(window))
            return json.loads(self.cli('list-panes', '--json'))
        text = self.cli('list-panes', '-t', f'bench:{window}', '-F', '#{pane_index}\t#{pane_width}\t#{pane_height}\t#{pane_current_path}\t#{pane_active}')
        return [{'index': int(x[0]), 'cols': int(x[1]), 'rows': int(x[2]), 'cwd': x[3],
                 'active': x[4] == '1'} for x in (line.split('\t') for line in text.splitlines())]

    def select(self, window, pane):
        if self.variant == 'mux':
            self.cli('select-window', str(window))
            if window == 1:
                self.cli('select-pane', '-U' if pane == 1 else '-D')
        else:
            self.cli('select-window', '-t', f'bench:{window}')
            self.cli('select-pane', '-t', f'bench:{window}.{pane}')
        self.client.drain()

    def metadata(self):
        # Capture selection before visiting windows changes it.
        if self.variant == 'mux':
            sessions = json.loads(self.cli('list-sessions', '--json'))
            windows = json.loads(self.cli('list-windows', '--json'))
            selected = next(w['id'] for w in windows if w['active'])
            if len(sessions) != 1:
                raise RuntimeError('expected exactly one restored session')
            name = sessions[0]['name']
            names = [w['name'] for w in windows]
        else:
            name = self.cli('display-message', '-p', '#{session_name}')
            selected = int(self.cli('display-message', '-p', '#{window_index}'))
            names = self.cli('list-windows', '-t', 'bench', '-F', '#{window_name}').splitlines()
        panes = {}
        for window in (1, 2):
            panes[str(window)] = [{key: pane[key] for key in ('index', 'cols', 'rows', 'cwd', 'active')}
                                  for pane in self.pane_list(window)]
        self.select(selected, next(p['index'] for p in panes[str(selected)] if p['active']))
        return {'session': name, 'window_names': names, 'selected_window': selected, 'panes': panes}

    def seed(self):
        # Normalize pane content width using a query, before creating splits.
        pane = self.pane_list(1)[0]
        if pane['cols'] != 100 and self.variant == 'mux':
            self.columns += 100 - pane['cols']
            self.client.size(40, self.columns)
            self.wait(lambda: self.pane_list(1)[0]['cols'] == 100)
        elif pane['cols'] != 100 or pane['rows'] != 40:
            raise RuntimeError('initial pane geometry mismatch')
        self.cli('rename-window', 'primary') if self.variant == 'mux' else self.cli('rename-window', '-t', 'bench:1', 'primary')
        if self.variant == 'mux':
            self.cli('split-window', '-v')
        else:
            self.cli('split-window', '-v', '-l', '20', '-t', 'bench:1')
        self.cli('new-window') if self.variant == 'mux' else self.cli('new-window', '-t', 'bench:2', '-n', 'secondary')
        if self.variant == 'mux':
            self.cli('rename-window', 'secondary')
        for window, pane in ((1, 1), (1, 2), (2, 1)):
            self.select(window, pane)
            label = f'w{window}p{pane}'
            cwd = self.directory / ('cwd-' + label)
            cwd.mkdir()
            argv = ['python3', str(ROOT / 'benchmarks/recovery_workload.py'), label, '--probe']
            # Disable echo, then clear prompts before the history fixture.
            self.shell(seed_command(cwd, argv), f'REC_DONE_{label}_{BASE_ROWS}')
            # mux samples cwd on output, at most every 250 ms without shell
            # integration. Emit a fresh rendered barrier after that interval;
            # polling metadata alone cannot trigger another sample. Apply the
            # same preparation to all variants, outside measured operations.
            self.wait_duration(0.3)
            barrier = 'REC_CWD_' + str(time.perf_counter_ns())
            self.shell(nonce_command(barrier), barrier)
            actual = next(p for p in self.pane_list(window) if p['index'] == pane)
            expected_rows = {(1, 1): 19, (1, 2): 20, (2, 1): 40}[(window, pane)]
            if actual['cols'] != 100 or actual['rows'] != expected_rows:
                raise RuntimeError('unequal pane content geometry')
            self.wait(lambda: next(p for p in self.pane_list(window) if p['index'] == pane)['cwd'] == str(cwd))
        self.select(1, 2)
        # Ensure layout debounce has completed before a scheduler/crash phase.
        self.wait_duration(2)

    def wait_duration(self, seconds):
        deadline = time.monotonic() + seconds
        next_update = time.monotonic() + 30
        while time.monotonic() < deadline:
            self.client.pump(min(0.05, max(0, deadline - time.monotonic())))
            if time.monotonic() >= next_update:
                print(self.variant, 'waiting', round(deadline - time.monotonic()), 'seconds', flush=True)
                next_update += 30

    def history(self, window, pane):
        self.select(window, pane)
        if self.variant == 'mux':
            self.capture.unlink(missing_ok=True)
            self.client.input(b'\x1bwggVGy')
            self.wait(lambda: self.capture.exists())
            # Clipboard command writes directly: wait for process/file stability.
            self.client.drain()
            text = self.capture.read_text()
        else:
            text = self.cli('capture-pane', '-p', '-J', '-S', '-', '-t', f'bench:{window}.{pane}')
        target = self.directory / f'history-{len(self.events):04d}-w{window}p{pane}.txt'
        target.write_text(text)
        return text

    def style_probe(self, window, pane):
        self.select(window, pane)
        self.client.input(b'\x1bwgg' if self.variant == 'mux' else b'\x02[')
        # tmux vi copy-mode uses g to go to history top; explicitly bind it below
        # using a command if needed, without timing this correctness probe.
        if self.variant != 'mux':
            self.cli('send-keys', '-X', 'history-top')
        label = f'w{window}p{pane}'
        self.client.until(lambda: any(record(label, 0) in line for line in self.client.screen.display))
        row = next(i for i, line in enumerate(self.client.screen.display) if record(label, 0) in line)
        line = self.client.screen.display[row]
        start = line.index(record(label, 0))
        # Skip the copy cursor cell; test a run of colored ASCII cells.
        styles = [{'data': self.client.screen.buffer[row][col].data,
                   'fg': self.client.screen.buffer[row][col].fg,
                   'bg': self.client.screen.buffer[row][col].bg,
                   'bold': self.client.screen.buffer[row][col].bold}
                  for col in range(start + 2, start + 10)]
        self.client.input(b'\x1b' if self.variant == 'mux' else b'q')
        self.client.drain()
        return styles

    def capture_fidelity(self, totals):
        result = {}
        for window, pane in ((1, 1), (1, 2), (2, 1)):
            label = f'w{window}p{pane}'
            result[label] = history_fidelity(self.history(window, pane), label, totals[label])
            if result[label]['recovered_rows']:
                result[label]['styles'] = self.style_probe(window, pane)
        return result

    def completed_snapshot(self):
        last = self.state / 'resurrect/last'
        if not self.stamp.exists() or not last.is_file():
            return False
        # Hook runs after archive creation. Validate gzip/tar and actual fixture
        # digests rather than trusting an option, mtime or successful CLI ack.
        verify_resurrect_snapshot(self.state / 'resurrect')
        return True

    def clean_save(self):
        if self.variant == 'tmux-persistence':
            script = self.cli('show-option', '-gqv', '@resurrect-save-script-path')
            self.stamp.unlink(missing_ok=True)
            start = time.perf_counter_ns()
            self.cli('run-shell', shlex.quote(script))
            self.wait(self.completed_snapshot)
            return {'save_ms': (time.perf_counter_ns() - start) / 1e6,
                    'snapshot_manifest': snapshot_manifest(self.state)}
        return {'save_ms': None, 'save_operation': 'mux clean shutdown includes flush/sync' if self.variant == 'mux' else 'unsupported'}

    def stop(self, crash=False):
        children = descendants(self.server)
        server_identity = proc_identity(self.server)
        if server_identity is None:
            raise RuntimeError('isolated server exited before planned stop/crash')
        start = time.perf_counter_ns()
        if crash:
            signal_owned({self.server: server_identity}, signal.SIGKILL)
        else:
            self.cli('kill-server')
        self.wait(lambda: proc_identity(self.server) != server_identity, attached=False)
        end = time.perf_counter_ns()
        # tmux/mux shells may remain after SIGKILL. Remove only recorded owned
        # descendants with unchanged start times before launching fresh shells.
        signal_owned(children, signal.SIGKILL, exclude=(self.server,))
        self.wait(lambda: all(identity is None or proc_identity(pid) != identity
                              for pid, identity in children.items()), attached=False)
        self.events.append({'owned_processes_before_stop': children, 'all_old_processes_ended': True})
        self.client.close()
        self.client = None
        self.server = None
        return (end - start) / 1e6

    def restore(self):
        start = self.launch(restarting=True)
        daemon_ready = time.perf_counter_ns()
        if self.variant == 'tmux-persistence':
            script = self.cli('show-option', '-gqv', '@resurrect-restore-script-path')
            complete = self.directory / 'restore-completed.txt'
            complete.unlink(missing_ok=True)
            restore_start = time.perf_counter_ns()
            self.cli('run-shell', shlex.quote(script) + ' && printf complete > ' + shlex.quote(str(complete)))
            self.wait(complete.exists)
            restore_end = time.perf_counter_ns()
            # Preserve the restored selection before removing the empty bootstrap.
            self.cli('kill-session', '-t', 'bootstrap')
        else:
            restore_start, restore_end = start, daemon_ready
        # A fresh nonce sent through the attached PTY prevents a journal-replayed
        # old shell prompt from being mistaken for a live restored shell.
        nonce = 'REC_LIVE_' + str(time.perf_counter_ns())
        self.shell(nonce_command(nonce), nonce)
        end = time.perf_counter_ns()
        return {'restart_attach_live_ms': (end - start) / 1e6,
                'decoded_prompt_ms': (daemon_ready - start) / 1e6,
                'restore_script_ms': (restore_end - restore_start) / 1e6 if self.variant == 'tmux-persistence' else None,
                'restore_operation': 'automatic journal replay included in launch' if self.variant == 'mux' else 'manual resurrect script' if self.variant == 'tmux-persistence' else 'no persistence'}

    def confirm_shells(self):
        confirmed = []
        for window, pane in ((1, 1), (1, 2), (2, 1)):
            self.select(window, pane)
            nonce = 'REC_SHELL_' + str(time.perf_counter_ns())
            self.shell(nonce_command(nonce), nonce)
            confirmed.append(f'w{window}p{pane}')
        self.select(1, 2)
        return confirmed

    def cleanup(self):
        try:
            if self.server and proc_identity(self.server):
                self.stop()
            elif self.client:
                self.client.close()
        finally:
            (self.directory / 'commands.json').write_text(json.dumps(self.events, indent=2))


def recovery_trial(variant, number, mode, age, output):
    run = Recovery(variant, output / f'{mode}-{age:.2f}-{number:03d}-{variant}')
    result = {'variant': variant, 'trial': number, 'mode': mode, 'age_fraction': age,
              'process_crash_only': True, 'correct': False, 'recovery_supported': variant != 'tmux'}
    totals = {label: BASE_ROWS for label in ('w1p1', 'w1p2', 'w2p1')}
    try:
        run.launch()
        run.seed()
        before_metadata = run.metadata()
        before = run.capture_fidelity(totals)
        if not all(v['full_history_correct'] and v['wrap_correct']
                   and any(cell['fg'] != 'default' for cell in v.get('styles', [])) for v in before.values()):
            raise RuntimeError('seed history/wrap correctness failed')
        run.select(1, 2)
        if mode == 'clean':
            result.update(run.clean_save())
        else:
            # No manual save in this path. Every variant gets the same default
            # interval and age schedule; only the persistence variant polls a
            # real scheduler-produced snapshot-completion hook.
            prime_start = time.monotonic()
            if variant == 'tmux-persistence':
                run.wait(run.completed_snapshot, timeout=INTERVAL_SECONDS + 30)
                stamp = run.stamp.read_text()
                result['snapshot_wall_time'] = float(stamp)
                result['snapshot_manifest'] = snapshot_manifest(run.state)
            else:
                run.wait_duration(max(0, INTERVAL_SECONDS - (time.monotonic() - run.initial_start_monotonic)))
                stamp = None
            result['prime_elapsed_seconds'] = time.monotonic() - prime_start
            age_start = time.monotonic()
            # Numbered post-snapshot output permits exact loss accounting.
            # Twenty tagged rows/second at the same monotonic schedule in all
            # variants. The .99 bucket stays below the equal 20k history limit.
            label = 'w1p2'
            for tick in range(max(1, math.floor(age * INTERVAL_SECONDS))):
                run.wait_duration(max(0, age_start + tick - time.monotonic()))
                dispatch = time.monotonic()
                if dispatch - (age_start + tick) > 0.5:
                    raise RuntimeError('post-snapshot output schedule missed its 0.5-second dispatch tolerance')
                run.events.append({'output_tick': tick, 'scheduled_monotonic': age_start + tick,
                                   'dispatched_monotonic': dispatch})
                argv = ['python3', str(ROOT / 'benchmarks/recovery_workload.py'), label,
                        '--start', str(totals[label]), '--count', '20']
                totals[label] += 20
                run.shell(shlex.join(argv), f'REC_DONE_{label}_{totals[label]}')
            run.wait_duration(max(0, age * INTERVAL_SECONDS - (time.monotonic() - age_start)))
            result['age_elapsed_seconds'] = time.monotonic() - age_start
            if stamp is not None:
                if run.stamp.read_text() != stamp:
                    raise RuntimeError('another periodic save occurred before target crash age')
                result['snapshot_age_at_crash_seconds'] = time.time() - float(stamp)
        result['before_metadata'] = before_metadata
        result['before_fidelity'] = before
        result['storage_before_stop'] = storage(run.state)
        result['stop_ms'] = run.stop(crash=mode == 'crash')
        if variant == 'mux' and mode == 'clean':
            result['save_shutdown_ms'] = result['stop_ms']
        result['durable_state_after_stop'] = snapshot_manifest(run.state)
        result.update(storage(run.state))
        result.update(run.restore())
        if variant == 'tmux':
            # Expected lack of persistence is an observed outcome, not a zero-ms
            # successful restore and not a fidelity-gated performance advantage.
            sessions = run.cli('list-sessions', '-F', '#{session_name}').splitlines()
            result['expected_unavailable'] = 'bench' not in sessions
            result['lost_rows'] = sum(totals.values())
            result['lost_tagged_utf8_bytes'] = sum(len(record(label, i).encode())
                                                   for label, count in totals.items() for i in range(count))
            result['correct'] = result['expected_unavailable']
        else:
            after_metadata = run.metadata()
            result['after_metadata'] = after_metadata
            result['metadata_correct'] = metadata_gate(before_metadata, after_metadata)
            started = time.perf_counter_ns()
            result['fresh_shells_confirmed'] = run.confirm_shells()
            after = run.capture_fidelity(totals)
            result['verification_ms'] = (time.perf_counter_ns() - started) / 1e6
            result['after_fidelity'] = after
            for label, gate in after.items():
                gate['formatting_correct'] = gate.get('styles') == before[label].get('styles')
            history_ok = all(v['prefix_correct'] and v['wrap_correct'] for v in after.values())
            if mode == 'clean':
                history_ok &= all(v['full_history_correct'] for v in after.values())
            result['lost_rows'] = sum(v['lost_rows'] for v in after.values()) if all(v['lost_rows'] is not None for v in after.values()) else None
            result['lost_tagged_utf8_bytes'] = sum(v['lost_tagged_utf8_bytes'] for v in after.values()) if all(v['lost_tagged_utf8_bytes'] is not None for v in after.values()) else None
            result['formatting_correct'] = all(v['formatting_correct'] for v in after.values())
            result['correct'] = result['metadata_correct'] and history_ok and result['formatting_correct']
    except Exception as error:
        result['error'] = str(error)
    finally:
        try:
            run.cleanup()
        except Exception as error:
            result['cleanup_error'] = str(error)
            result['correct'] = False
        (run.directory / 'sample.json').write_text(json.dumps(result, indent=2))
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', required=True, type=Path)
    p.add_argument('--trials', default=30, type=int)
    p.add_argument('--mode', choices=('clean', 'crash', 'both'), default='both')
    a = p.parse_args()
    if os.uname().sysname != 'Linux' or not os.environ.get('BENCH_NIXPKGS_REV') or a.trials < 20:
        p.error('requires Linux, Nix benchmark devShell and >=20 trials')
    for tool in ('rustc', 'cargo', 'tmux', 'python3', 'bash'):
        path = shutil.which(tool)
        if not path:
            p.error(tool + ' is missing')
        if not str(Path(path).resolve()).startswith('/nix/store/'):
            p.error(tool + ' must come from Nix')
    if command(['git', '-C', str(SOURCE_ROOT), 'rev-parse', 'HEAD']) != SOURCE_COMMIT:
        p.error('use scripts/benchmark-nix or supply the exact pinned source checkout')
    if command(['git', '-C', str(SOURCE_ROOT), 'diff', SOURCE_COMMIT, '--', 'src', 'vendor', 'Cargo.toml', 'Cargo.lock']):
        p.error('runtime source differs from benchmark pin')
    if not command(['rustc', '--version']).startswith('rustc ' + os.environ['BENCH_RUST_VERSION']):
        p.error('Rust version differs from benchmark devShell pin')
    if command(['tmux', '-V']) != 'tmux ' + os.environ['BENCH_TMUX_VERSION']:
        p.error('tmux version differs from benchmark devShell pin')
    a.output = a.output.resolve()
    a.output.mkdir(parents=True, exist_ok=False)
    manifest = {'kind': 'recovery', 'source_commit': SOURCE_COMMIT,
                'harness_commit': command(['git', 'rev-parse', 'HEAD']),
                'nixpkgs_rev': os.environ['BENCH_NIXPKGS_REV'], 'interval_seconds': INTERVAL_SECONDS,
                'ages': AGES, 'trials': a.trials, 'mode': a.mode,
                'versions': {tool: command([tool, '-V' if tool == 'tmux' else '--version'])
                             for tool in ('rustc', 'cargo', 'tmux', 'python3', 'bash')},
                'lock': json.loads((ROOT / '.nix/flake.lock').read_text()),
                'execution_context': os.environ.get('BENCH_EXECUTION_CONTEXT', 'selected-cloud'),
                'uname': command(['uname', '-a']), 'mounts': Path('/proc/mounts').read_text(),
                'cpuinfo': Path('/proc/cpuinfo').read_text(), 'loadavg': Path('/proc/loadavg').read_text()}
    (a.output / 'environment.json').write_text(json.dumps(manifest, indent=2))
    with (a.output / 'build.log').open('w') as log:
        subprocess.run(['cargo', 'build', '--release', '--locked'], cwd=SOURCE_ROOT, stdout=log,
                       stderr=subprocess.STDOUT, check=True)
    samples = []
    rng = random.Random(20260930)
    modes = [('clean', 0.0)] if a.mode == 'clean' else [('crash', age) for age in AGES]
    if a.mode == 'both':
        modes.insert(0, ('clean', 0.0))
    for mode, age in modes:
        for number in range(a.trials + 1):
            variants = list(VARIANTS)
            rng.shuffle(variants)
            for variant in variants:
                sample = recovery_trial(variant, number, mode, age, a.output)
                samples.append(sample)
                print(mode, age, number, variant, 'PASS' if sample['correct'] else sample.get('error', 'fidelity loss'), flush=True)
    # Keep failure counts/losses even where correctness suppresses timing claims.
    from testable import summarize_recovery
    summary = summarize_recovery(samples)
    (a.output / 'summary.json').write_text(json.dumps(summary, indent=2))
    if any(not s['correct'] for s in samples if s['trial'] > 0):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
