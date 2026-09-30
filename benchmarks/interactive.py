#!/usr/bin/env python3
"""Paired attached-PTY operation sweeps; gates visible state then metadata.

No CLI acknowledgement is a latency endpoint. CLI is used for preparation and
post-render verification only. Full failed trials remain and block aggregates.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import random
import shlex
import statistics
import subprocess
import time
import traceback

from run import ROOT, SOURCE_ROOT, SOURCE_COMMIT, Terminal, command, resource_sample
from recovery import Recovery, proc_identity

VARIANTS = ('mux', 'tmux', 'tmux-persistence')
SCALES = ((1, 1), (3, 2), (6, 4))


def statistics_of(values):
    values = sorted(values)
    return {'n': len(values), 'median': statistics.median(values),
            'p95': values[math.ceil(.95 * len(values)) - 1]}


class Interactive(Recovery):
    def __init__(self, variant, directory):
        super().__init__(variant, directory)
        self.actions = []
        self.attachments = 0
        self.secondary = None
        self.marker = 'BENCH_READY>'
        self.windows = 0
        if variant != 'mux':
            bindings = ['bind -n M-a switch-client -T benchmark',
                        'bind -n M-t new-window', 'bind -n M-T new-session',
                        'bind -n M-f resize-pane -Z', 'bind -n M-w copy-mode',
                        'bind -n M-s switch-client -T session-pick',
                        "bind -T session-pick 1 switch-client -t bench",
                        "bind -T session-pick 2 switch-client -t other",
                        "bind -T session-pick x kill-session",
                        'bind -T benchmark - split-window -v',
                        'bind -T benchmark | split-window -h',
                        'bind -T benchmark ! break-pane',
                        'bind -T benchmark x kill-pane',
                        'bind -T benchmark d detach-client',
                        'bind -T benchmark Left select-pane -L',
                        'bind -T benchmark Right select-pane -R',
                        'bind -T benchmark Up select-pane -U',
                        'bind -T benchmark Down select-pane -D',
                        'bind -T benchmark C-Left resize-pane -L 1',
                        'bind -T benchmark C-Right resize-pane -R 1',
                        'bind -T benchmark C-Up resize-pane -U 1',
                        'bind -T benchmark C-Down resize-pane -D 1',
                        "bind -T copy-mode-vi Escape cancel",
                        "bind -T copy-mode-vi g send-keys -X history-top",
                        "bind -T copy-mode-vi G send-keys -X history-bottom"]
            for n in range(1, 10):
                bindings.append(f'bind -n M-{n} select-window -t :{n}')
            with self.config.open('a') as f:
                f.write('\n'.join(bindings) + '\n')

    def panes(self):
        if self.variant == 'mux':
            return json.loads(self.cli('list-panes', '--json'))
        return [dict(id=x[0], index=int(x[1]), cols=int(x[2]), rows=int(x[3]), active=x[4] == '1')
                for x in (line.split('\t') for line in self.cli('list-panes', '-F',
                      '#{pane_id}\t#{pane_index}\t#{pane_width}\t#{pane_height}\t#{pane_active}').splitlines())]

    def window_count(self):
        if self.variant == 'mux':
            return len(json.loads(self.cli('list-windows', '--json')))
        return len(self.cli('list-windows', '-F', '#{window_id}').splitlines())

    def select_window(self, n):
        self.cli('select-window', str(n)) if self.variant == 'mux' else self.cli('select-window', '-t', f':{n}')

    def select_id(self, pane):
        if self.variant == 'mux':
            # Explicit target chooses that pane before the harmless naming command.
            self.cli('--pane', str(pane['id']), 'rename-window', self.current_name)
        else:
            self.cli('select-pane', '-t', str(pane['id']))

    def rename(self, name):
        self.current_name = name
        self.cli('rename-window', name)

    def shell(self, text, marker):
        self.client.input((text + '\n').encode())
        self.client.until(lambda: self.client.contains(marker), timeout=30)
        self.client.drain()

    def seed_pane(self, tag, rows):
        # Same tagged ASCII history per pane; frame is redrawn without scrolling
        # on SIGWINCH so a split/resize exposes an actual shell response.
        script = self.directory / ('pane-' + tag + '.sh')
        script.write_text("stty -echo\nPS1='" + tag + ">'\n" +
                         "printf '\\033[2J\\033[H'\n" +
                         f"for ((i=0;i<{rows};i++)); do printf '{tag}H%05d payload\\n' \"$i\"; done\n" +
                         "printf '" + tag + "FRAME\\n'\n")
        self.shell('source ' + shlex.quote(str(script)), tag + '>')

    def cursor_at(self, tag):
        sc = self.client.screen
        line = sc.display[sc.cursor.y]
        return tag + '>' in line[:sc.cursor.x + 1]

    def action(self, name, keys, predicate, verify=lambda: True):
        self.client.drain()
        if predicate():
            raise RuntimeError('action precondition already satisfied: ' + name)
        before = resource_sample((self.server, self.client.process.pid))
        before_frame = list(self.client.screen.display)
        start = self.client.input(keys)
        end = self.client.until(predicate, timeout=12)
        frame = list(self.client.screen.display)
        assert predicate() and (frame != before_frame or name.startswith('focus_'))
        evidence = verify()
        if not evidence:
            raise RuntimeError('post-render semantic gate failed: ' + name)
        after = resource_sample((self.server, self.client.process.pid))
        self.actions.append({'operation': name, 'input_ns': start, 'decoded_ns': end,
                             'latency_ms': (end - start) / 1e6, 'keys_hex': keys.hex(),
                             'before_frame': before_frame, 'after_frame': frame,
                             'semantic_evidence': evidence, 'resources_before': before,
                             'resources_after': after,
                             'cpu_tick_delta_lower_bound': after['cpu_ticks'] - before['cpu_ticks']})

    def attachment(self, name, secondary=False):
        self.attachments += 1
        directory = self.directory / ('attach-' + str(self.attachments))
        directory.mkdir()
        argv = ([self.mux, '--config', str(self.config), '--session', 'bench'] if self.variant == 'mux'
                else self.tmux + ['attach-session', '-t', 'bench'])
        start = time.perf_counter_ns()
        term = Terminal(argv, self.env, directory, 40 if self.variant == 'mux' else 41, self.columns)
        end = term.until(lambda: term.contains('B1P1>'))
        self.actions.append({'operation': name, 'input_ns': start, 'decoded_ns': end,
                             'latency_ms': (end - start) / 1e6, 'launch': argv,
                             'after_frame': list(term.screen.display), 'semantic_evidence': 'B1P1 visible'})
        if secondary:
            self.secondary = term
        else:
            self.client = term

    def prepare(self, windows, panes, history, busy):
        self.launch()
        # Normalize content before splits; sidebar width is measured, not assumed.
        width = self.panes()[0]['cols']
        if self.variant == 'mux' and width != 100:
            self.columns += 100 - width
            self.client.size(40, self.columns)
            self.wait(lambda: self.panes()[0]['cols'] == 100)
        assert self.panes()[0]['rows'] == 40 and self.panes()[0]['cols'] == 100
        # Other session is a one-pane reference, same for all variants.
        self.cli('new-session', '-s', 'other') if self.variant == 'mux' else self.cli('new-session', '-d', '-s', 'other')
        if self.variant != 'mux':
            self.cli('switch-client', '-t', 'other')
        self.seed_pane('OTHER', history)
        self.client.input(b'\x1bs1')
        self.client.until(lambda: not self.client.contains('OTHER>') and self.client.contains('BENCH_READY>'))
        self.geometry = []
        for w in range(1, windows + 2):
            if w > 1:
                self.cli('new-window')
            self.rename('W' + str(w))
            if w <= windows:
                # Balanced two-row layout; four panes use another split in each row.
                if panes >= 2:
                    self.cli('split-window', '-v')
                if panes == 4:
                    self.cli('split-window', '-h')
                    self.cli('select-pane', '-U')
                    self.cli('split-window', '-h')
            current = self.panes()
            self.geometry.append([{k: p[k] for k in ('index', 'cols', 'rows')} for p in current])
            for p in current:
                self.select_id(p)
                self.seed_pane(f'B{w}P{p["index"]}', history)
            if w == windows + 1 and busy:
                script = self.directory / 'busy.py'
                script.write_text("import time,sys\nfor i in range(100000):\n sys.stdout.write('\\x1b[H'+'BACKGROUND%08d'%i+'x'*60+'\\n'+'x'*80+'\\n');sys.stdout.flush();time.sleep(.02)\n")
                self.client.input(('python3 ' + shlex.quote(str(script)) + '\n').encode())
        self.windows = windows + 1
        self.select_window(1)
        self.rename('W1')
        self.select_id(self.panes()[0])
        self.client.until(lambda: self.client.contains('B1P1>'))
        self.client.drain()
        self.start_resources = resource_sample((self.server, self.client.process.pid))
        # Reattach startup to an already populated layout is distinct from cold
        # daemon startup and from persistence restart.
        self.client.close()
        self.attachment('populated_attach_startup')

    def sweep(self, windows, panes):
        target = windows + 1
        self.action('window_switch_away', f'\x1b{target}'.encode(), lambda: not self.client.contains('B1P1>'),
                    lambda: self.window_count() == self.windows)
        self.action('window_switch_back', b'\x1b1', lambda: self.client.contains('B1P1>'))
        self.action('session_switch_away', b'\x1bs2', lambda: self.client.contains('OTHER>'))
        self.action('session_switch_back', b'\x1bs1', lambda: self.client.contains('B1P1>'))
        self.attachment('second_client_attach', secondary=True)
        self.secondary.close()
        self.secondary = None
        # Temporary window with one full-size pane makes mutations equivalent at
        # every scale without changing the populated fixture behind it.
        n = self.window_count()
        self.action('create_window', b'\x1bt', lambda: self.client.contains('BENCH_READY>') and not self.client.contains('B1P1>'),
                    lambda: self.window_count() == n + 1)
        self.seed_pane('TEMP', 100)
        for orientation, key in [('vertical', b'-'), ('horizontal', b'|')]:
            self.action('split_' + orientation, b'\x1ba' + key,
                        lambda: self.client.contains('TEMP>') and self.client.contains('BENCH_READY>'),
                        lambda: len(self.panes()) == 2)
            self.seed_pane('NEW', 100)
            direction = b'\x1b[A' if orientation == 'vertical' else b'\x1b[D'
            back = b'\x1b[B' if orientation == 'vertical' else b'\x1b[C'
            self.action('focus_' + orientation + '_first', b'\x1ba' + direction,
                        lambda: self.cursor_at('TEMP'), lambda: self.panes()[0]['active'])
            self.action('focus_' + orientation + '_second', b'\x1ba' + back,
                        lambda: self.cursor_at('NEW'), lambda: self.panes()[1]['active'])
            # Divider motion must produce new pane geometry and a visible redraw.
            before = list(self.client.screen.display)
            sizes = [(p['cols'], p['rows']) for p in self.panes()]
            resize = b'\x1b[1;5A' if orientation == 'vertical' else b'\x1b[1;5D'
            border = (19, 0) if orientation == 'vertical' else (0, 49)
            by, bx = border
            if self.variant == 'mux':
                bx += self.columns - 100
            old_border = self.client.screen.buffer[by][bx].data
            # Divider must leave its old coordinate. Leader popup alone cannot
            # end this sample; semantic dimensions are checked afterward.
            self.action('resize_' + orientation, b'\x1ba' + resize,
                        lambda: self.client.screen.buffer[by][bx].data != old_border,
                        lambda: [(p['cols'], p['rows']) for p in self.panes()] != sizes)
            # mux stays in repeat leader on resize; Escape exits it. tmux keytable
            # returns after its command; Escape is harmless shell input.
            self.client.input(b'\x1b')
            self.client.drain()
            self.action('zoom_in_' + orientation, b'\x1bf', lambda: self.client.contains('NEW>') and not self.client.contains('TEMP>'))
            self.action('zoom_out_' + orientation, b'\x1bf', lambda: self.client.contains('NEW>') and self.client.contains('TEMP>'))
            keys = b'\x1baxy' if self.variant == 'mux' else b'\x1bax'
            self.action('delete_pane_' + orientation, keys,
                        lambda: self.client.contains('TEMP>') and not self.client.contains('NEW>'),
                        lambda: len(self.panes()) == 1)
        # Break preserves a live tagged shell and creates a new visible window.
        self.client.input(b'\x1ba-')
        self.client.until(lambda: self.client.contains('BENCH_READY>'))
        self.seed_pane('MOVE', 100)
        n = self.window_count()
        self.action('break_pane_into_window', b'\x1ba!', lambda: self.client.contains('MOVE>') and not self.client.contains('TEMP>'),
                    lambda: self.window_count() == n + 1 and len(self.panes()) == 1)
        keys = b'\x1baxy' if self.variant == 'mux' else b'\x1bax'
        self.action('delete_window_last_pane', keys, lambda: not self.client.contains('MOVE>'), lambda: self.window_count() == n)
        self.select_window(n)
        self.client.until(lambda: self.client.contains('TEMP>'))
        self.action('delete_window', keys, lambda: not self.client.contains('TEMP>'), lambda: self.window_count() == n - 1)
        self.select_window(1)
        self.client.until(lambda: self.client.contains('B1P1>'))
        # History endpoints must be pane content, never a command acknowledgement.
        self.action('history_top', b'\x1bwgg', lambda: any('B1P1H00000' in line for line in self.client.screen.display[:40]))
        self.action('history_bottom', b'G', lambda: any('B1P1H00999' in line for line in self.client.screen.display[:40]))
        self.client.input(b'gg')
        self.client.drain()
        self.action('history_search', b'/B1P1H00500\r', lambda: any('B1P1H00500 payload' in line for line in self.client.screen.display[:39]))
        self.client.input(b'\x1b')
        self.client.drain()
        # New session is named by implementation; verify counts rather than a
        # guessed default name, then delete it through the actual session UI.
        self.action('create_session', b'\x1bT', lambda: self.client.contains('BENCH_READY>') and not self.client.contains('B1P1>'))
        if self.variant == 'mux':
            self.client.input(b'\x1bs')
            self.client.drain()
            self.action('delete_session', b'xy', lambda: self.client.contains('B1P1>') or self.client.contains('OTHER>'))
        else:
            self.action('delete_session', b'\x1bsx', lambda: self.client.contains('B1P1>') or self.client.contains('OTHER>'))
        self.client.input(b'\x1bs1')
        self.client.until(lambda: self.client.contains('B1P1>'))
        self.client.drain()
        # Return to original layout before client lifecycle measurements.
        if self.variant == 'mux':
            self.client.input(b'\x1bad')
        else:
            self.client.input(b'\x1bad')
        self.client.process.wait(timeout=10)
        self.client.close()
        self.attachment('reattach_after_detach')

    def cleanup(self):
        if self.secondary:
            self.secondary.close()
        super().cleanup()


def trial(variant, number, scale, busy, history, output):
    directory = output / f'{scale[0]}w{scale[1]}p-{busy}-{number:03d}-{variant}'
    r = Interactive(variant, directory)
    result = dict(variant=variant, trial=number, scale=list(scale), busy=busy, history=history,
                  correct=False, started_ns=time.perf_counter_ns())
    try:
        r.prepare(*scale, history, busy)
        result['initial_geometry'] = r.geometry
        result['initial_resources'] = r.start_resources
        r.sweep(*scale)
        result['correct'] = True
    except Exception as e:
        result.update(error=str(e), traceback=traceback.format_exc())
    finally:
        result['actions'] = r.actions
        try:
            r.cleanup()
        except Exception as e:
            result.update(correct=False, cleanup_error=str(e))
        result['ended_ns'] = time.perf_counter_ns()
        (directory / 'sample.json').write_text(json.dumps(result, indent=2))
    return result


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--output', required=True, type=Path)
    p.add_argument('--trials', type=int, default=20)
    p.add_argument('--smoke', action='store_true')
    a = p.parse_args()
    if not os.environ.get('BENCH_NIXPKGS_REV') or (a.trials < 20 and not a.smoke):
        p.error('requires pinned Nix and >=20 trials (or explicitly diagnostic --smoke)')
    assert command(['git', '-C', str(SOURCE_ROOT), 'rev-parse', 'HEAD']) == SOURCE_COMMIT
    a.output.mkdir(parents=True, exist_ok=False)
    env = dict(source_commit=SOURCE_COMMIT, harness_commit=command(['git', 'rev-parse', 'HEAD']),
               execution_context='hosted-ci-interactive-diagnostic' if a.smoke else 'hosted-ci-interactive-paired',
               args=vars(a) | {'output': str(a.output)}, nixpkgs=os.environ['BENCH_NIXPKGS_REV'],
               versions={t:command([t,'-V' if t=='tmux' else '--version']) for t in ('rustc','cargo','tmux','python3','bash')},
               cpuinfo=Path('/proc/cpuinfo').read_text(), meminfo=Path('/proc/meminfo').read_text(),
               uname=command(['uname','-a']), loadavg=Path('/proc/loadavg').read_text(),
               runner={k:os.environ.get(k) for k in ('ImageVersion','GITHUB_RUN_ID','RUNNER_NAME')},
               lock=json.loads((ROOT/'.nix/flake.lock').read_text()))
    (a.output/'environment.json').write_text(json.dumps(env,indent=2))
    with (a.output/'build.log').open('w') as log:
        subprocess.run(['cargo','build','--release','--locked'],cwd=SOURCE_ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
    rng=random.Random(20261001)
    samples=[]
    for scale in (SCALES[:1] if a.smoke else SCALES):
        for busy in (False, True):
            for n in range(1 if a.smoke else a.trials+1):
                variants=list(VARIANTS);rng.shuffle(variants)
                for variant in variants:
                    s=trial(variant,n,scale,busy,1000,a.output);samples.append(s)
                    print(scale,busy,n,variant,'PASS' if s['correct'] else s.get('error'),flush=True)
    (a.output/'samples.json').write_text(json.dumps(samples,indent=2))
    summaries={}
    if not a.smoke:
        for scale in SCALES:
            for busy in (False,True):
                group=[s for s in samples if s['scale']==list(scale) and s['busy']==busy]
                geometry=[s.get('initial_geometry') for s in group]
                valid=all(s['correct'] for s in group) and all(g==geometry[0] for g in geometry)
                for v in VARIANTS:
                    selected=[s for s in group if s['variant']==v and s['trial']>0]
                    key=f'{scale[0]}w{scale[1]}p/{busy}/{v}'
                    summaries[key]={'correct':valid,'trials':len(selected)}
                    if valid:
                        names=[a['operation'] for a in selected[0]['actions']]
                        assert all([a['operation'] for a in s['actions']]==names for s in selected)
                        summaries[key]['operations']={name:statistics_of([a['latency_ms'] for s in selected for a in s['actions'] if a['operation']==name]) for name in names}
                        summaries[key]['pss_kib']=statistics_of([s['initial_resources']['pss_kib'] for s in selected])
    (a.output/'summary.json').write_text(json.dumps(summaries,indent=2))
    if any(not s['correct'] for s in samples) or any(not s['correct'] for s in summaries.values()):
        raise SystemExit(1)

if __name__ == '__main__':
    main()
