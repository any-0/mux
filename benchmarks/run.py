#!/usr/bin/env python3
"""Linux attached-PTY microbenchmarks; unvalidated until run in Nix.

Reads client ANSI output into pyte. Latency ends only after the expected
viewport rows exist in that terminal emulator, never at a CLI acknowledgement.
"""
import argparse
import codecs
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import pty
import random
import re
import select
import shlex
import shutil
import statistics
import struct
import subprocess
import termios
import time

ROOT = Path(__file__).resolve().parent.parent
SOURCE_COMMIT = (ROOT / 'benchmarks/mux-revision').read_text().strip()
SOURCE_ROOT = Path(os.environ.get('BENCH_MUX_SOURCE_DIR', ROOT)).resolve()
ROW = re.compile(r'ROW(\d{8}) x{68}')


def command(argv, env=None):
    return subprocess.check_output(argv, env=env, text=True, stderr=subprocess.STDOUT,
                                   timeout=120).strip()


class Terminal:
    def __init__(self, argv, env, directory, rows, columns):
        import pyte

        self.master, slave = pty.openpty()
        self.directory = directory
        self.input_events = []
        self.size(rows, columns)
        master = self.master

        class AttachedScreen(pyte.Screen):
            def scroll_up(self, count=1):
                saved = self.cursor.y
                top, bottom = self.margins or (0, self.lines - 1)
                self.cursor.y = bottom
                for _ in range(min(count or 1, bottom - top + 1)):
                    self.index()
                self.cursor.y = saved

            def scroll_down(self, count=1):
                saved = self.cursor.y
                top, bottom = self.margins or (0, self.lines - 1)
                self.cursor.y = top
                for _ in range(min(count or 1, bottom - top + 1)):
                    self.reverse_index()
                self.cursor.y = saved

            def report_device_attributes(self, mode=0, **kwargs):
                # pyte collapses secondary DA (CSI > c) into primary DA.
                # Its default VT102 reply is therefore incorrect and may be
                # forwarded into the pane shell. This renderer advertises no
                # identity extensions; tmux uses its terminal fallback.
                pass

            @property
            def display(self):
                from wcwidth import wcwidth
                lines = []
                for y in range(self.lines):
                    text, continuation = [], False
                    for x in range(self.columns):
                        if continuation:
                            continuation = False
                            continue
                        # Incremental redraw can overwrite a wide glyph's
                        # leading cell while pyte retains its empty stub.
                        # An orphan stub is a blank cell, not a new glyph.
                        data = self.buffer[y][x].data or ' '
                        continuation = wcwidth(data[0]) == 2
                        text.append(data)
                    lines.append(''.join(text))
                return lines

            def write_process_input(self, data):
                os.write(master, data.encode())

            def report_device_status(self, mode, **kwargs):
                # pyte 0.8.2 does not accept DEC's private DSR keyword.
                # tmux requests it during real attached-client negotiation.
                if kwargs.get('private'):
                    if mode == 6:
                        self.write_process_input(f'\x1b[?{self.cursor.y + 1};{self.cursor.x + 1}R')
                else:
                    super().report_device_status(mode)

        self.screen = AttachedScreen(columns, rows)
        class AttachedStream(pyte.Stream):
            # tmux uses SU/SD optimizations that pyte 0.8.2 omits.
            csi = pyte.Stream.csi | {'S': 'scroll_up', 'T': 'scroll_down'}
            events = pyte.Stream.events | {'scroll_up', 'scroll_down'}

        self.stream = AttachedStream(self.screen)
        self.decoder = codecs.getincrementaldecoder('utf-8')('replace')
        self.raw = (directory / 'client.ansi').open('wb')
        # A real controlling terminal makes TIOCSWINSZ deliver SIGWINCH to the
        # foreground client, as a terminal emulator does. The controller is
        # single-threaded; preexec runs after setsid and stdio redirection.
        self.process = subprocess.Popen(argv, stdin=slave, stdout=slave, stderr=slave,
                                        env=env, cwd=directory, start_new_session=True,
                                        preexec_fn=lambda: fcntl.ioctl(0, termios.TIOCSCTTY, 0))
        os.close(slave)

    def size(self, rows, columns):
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack('HHHH', rows, columns, 0, 0))
        if hasattr(self, 'screen'):
            self.screen.resize(lines=rows, columns=columns)

    def pump(self, wait=0.05):
        if select.select([self.master], [], [], wait)[0]:
            try:
                data = os.read(self.master, 65536)
            except OSError as error:
                raise RuntimeError('attached client PTY closed') from error
            if not data:
                raise RuntimeError('attached client PTY reached EOF')
            self.raw.write(data)
            self.stream.feed(self.decoder.decode(data))

    def until(self, predicate, timeout=30):
        deadline = time.monotonic() + timeout
        while not predicate():
            if time.monotonic() >= deadline:
                raise RuntimeError('render correctness gate timed out: ' + repr(self.screen.display))
            self.pump()
        return time.perf_counter_ns()

    def input(self, data):
        # Timestamp immediately before writing the actual attached client input.
        start = time.perf_counter_ns()
        os.write(self.master, data)
        self.input_events.append({'input_ns': start, 'hex': data.hex()})
        return start

    def rows(self):
        return [int(m.group(1)) for line in self.screen.display for m in ROW.finditer(line)]

    def contains(self, text):
        return any(text in line for line in self.screen.display)

    def drain(self):
        deadline = time.monotonic() + 0.2
        while time.monotonic() < deadline:
            self.pump(0.01)

    def close(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        os.close(self.master)
        self.raw.close()
        (self.directory / 'input.json').write_text(json.dumps(self.input_events, indent=2))


def resource_sample(roots):
    """Idle server/client/shell process tree. RSS sums shared pages repeatedly."""
    processes = {}
    for path in Path('/proc').glob('[0-9]*'):
        try:
            stat = (path / 'stat').read_text().rsplit(')', 1)[1].split()
            processes[int(path.name)] = (int(stat[1]), int(stat[11]) + int(stat[12]))
        except (OSError, ValueError, IndexError):
            continue
    included = set(roots)
    while True:
        children = {pid for pid, (parent, _) in processes.items() if parent in included}
        if children <= included:
            break
        included |= children
    pss = rss = ticks = 0
    evidence = []
    for pid in sorted(included):
        if pid not in processes:
            continue
        values = {}
        try:
            memory_lines = Path(f'/proc/{pid}/smaps_rollup').read_text().splitlines()
        except (FileNotFoundError, ProcessLookupError):
            # Short-lived plugin/workload helpers may exit between /proc reads.
            # They are absent from this resident-memory snapshot and CPU lower bound.
            continue
        for line in memory_lines:
            key, _, value = line.partition(':')
            if key in ('Pss', 'Rss'):
                values[key] = int(value.split()[0])
        pss += values['Pss']
        rss += values['Rss']
        ticks += processes[pid][1]
        evidence.append({'pid': pid, **values})
    return {'pss_kib': pss, 'rss_kib': rss, 'cpu_ticks': ticks, 'processes': evidence}


def storage(directory):
    files = [p for p in directory.rglob('*') if p.is_file() and not p.is_symlink()]
    return {'storage_bytes': sum(p.stat().st_size for p in files),
            'storage_allocated_bytes': sum(p.stat().st_blocks * 512 for p in files)}


def trial(variant, number, output, args):
    directory = output / f'{number:03d}-{variant}'
    directory.mkdir()
    runtime = directory / 'runtime'
    state = directory / 'state'
    runtime.mkdir(mode=0o700)
    state.mkdir(mode=0o700)
    env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), XDG_STATE_HOME=str(state),
               XDG_CONFIG_HOME=str(directory / 'config'), SHELL=os.environ['BENCH_SHELL'],
               TERM='xterm-256color', COLORTERM='truecolor', LC_ALL='C.UTF-8',
               BASH_ENV='/dev/null', ENV='/dev/null', INPUTRC='/dev/null')
    for key in ('MUX', 'MUX_PANE', 'TMUX', 'TMUX_PANE'):
        env.pop(key, None)
    mux = str(SOURCE_ROOT / 'target/release/mux')
    socket = str(runtime / 'tmux.sock')
    tmux = ['tmux', '-S', socket]
    # Equal 100x40 pane content. tmux gets one extra row for continuum's
    # status-driven scheduler; mux's four-column strip gets extra width.
    if variant == 'mux':
        config = directory / 'mux.toml'
        capture = directory / 'history.txt'
        writer = directory / 'clipboard'
        writer.write_text('#!' + shutil.which('bash') + '\nset -eu\ncat > ' + shlex.quote(str(capture) + '.tmp') +
                          '\nmv ' + shlex.quote(str(capture) + '.tmp') + ' ' + shlex.quote(str(capture)) + '\n')
        writer.chmod(0o700)
        config.write_text('clipboard_command = ' + json.dumps([str(writer)]) + '\n[vim]\n"PageUp" = "half-page-up"\n')
        argv, rows, cols = [mux, '--config', str(config), '--session', 'bench'], 40, 105
    else:
        config = directory / 'tmux.conf'
        lines = ["set -g status on", "set -g status-interval 1", "set -g status-left ''",
                 "set -g status-right ''", "set -g history-limit 20000", "set -g mode-keys vi",
                 "set -g default-shell " + shlex.quote(env['SHELL']), "set -g escape-time 0",
                 "bind -T copy-mode-vi PageUp send-keys -X halfpage-up"]
        if variant == 'tmux-persistence':
            lines += ["set -g @resurrect-dir " + shlex.quote(str(state / 'resurrect')),
                      "set -g @resurrect-capture-pane-contents on",
                      "set -g @continuum-save-interval 15", "set -g @continuum-restore off",
                      "run-shell " + shlex.quote(os.environ['BENCH_RESURRECT']),
                      "run-shell " + shlex.quote(os.environ['BENCH_CONTINUUM'])]
        config.write_text('\n'.join(lines) + '\n')
        argv, rows, cols = tmux + ['-f', str(config), 'new-session', '-s', 'bench'], 41, 100
    result = {'variant': variant, 'trial': number, 'commands': [argv], 'correct': False,
              'started_monotonic_ns': time.perf_counter_ns(),
              'load_before': Path('/proc/loadavg').read_text(),
              'cpu_pressure_before': Path('/proc/pressure/cpu').read_text()}
    (directory / 'commands.json').write_text(json.dumps(result['commands'], indent=2))
    start = time.perf_counter_ns()
    terminal = Terminal(argv, env, directory, rows, cols)
    try:
        end = terminal.until(lambda: terminal.contains('BENCH_READY>'))
        result['startup_ms'] = (end - start) / 1e6
        terminal.input(b"stty -echo; printf 'DIM:'; stty size; PS1=''\n")
        terminal.until(lambda: any('DIM:' in line and re.search(r'40\s+\d+', line)
                                  for line in terminal.screen.display))
        dimension = next(re.search(r'DIM:40\s+(\d+)', line) for line in terminal.screen.display
                         if re.search(r'DIM:40\s+(\d+)', line))
        pane_cols = int(dimension.group(1))
        if pane_cols != 100 and variant == 'mux':
            terminal.size(rows, cols + 100 - pane_cols)
            terminal.drain()
            terminal.input(b"printf 'CHECK:'; stty size\n")
            terminal.until(lambda: any(re.search(r'CHECK:40\s+100', line) for line in terminal.screen.display))
        elif pane_cols != 100:
            raise RuntimeError('unequal pane content size')
        if variant == 'tmux-persistence':
            save_path = command(tmux + ['show-option', '-gqv', '@resurrect-save-script-path'], env)
            status = command(tmux + ['show-option', '-gqv', 'status-right'], env)
            if not save_path or 'continuum_save.sh' not in status:
                raise RuntimeError('persistence plugins are not actually loaded')
            result['plugin_gate'] = {'save_path': save_path, 'status_right': status}
        if variant == 'mux':
            # Find this isolated daemon by its exact socket argument.
            matches = [int(p.name) for p in Path('/proc').glob('[0-9]*')
                       if (p / 'cmdline').exists() and b'__server\0' in (p / 'cmdline').read_bytes()
                       and str(runtime / 'mux.sock').encode() in (p / 'cmdline').read_bytes()]
            if len(matches) != 1:
                raise RuntimeError('cannot identify isolated mux daemon')
            server = matches[0]
        else:
            server = int(command(tmux + ['display-message', '-p', '#{pid}'], env))
        roots = [server, terminal.process.pid]
        before = resource_sample(roots)
        workload = ['python3', str(ROOT / 'benchmarks/workload.py'), '--rows', str(args.rows)]
        result['commands'].append(workload)
        start = terminal.input((shlex.join(workload) + '\n').encode())
        def output_rendered():
            visible = terminal.rows()
            return (terminal.contains('BENCH_OUTPUT_DONE') and len(visible) >= 35
                    and visible == list(range(args.rows - len(visible), args.rows)))
        end = terminal.until(output_rendered, timeout=120)
        visible = terminal.rows()
        if len(visible) < 35 or visible != list(range(args.rows - len(visible), args.rows)):
            raise RuntimeError('output viewport failed: expected contiguous final workload rows')
        result['output_ms'] = (end - start) / 1e6
        result['output_mib_s'] = (args.rows * 82 + 19) / 2**20 / ((end - start) / 1e9)
        terminal.drain()
        after = resource_sample(roots)
        result.update(after)
        result['output_cpu_seconds'] = (after['cpu_ticks'] - before['cpu_ticks']) / os.sysconf('SC_CLK_TCK')
        idle_start = time.perf_counter()
        idle_before = resource_sample(roots)
        while time.perf_counter() - idle_start < args.idle_seconds:
            terminal.pump(0.05)
        idle_after = resource_sample(roots)
        result['idle_cpu_percent'] = 100 * (idle_after['cpu_ticks'] - idle_before['cpu_ticks']) / os.sysconf('SC_CLK_TCK') / (time.perf_counter() - idle_start)
        result.update(storage(state))
        # Mode entry is not itself a latency sample.
        terminal.input(b'\x1bw' if variant == 'mux' else b'\x02[')
        terminal.drain()
        terminal.input(b'\x1b[5~')
        terminal.until(lambda: len(terminal.rows()) == 40 and terminal.rows()[-1] < args.rows - 1)
        terminal.drain()
        scroll = []
        result['scroll_events'] = []
        for _ in range(args.scroll_samples):
            prior = terminal.rows()
            if len(prior) != 40:
                # Entry overlays/cursor placement can obscure a row: fail closed.
                raise RuntimeError('copy mode does not expose 40 complete workload rows')
            # Both explicitly bind PageUp to half-page-up: exactly 20 rows.
            target = [i - 20 for i in prior]
            if min(target) < 0:
                raise RuntimeError('workload history too short for scroll sample count')
            start = terminal.input(b'\x1b[5~')
            end = terminal.until(lambda: terminal.rows() == target)
            scroll.append((end - start) / 1e6)
            result['scroll_events'].append({'input_ns': start, 'decoded_viewport_ns': end,
                                           'before': prior, 'expected': target,
                                           'observed': terminal.rows()})
            terminal.drain()
        result['scroll_ms'] = scroll
        result['viewport_sha256'] = hashlib.sha256('\n'.join(terminal.screen.display).encode()).hexdigest()
        # Verify all generated history after measurements, so correctness
        # capture cannot change the measured resident-memory/scroll state.
        if variant == 'mux':
            terminal.input(b'\x1b')
            terminal.drain()
            terminal.input(b'\x1bwggVGy')
            terminal.until(lambda: capture.exists())
            history = capture.read_text()
        else:
            history = command(tmux + ['capture-pane', '-p', '-J', '-S', '-'], env)
            (directory / 'history.txt').write_text(history)
        found = list(ROW.finditer(history))
        if [int(m.group(1)) for m in found] != list(range(args.rows)) or history.count('ROW') != args.rows:
            raise RuntimeError('full history does not match the equal numbered workload')
        expected = '\n'.join(f'ROW{i:08d} ' + 'x' * 68 for i in range(args.rows))
        result['history_gate'] = {'rows': len(found), 'columns': 100, 'pane_rows': 40,
                                  'sha256': hashlib.sha256('\n'.join(m.group(0) for m in found).encode()).hexdigest(),
                                  'expected_sha256': hashlib.sha256(expected.encode()).hexdigest()}
        result['correct'] = True
    except Exception as error:
        result['error'] = str(error)
    finally:
        result['ended_monotonic_ns'] = time.perf_counter_ns()
        result['load_after'] = Path('/proc/loadavg').read_text()
        terminal.close()
        subprocess.run([mux, 'kill-server'] if variant == 'mux' else tmux + ['kill-server'],
                       env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        (directory / 'commands.json').write_text(json.dumps(result['commands'], indent=2))
        (directory / 'sample.json').write_text(json.dumps(result, indent=2))
    return result


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--trials', type=int, default=30)
    p.add_argument('--rows', type=int, default=10000)
    p.add_argument('--scroll-samples', type=int, default=50)
    p.add_argument('--idle-seconds', type=float, default=3)
    a = p.parse_args()
    if platform.system() != 'Linux' or not os.environ.get('BENCH_NIXPKGS_REV'):
        p.error('run on Linux inside nix develop .#benchmark')
    if a.trials < 20 or a.rows > 19000 or a.rows < 100 + 40 * a.scroll_samples:
        p.error('need >=20 trials and enough equal history, <=19000 rows')
    if command(['git', '-C', str(SOURCE_ROOT), 'rev-parse', 'HEAD']) != SOURCE_COMMIT:
        p.error('use scripts/benchmark-nix or supply the exact pinned source checkout')
    if command(['git', '-C', str(SOURCE_ROOT), 'diff', SOURCE_COMMIT, '--', 'src', 'vendor', 'Cargo.toml', 'Cargo.lock']):
        p.error('mux runtime differs from the benchmark source pin')
    a.output = a.output.resolve()
    a.output.mkdir(parents=True, exist_ok=False)
    environment = {'source_commit': SOURCE_COMMIT, 'harness_commit': command(['git', 'rev-parse', 'HEAD']),
                   'uname': platform.uname()._asdict(), 'versions': {}, 'args': vars(a) | {'output': str(a.output)},
                   'nixpkgs': os.environ['BENCH_NIXPKGS_REV'],
                   'flake_lock': json.loads((ROOT / '.nix/flake.lock').read_text()),
                   'execution_context': os.environ.get('BENCH_EXECUTION_CONTEXT', 'selected-cloud'),
                   'cpuinfo': Path('/proc/cpuinfo').read_text(), 'meminfo': Path('/proc/meminfo').read_text(),
                   'loadavg': Path('/proc/loadavg').read_text(),
                   'cgroup': Path('/proc/self/cgroup').read_text()}
    environment['runner'] = {key: os.environ.get(key) for key in
                             ('RUNNER_NAME', 'RUNNER_OS', 'RUNNER_ARCH', 'ImageOS', 'ImageVersion',
                              'GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT')}
    environment['mounts'] = Path('/proc/mounts').read_text()
    for tool in ('rustc', 'cargo', 'python3', 'tmux', 'bash'):
        environment['versions'][tool] = {'path': shutil.which(tool),
                                        'version': command([tool, '-V' if tool == 'tmux' else '--version'])}
        if not str(Path(environment['versions'][tool]['path']).resolve()).startswith('/nix/store/'):
            p.error(tool + ' must come from the Nix store')
    (a.output / 'environment.json').write_text(json.dumps(environment, indent=2))
    if not environment['versions']['rustc']['version'].startswith('rustc ' + os.environ['BENCH_RUST_VERSION']):
        p.error('Rust does not match the Nix shell pin')
    build = subprocess.run(['cargo', 'build', '--locked', '--release'], cwd=SOURCE_ROOT,
                           stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    (a.output / 'build.log').write_text(build.stdout)
    build.check_returncode()
    samples = []
    rng = random.Random(20260930)
    for number in range(a.trials + 1):
        variants = ['mux', 'tmux', 'tmux-persistence']
        rng.shuffle(variants)
        print('paired block', number, variants, flush=True)
        for variant in variants:
            sample = trial(variant, number, a.output, a)
            samples.append(sample)
            print(variant, number, 'PASS' if sample['correct'] else sample['error'], flush=True)
    # Trial zero is retained as a warm-up, but never included in aggregates.
    summaries = {}
    for variant in ('mux', 'tmux', 'tmux-persistence'):
        selected = [s for s in samples if s['variant'] == variant and s['trial'] > 0]
        if not all(s['correct'] for s in samples if s['variant'] == variant):
            summaries[variant] = {'blocked': 'correctness gate failed', 'failed': sum(not s['correct'] for s in selected)}
            continue
        metrics = {}
        for key in ('startup_ms', 'output_ms', 'output_mib_s', 'pss_kib', 'rss_kib',
                    'output_cpu_seconds', 'idle_cpu_percent', 'storage_bytes', 'storage_allocated_bytes', 'scroll_ms'):
            values = [v for s in selected for v in (s[key] if isinstance(s[key], list) else [s[key]])]
            ordered = sorted(values)
            metrics[key] = {'n': len(values), 'median': statistics.median(values),
                            'p95': ordered[math.ceil(0.95 * len(ordered)) - 1]}
        summaries[variant] = metrics
    (a.output / 'summary.json').write_text(json.dumps(summaries, indent=2))
    if any('blocked' in value for value in summaries.values()):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
