#!/usr/bin/env python3
"""Attached-key action latency; metadata gates run AFTER visible endpoint."""
import argparse
import json
import os
from pathlib import Path
import random
import shlex
import statistics
import subprocess
import time
import traceback

from run import Terminal, ROOT, SOURCE_ROOT, SOURCE_COMMIT, command, resource_sample
from recovery import Recovery
from audit_hosted import stats

VARIANTS = ('mux', 'tmux', 'tmux-persistence')
SCALES = ((1, 1), (3, 2), (6, 4))


class Interactive(Recovery):
    def __init__(self, variant, directory):
        super().__init__(variant, directory)
        self.measurements = []
        self.attach_number = 0
        if variant != 'mux':
            bindings = ['set -g detach-on-destroy off', 'bind -n M-a switch-client -T bench',
                        'bind -n M-t new-window', 'bind -n M-T new-session',
                        'bind -n M-f resize-pane -Z',
                        'bind -n M-s switch-client -l',
                        'bind -n M-w copy-mode',
                        'bind -T copy-mode-vi g send-keys -X history-top',
                        'bind -T copy-mode-vi G send-keys -X history-bottom',
                        'bind -T copy-mode-vi Escape send-keys -X cancel',
                        'bind -T bench - split-window -v -l 50%', 'bind -T bench | split-window -h -l 50%',
                        'bind -T bench x confirm-before -p "kill pane?" kill-pane', 'bind -T bench ! break-pane',
                        'bind -T bench > swap-window -d -t +1', 'bind -T bench < swap-window -d -t -1',
                        'bind -T bench d detach-client',
                        'bind -T bench K confirm-before -p "kill session?" kill-session',
                        'bind -T bench , command-prompt -p "rename window:" "rename-window %%"',
                        'bind -T bench $ command-prompt -p "rename session:" "rename-session %%"',
                        'bind -T bench Left select-pane -L', 'bind -T bench Right select-pane -R',
                        'bind -T bench Up select-pane -U', 'bind -T bench Down select-pane -D',
                        'bind -T bench C-Right resize-pane -R 2',
                        'bind -T bench C-Left resize-pane -L 2',
                        'bind -T bench C-Up resize-pane -U 2',
                        'bind -T bench C-Down resize-pane -D 2']
            pipe='cat > '+shlex.quote(str(self.capture)+'.tmp')+' && mv '+shlex.quote(str(self.capture)+'.tmp')+' '+shlex.quote(str(self.capture))
            bindings += ['bind -T copy-mode-vi V send-keys -X select-line',
                         'bind -T copy-mode-vi y send-keys -X copy-pipe-and-cancel '+shlex.quote(pipe)]
            bindings += [f'bind -n M-{i} select-window -t :{i}' for i in range(1, 10)]
            with self.config.open('a') as f:
                f.write('\n' + '\n'.join(bindings) + '\n')

    def panes(self):
        if self.variant == 'mux':
            return json.loads(self.cli('list-panes', '--json'))
        raw = self.cli('list-panes', '-F', '#{pane_id}\t#{pane_index}\t#{pane_width}\t#{pane_height}\t#{pane_active}')
        return [dict(id=x[0], index=int(x[1]), cols=int(x[2]), rows=int(x[3]), active=x[4]=='1')
                for x in (line.split('\t') for line in raw.splitlines())]

    def windows(self):
        if self.variant == 'mux':
            return json.loads(self.cli('list-windows', '--json'))
        return [dict(index=int(x[0]), name=x[1], active=x[2]=='1') for x in
                (line.split('\t') for line in self.cli('list-windows', '-F', '#{window_index}\t#{window_name}\t#{window_active}').splitlines())]

    def active(self):
        return next(p for p in self.panes() if p['active'])

    def choose(self, pane):
        if self.variant == 'mux':
            self.cli('rename-window', 'fixture', '--pane', str(pane['id']))
        else:
            self.cli('select-pane', '-t', pane['id'])
            self.cli('rename-window', 'fixture')
        self.client.drain(0.02)

    def window(self, index):
        self.cli('select-window', str(index)) if self.variant == 'mux' else self.cli('select-window', '-t', f':{index}')
        self.client.drain(0.02)

    def shell_marker(self, label, rows):
        # Each retained row is 30 cells, fitting the smallest seeded pane.
        script = "stty -echo; PS1='" + label + "> '; printf '\\033[2J\\033[H'; python3 -c " + shlex.quote(
            f"print('\\n'.join('{label}-H%05d '%i+'x'*12 for i in range({rows})))")
        self.shell(script, f'{label}-H{rows-1:05d} '+ 'x'*12)
        self.client.until(lambda:self.client.contains(label+'>'))

    def shell(self, text, marker):
        self.client.input((text+'\n').encode())
        self.client.until(lambda:self.client.contains(marker))
        # shell_marker separately waits for the fresh tagged Bash prompt.

    def keys(self, data):
        self.client.input(data)
        # A lone Escape requires the input decoder's disambiguation window.
        self.client.drain(0.06 if data==b'\x1b' else 0.02)

    def action(self, name, data, predicate, check=None, terminal=None):
        t = terminal or self.client
        t.drain(0.02)
        before = list(t.screen.display)
        # Reject a pre-satisfied gate: the endpoint must observe a transition.
        if predicate():
            raise RuntimeError(name + ': visible gate already true before input')
        cpu_before = resource_sample([self.server, self.client.process.pid])
        start = t.input(data)
        end = t.until(predicate, timeout=10)
        evidence = {'name': name, 'input_ns': start, 'decoded_ns': end,
                    'latency_ms': (end-start)/1e6, 'input_hex': data.hex(),
                    'before': before, 'after': list(t.screen.display),
                    'cursor_after': [t.screen.cursor.x, t.screen.cursor.y]}
        cpu_after = resource_sample([self.server, self.client.process.pid])
        evidence['cpu_ticks_delta_lower_bound'] = cpu_after['cpu_ticks']-cpu_before['cpu_ticks']
        evidence['post_resource'] = cpu_after
        if check:
            evidence['metadata'] = check()
        evidence['correct'] = True
        self.measurements.append(evidence)
        if '-000-' in self.directory.name:
            print(self.directory.name, name, 'visible and semantic gate PASS', flush=True)
        return evidence

    def attach(self, name, expected, second=False):
        directory = self.directory / f'attach-{self.attach_number}'
        directory.mkdir(); self.attach_number += 1
        argv = ([self.mux, '--config', str(self.config), '--session', 'bench'] if self.variant=='mux'
                else self.tmux + ['attach-session', '-t', 'bench'])
        start = time.perf_counter_ns()
        t = Terminal(argv, self.env, directory, 40 if self.variant=='mux' else 41, self.columns)
        end = t.until(lambda: all(t.contains(x) for x in expected))
        self.measurements.append({'name': name, 'launch': argv, 'input_ns': start, 'decoded_ns': end,
                                  'latency_ms': (end-start)/1e6, 'correct': True,
                                  'after': list(t.screen.display), 'post_resource': resource_sample([self.server,t.process.pid]+([self.client.process.pid] if second else []))})
        if not second:
            self.client = t
        return t

    def bar(self):
        screen=self.client.screen
        cells=((y,x) for y in range(2,screen.lines) for x in range(min(5,screen.columns))) if self.variant=='mux' else ((screen.lines-1,x) for x in range(screen.columns))
        return [tuple(screen.buffer[y][x]) for y,x in cells]

    def selected_window(self):
        window=next(w for w in self.windows() if w['active'])
        return window.get('id',window.get('index'))

    def visible_selected_window(self, number, count, name):
        if self.variant=='mux':
            row=(self.client.screen.lines-2-count*3)//2+(number-1)*3+1
            return self.client.screen.buffer[row][1].data=='•'
        return f'{number}:{name}*' in self.client.screen.display[-1]

    def count_gate(self, expected):
        panes = self.panes()
        assert len(panes)==expected, (expected, panes)
        return panes


def exercise(variant, trial, windows, pane_count, load, output, history_rows):
    r=Interactive(variant, output / f'w{windows}-p{pane_count}-{load}-{trial:03d}-{variant}')
    result={'variant': variant, 'trial': trial, 'windows': windows, 'panes_per_window':pane_count,
            'load':load, 'correct':False, 'started_ns':time.perf_counter_ns(), 'history_rows_per_pane':history_rows}
    try:
        r.launch()
        first=r.panes()[0]
        if first['cols']!=100 and variant=='mux':
            r.columns += 100-first['cols']; r.client.size(40,r.columns)
            r.wait(lambda:r.panes()[0]['cols']==100)
        assert r.panes()[0]['rows']==40 and r.panes()[0]['cols']==100
        fixture=[]
        for w in range(1,windows+1):
            if w>1:
                r.cli('new-window')
            if pane_count>=2:
                r.cli('split-window','-v') if variant=='mux' else r.cli('split-window','-v','-l','50%')
            if pane_count==4:
                r.cli('split-window','-h') if variant=='mux' else r.cli('split-window','-h','-l','50%')
                r.cli('select-pane','-U')
                r.cli('split-window','-h') if variant=='mux' else r.cli('split-window','-h','-l','50%')
            panes=r.panes()
            assert len(panes)==pane_count
            for p in panes:
                r.choose(p);label=f'W{w:02d}P{p["index"]:02d}'
                r.shell_marker(label,history_rows)
                # Full seed history is an untimed equivalence gate.
                if variant=='mux':
                    if r.capture.exists():r.capture.unlink()
                    r.keys(b'\x1bw');r.client.input(b'ggVGy')
                    r.client.until(lambda:r.capture.exists())
                    history=r.capture.read_text();r.keys(b'\x1b')
                else:
                    history=r.cli('capture-pane','-p','-J','-S','-')
                import re, hashlib
                records=re.findall(label+r'-H\d{5} x{12}',history)
                assert records==[f'{label}-H{i:05d} '+ 'x'*12 for i in range(history_rows)], (label,len(records))
                p['history_sha256']=hashlib.sha256('\n'.join(records).encode()).hexdigest()
                (r.directory/(label+'-history.txt')).write_text(history)
            fixture.append({'window':w,'panes':panes})
        # A matched extra window isolates background activity in both profiles.
        r.cli('new-window');r.cli('rename-window','BACKGROUND');r.shell_marker('BACKGROUND',history_rows)
        if load=='busy':
            script="import sys,time; i=0\nwhile True:\n sys.stdout.write('\\033[H'+('LOAD%08d'%i+'x'*60+'\\n')*5);sys.stdout.flush();i+=1;time.sleep(.02)"
            r.client.input(('python3 -u -c '+shlex.quote(script)+'\n').encode());r.client.drain(0.02)
        r.window(1);panes=r.panes();r.choose(panes[0])
        labels=[f'W01P{p["index"]:02d}>' for p in panes]
        r.client.until(lambda:all(r.client.contains(x) for x in labels))
        result['fixture']=fixture
        result['outer_columns']=r.columns
        result['resource_before']=resource_sample([r.server,r.client.process.pid])
        # Startup with retained, populated, matched window/pane layout = attach.
        r.client.close();r.client=None
        r.attach('populated_attach_startup',labels)
        # Dedicated scratch window keeps every action sequence identical at all scales.
        r.action('create_window',b'\x1bt',lambda:r.client.contains('BENCH_READY>'),lambda:r.windows())
        scratch=len(r.windows());r.shell_marker('SCRATCH',history_rows)
        r.action('switch_window',b'\x1b1',lambda:all(r.client.contains(x) for x in labels))
        r.action('switch_window_back',f'\x1b{scratch}'.encode(),lambda:r.client.contains('SCRATCH>'))
        # Two split orientations, full lifecycle, directional focus and zoom.
        for orientation,key in [('vertical',b'-'),('horizontal',b'|')]:
            r.action('split_'+orientation,b'\x1ba'+key,lambda:r.client.contains('BENCH_READY>'),lambda:r.count_gate(2))
            r.shell_marker('CHILD',history_rows)
            child=r.active();parent=next(p for p in r.panes() if not p['active'])
            # Shell prompts can fall out of the retained viewport after a split.
            # Repaint the survivor outside timing before focus/zoom operations.
            r.choose(parent)
            r.client.input(b"printf '\\033[2J\\033[H'\n")
            r.client.until(lambda:r.client.contains('SCRATCH>'))
            r.choose(child)
            # Cursor location gives a visible focused-pane endpoint.
            direction=b'\x1b[A' if orientation=='vertical' else b'\x1b[D'
            parent_row=next(y for y,line in enumerate(r.client.screen.display) if 'SCRATCH>' in line)
            parent_x=next(line.index('SCRATCH>') for line in r.client.screen.display if 'SCRATCH>' in line)+len('SCRATCH> ')
            def focus_gate():
                active=r.active();assert active['id']==parent['id'];return active
            r.action('focus_'+orientation,b'\x1ba'+direction,
                     lambda:r.client.screen.cursor.y==parent_row and r.client.screen.cursor.x==parent_x,
                     focus_gate)
            child_row=next(y for y,line in enumerate(r.client.screen.display) if 'CHILD>' in line)
            child_x=next(line.index('CHILD>') for line in r.client.screen.display if 'CHILD>' in line)+len('CHILD> ')
            opposite=b'\x1b[B' if orientation=='vertical' else b'\x1b[C'
            def child_focus_gate():
                active=r.active();assert active['id']==child['id'];return active
            r.action('focus_'+orientation+'_back',b'\x1ba'+opposite,lambda:r.client.screen.cursor.y==child_row and r.client.screen.cursor.x==child_x,child_focus_gate)
            old=list(r.client.screen.display)
            resize=b'\x1b[1;5A' if orientation=='vertical' else b'\x1b[1;5D'
            old_dims=[(p['cols'],p['rows']) for p in r.panes()]
            def resized():
                p=r.panes();assert [(x['cols'],x['rows']) for x in p]!=old_dims;return p
            offset=r.columns-100 if variant=='mux' else 0
            by,bx=(next(y for y,line in enumerate(old) if '─'*40 in line or 'q'*40 in line),offset+10) if orientation=='vertical' else (0,offset+parent['cols'])
            new_y,new_x=(by-2,bx) if orientation=='vertical' else (by,bx-2)
            old_border=r.client.screen.buffer[by][bx].data
            def visibly_resized():
                screen=r.client.screen
                return screen.buffer[by][bx].data!=old_border and screen.buffer[new_y][new_x].data==old_border
            r.action('resize_'+orientation,b'\x1ba'+resize,visibly_resized,resized)
            # mux retains leader after resize. Cancel equally outside timing.
            if variant=='mux':r.keys(b'\x1b')
            r.action('zoom_'+orientation,b'\x1bf',lambda:not r.client.contains('SCRATCH>') and r.client.contains('CHILD>'))
            r.action('unzoom_'+orientation,b'\x1bf',lambda:r.client.contains('SCRATCH>') and r.client.contains('CHILD>'))
            r.keys(b'\x1bax') # confirmation dialog, identical prepared commit unit
            kill=b'y'
            r.action('delete_pane_'+orientation,kill,lambda:not r.client.contains('CHILD>') and r.client.contains('SCRATCH>'),lambda:r.count_gate(1))
        # Window reordering and break-pane movement: markers verify the actual object.
        r.action('split_for_break',b'\x1ba-',lambda:r.client.contains('BENCH_READY>'),lambda:r.count_gate(2))
        r.shell_marker('MOVED',history_rows)
        r.action('break_pane',b'\x1ba!',lambda:r.client.contains('MOVED>') and not r.client.contains('SCRATCH>'),lambda:r.count_gate(1))
        moved=len(r.windows())
        # Delete the moved pane's single-pane window using the same lifecycle key.
        r.keys(b'\x1bax');kill=b'y'
        r.action('delete_window',kill,lambda:not r.client.contains('MOVED>') and r.client.contains('SCRATCH>'),lambda:r.windows())
        r.window(scratch)
        # Swap into previous slot; visible sidebar/status index is checked post endpoint.
        before=r.bar();old_index=r.selected_window()
        count=len(r.windows());selected=next(w for w in r.windows() if w['active'])
        name=selected.get('name',selected.get('label','fixture'))
        def moved_left():
            assert r.selected_window()==old_index-1
            return r.windows()
        r.action('reorder_window_left',b'\x1ba<',lambda:r.visible_selected_window(old_index-1,count,name),moved_left)
        before=r.bar()
        def moved_right():
            assert r.selected_window()==old_index
            return r.windows()
        r.action('reorder_window_right',b'\x1ba>',lambda:r.visible_selected_window(old_index,count,name),moved_right)
        # Rename editors are real UI operations. Editor disappearance alone
        # is supplemented by exact accepted names, never an acknowledgement.
        r.action('open_window_rename',b'\x1ba,',lambda:r.client.contains('rename window:'))
        def renamed_window():
            current=next(w for w in r.windows() if w['active'])
            assert current['name']=='BENCH_RENAMED';return current
        r.action('commit_window_rename',b'\x15BENCH_RENAMED\r',lambda:not r.client.contains('rename window:') and r.client.contains('SCRATCH>'),renamed_window)
        r.action('open_session_rename',b'\x1ba$',lambda:r.client.contains('rename session:'))
        def renamed_session():
            if variant=='mux':
                sessions=json.loads(r.cli('list-sessions','--json'))
                assert any(s['current'] and s['name']=='BENCH_RENAMED_SESSION' for s in sessions)
                return sessions
            name=r.cli('display-message','-p','#{session_name}')
            assert name=='BENCH_RENAMED_SESSION';return name
        r.action('commit_session_rename',b'\x15BENCH_RENAMED_SESSION\r',lambda:not r.client.contains('rename session:') and r.client.contains('SCRATCH>'),renamed_session)
        r.cli('rename-session','bench')
        # New session lifecycle and previous-session switching.
        r.action('create_session',b'\x1bT',lambda:r.client.contains('BENCH_READY>'))
        r.shell_marker('SECOND_SESSION',history_rows)
        switch=b'\x1bs\x1bs' if variant=='mux' else b'\x1bs'
        r.action('switch_session',switch,lambda:r.client.contains('SCRATCH>'))
        r.action('switch_session_back',switch,lambda:r.client.contains('SECOND_SESSION>'))
        if variant=='mux':
            r.keys(b'\x1bs');r.keys(b'x');kill=b'y\x1b'
        else:
            # Match a prepared native confirmation dialog for both backends.
            r.keys(b'\x1baK')
            kill=b'y'
        offset=r.columns-100 if variant=='mux' else 0
        r.action('delete_session',kill,lambda:any('SCRATCH>' in line and line.index('SCRATCH>')==offset for line in r.client.screen.display))
        # Native TIOCSWINSZ + SIGWINCH: stop at the new visible geometry,
        # not a CLI acknowledgement or a shell probe with a settle delay.
        selected=next(w for w in r.windows() if w['active'])
        index=r.selected_window();count=len(r.windows());name=selected['name']
        before=list(r.client.screen.display)
        start=time.perf_counter_ns();r.client.size(44 if variant=='mux' else 45,r.columns)
        assert not r.visible_selected_window(index,count,name)
        end=r.client.until(lambda:r.visible_selected_window(index,count,name) and r.client.contains('SCRATCH>'))
        dimensions=r.active();assert dimensions['rows']==44 and dimensions['cols']==100,dimensions
        r.measurements.append({'name':'terminal_resize_viewport','latency_ms':(end-start)/1e6,'input_ns':start,'decoded_ns':end,'correct':True,'before':before,'after':list(r.client.screen.display),'dimensions_after':dimensions,'resize':{'content_rows':44,'content_columns':100}})
        r.client.size(40 if variant=='mux' else 41,r.columns)
        r.client.until(lambda:r.visible_selected_window(index,count,name))
        r.shell_marker('SCRATCH',history_rows)
        # Search a retained record far outside the live viewport.
        before=list(r.client.screen.display)
        tile_before=tuple(r.client.screen.buffer[0][1])
        r.action('enter_copy_mode',b'\x1bw',lambda:tuple(r.client.screen.buffer[0][1])!=tile_before if variant=='mux' else r.client.screen.display!=before)
        r.action('history_search_backward',b'?SCRATCH-H00100\r',lambda:r.client.contains('SCRATCH-H00100 '+'x'*12))
        before=list(r.client.screen.display)
        r.action('history_top',b'gg' if variant=='mux' else b'g',lambda:r.client.contains('SCRATCH-H00000'))
        x=r.client.screen.cursor.x;y=r.client.screen.cursor.y
        r.action('copy_cursor_right',b'l',lambda:r.client.screen.cursor.x==x+1 and r.client.screen.cursor.y==y)
        r.action('copy_cursor_left',b'h',lambda:r.client.screen.cursor.x==x and r.client.screen.cursor.y==y)
        before=(r.client.screen.cursor.x,r.client.screen.cursor.y)
        r.action('copy_big_word_forward',b'W',lambda:r.client.screen.cursor.x==x+15 and r.client.screen.cursor.y==y)
        # Selection and receipt have explicit distinct endpoint labels.
        r.keys(b'0');r.keys(b'V')
        if r.capture.exists():r.capture.unlink()
        r.action('yank_line_clipboard_receipt',b'y',lambda:r.capture.exists())
        copied=r.capture.read_text();assert copied.rstrip()=='SCRATCH-H00000 '+'x'*12,copied
        r.measurements[-1]['clipboard_text']=copied
        r.keys(b'\x1bw');r.keys(b'gg' if variant=='mux' else b'g')
        r.action('history_bottom',b'G',lambda:r.client.contains('SCRATCH>') and not r.client.contains('SCRATCH-H00000'))
        r.keys(b'\x1b')
        # Multi-client redraw: second attached client gets populated viewport.
        second=r.attach('second_client_attach',['SCRATCH>'],second=True)
        second.close()
        # Detach endpoint is process exit, distinct from rendered viewport.
        start=r.client.input(b'\x1bad')
        r.client.process.wait(timeout=10);end=time.perf_counter_ns()
        r.measurements.append({'name':'detach_client_exit','input_hex':b'\x1bad'.hex(),'input_ns':start,'decoded_ns':end,'latency_ms':(end-start)/1e6,'correct':True})
        r.client.close();r.client=None
        r.attach('reattach',['SCRATCH>'])
        result['correct']=True
    except Exception as error:
        result['error']=str(error);result['traceback']=traceback.format_exc()
    finally:
        result['measurements']=r.measurements
        try:r.cleanup()
        except Exception as error:result['cleanup_error']=str(error);result['correct']=False
        result['ended_ns']=time.perf_counter_ns()
        (r.directory/'sample.json').write_text(json.dumps(result,indent=2))
    return result


def main():
    p=argparse.ArgumentParser();p.add_argument('--output',type=Path,required=True)
    p.add_argument('--trials',type=int,default=20);p.add_argument('--smoke',action='store_true')
    a=p.parse_args()
    assert os.environ.get('BENCH_NIXPKGS_REV') and command(['git','-C',str(SOURCE_ROOT),'rev-parse','HEAD'])==SOURCE_COMMIT
    assert a.smoke or a.trials>=20
    a.output=a.output.resolve();a.output.mkdir(parents=True,exist_ok=False)
    env={'source_commit':SOURCE_COMMIT,'harness_commit':command(['git','rev-parse','HEAD']),
         'nixpkgs':os.environ['BENCH_NIXPKGS_REV'],'args':vars(a)|{'output':str(a.output)},
         'inter_operation_drain_seconds':.02,'lone_escape_drain_seconds':.06,
         'execution_context':'hosted-ci-interactive-correctness' if a.smoke else 'hosted-ci-interactive-performance',
         'runner':{k:os.environ.get(k) for k in ('ImageVersion','GITHUB_RUN_ID','RUNNER_NAME')},
         'versions':{k:command([k,'-V' if k=='tmux' else '--version']) for k in ('rustc','cargo','python3','tmux','bash')},
         'cpuinfo':Path('/proc/cpuinfo').read_text(),'meminfo':Path('/proc/meminfo').read_text(),
         'mounts':Path('/proc/mounts').read_text(),'lock':json.loads((ROOT/'.nix/flake.lock').read_text())}
    (a.output/'environment.json').write_text(json.dumps(env,indent=2))
    with (a.output/'build.log').open('w') as log:subprocess.run(['cargo','build','--locked','--release'],cwd=SOURCE_ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
    samples=[];rng=random.Random(20260930)
    scales=SCALES
    for w,panes in scales:
        for load in ('idle','busy'):
            for n in range(1 if a.smoke else a.trials+1):
                variants=list(VARIANTS);rng.shuffle(variants)
                for v in variants:
                    s=exercise(v,n,w,panes,load,a.output,200 if a.smoke else 1000);samples.append(s)
                    print(w,panes,load,n,v,'PASS' if s['correct'] else s.get('error'),flush=True)
                    if a.smoke and not s['correct']:
                        raise SystemExit('Correctness preflight failed; inspect retained sample.json')
    summary={}
    for w,panes in scales:
        for load in ('idle','busy'):
            for v in VARIANTS:
                selected=[s for s in samples if s['windows']==w and s['load']==load and s['variant']==v]
                key=f'w{w}-p{panes}/{load}/{v}'
                if any(not s['correct'] for s in selected):summary[key]={'blocked':True};continue
                measured=[s for s in selected if s['trial']>0]
                if not measured:continue
                names=set(m['name'] for s in measured for m in s['measurements'])
                summary[key]={name:stats([m['latency_ms'] for s in measured for m in s['measurements'] if m['name']==name]) for name in sorted(names)}
    (a.output/'summary.json').write_text(json.dumps(summary,indent=2))
    if any(not s['correct'] for s in samples):raise SystemExit(1)

if __name__=='__main__':main()
