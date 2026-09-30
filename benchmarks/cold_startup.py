#!/usr/bin/env python3
"""Cold daemon restart to populated visible layout; clean saves, not crash tests."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import re
import shlex
import subprocess
import time
import traceback

from interactive import Interactive, SCALES, VARIANTS
from run import ROOT, SOURCE_ROOT, SOURCE_COMMIT, command, resource_sample
from audit_hosted import stats
from recovery import nonce_command

ROWS = 1000


def records(label):
    return [f'{label}-H{i:05d} '+ 'x'*12 for i in range(ROWS)]


class Cold(Interactive):
    def select_pane(self, pane, window):
        if self.variant == 'mux':
            self.cli('rename-window', f'W{window:02d}', '--pane', str(pane['id']))
        else:
            self.cli('select-pane', '-t', pane['id'])
        self.client.drain(.02)

    def slots(self, count):
        panes = self.panes()
        assert len(panes) == count, panes
        for pane in panes:
            pane['slot'] = {1:1,4:2,2:3,3:4}[pane['index']] if self.variant=='mux' and count==4 else pane['index']
        return sorted(panes, key=lambda p:p['slot'])

    def seed_scale(self, windows, count):
        assert self.panes()[0]['cols']==100 and self.panes()[0]['rows']==40
        for w in range(1, windows+1):
            if w>1:self.cli('new-window')
            self.cli('rename-window', f'W{w:02d}')
            if count>=2:
                self.cli('split-window','-v') if self.variant=='mux' else self.cli('split-window','-v','-l','50%')
            if count==4:
                self.cli('split-window','-h') if self.variant=='mux' else self.cli('split-window','-h','-l','50%')
                self.cli('select-pane','-U')
                self.cli('split-window','-h') if self.variant=='mux' else self.cli('split-window','-h','-l','50%')
            for p in self.slots(count):
                self.select_pane(p,w)
                label=f'W{w:02d}P{p["slot"]:02d}'
                cwd=self.directory/('cwd-'+label);cwd.mkdir(exist_ok=True)
                self.shell('cd '+shlex.quote(str(cwd))+'; printf CWD_SET', 'CWD_SET')
                # mux's cwd sampler is output-triggered and rate-limited.
                self.client.drain(.3)
                self.shell_marker(label,ROWS)
                self.wait(lambda:next(x for x in self.panes() if x['id']==p['id'])['cwd']==str(cwd)) if self.variant=='mux' else None
            self.select_pane(self.slots(count)[0],w)
        self.window(1)

    def layout(self, windows, count):
        selected=self.selected_window()
        result=[]
        for w in range(1,windows+1):
            self.window(w)
            panes=self.slots(count)
            result.append({'window':w,'name':self.windows()[w-1]['name'],
                           'panes':[{'slot':p['slot'],'cols':p['cols'],'rows':p['rows'],
                                     'active':p['active'],'cwd':p['cwd']} if self.variant=='mux' else
                                    {'slot':p['slot'],'cols':p['cols'],'rows':p['rows'],
                                     'active':p['active'],'cwd':self.cli('display-message','-p','-t',p['id'],'#{pane_current_path}')} for p in panes]})
        self.window(selected)
        return {'selected_window':selected,'windows':result}

    def histories(self, windows, count, phase):
        evidence={}
        for w in range(1,windows+1):
            self.window(w)
            for p in self.slots(count):
                self.select_pane(p,w);label=f'W{w:02d}P{p["slot"]:02d}'
                if self.variant=='mux':
                    self.capture.unlink(missing_ok=True)
                    self.keys(b'\x1bw');self.client.input(b'ggVGy')
                    self.client.until(self.capture.exists)
                    text=self.capture.read_text();self.keys(b'\x1b')
                else:text=self.cli('capture-pane','-p','-J','-S','-','-t',p['id'])
                actual=re.findall(label+r'-H\d{5} x{12}',text)
                assert actual==records(label),(phase,label,len(actual))
                filename=f'{phase}-{label}-history.txt';(self.directory/filename).write_text(text)
                evidence[label]={'file':filename,'rows':len(actual),
                                 'sha256':hashlib.sha256('\n'.join(actual).encode()).hexdigest()}
            self.select_pane(self.slots(count)[0],w)
        self.window(1)
        return evidence

    def visible(self, count, fresh=False):
        text='\n'.join(self.client.screen.display)
        return all(records(f'W01P{i:02d}')[-1] in text for i in range(1,count+1)) and (
            all(f'W01P{i:02d}>' in text for i in range(1,count+1)) if fresh else text.count('BENCH_READY>')>=count)

    def save(self):
        if self.variant!='tmux-persistence':return None
        self.stamp.unlink(missing_ok=True)
        script=self.cli('show-option','-gqv','@resurrect-save-script-path')
        start=time.perf_counter_ns();self.cli('run-shell',shlex.quote(script))
        self.wait(lambda:self.stamp.exists() and (self.state/'resurrect/last').is_file())
        # The independent post-restart histories, not this hook, are the save correctness gate.
        return (time.perf_counter_ns()-start)/1e6


def trial(variant,n,windows,count,output):
    r=Cold(variant,output/f'w{windows}-{n:03d}-{variant}')
    s={'variant':variant,'trial':n,'windows':windows,'panes_per_window':count,'correct':False,
       'operation':'fresh populated creation including identical history generation' if variant=='tmux' else 'clean saved-layout cold daemon restart',
       'persistence_supported':variant!='tmux','history_rows_per_pane':ROWS,'source_commit':SOURCE_COMMIT}
    try:
        r.launch();r.seed_scale(windows,count)
        s['before_layout']=r.layout(windows,count)
        s['before_history']=r.histories(windows,count,'before')
        # Ensure layout debounce completed for every variant before stopping.
        r.client.drain(2)
        s['save_ms']=r.save();s['stop_ms']=r.stop()
        s['old_processes_ended']=any(e.get('all_old_processes_ended') for e in r.events)
        start=r.launch(restarting=True)
        if variant=='tmux-persistence':
            script=r.cli('show-option','-gqv','@resurrect-restore-script-path')
            r.cli('run-shell',shlex.quote(script))
            r.cli('kill-session','-t','bootstrap')
        elif variant=='tmux':
            r.cli('rename-session','bench');r.seed_scale(windows,count)
        end=r.client.until(lambda:r.visible(count,fresh=variant=='tmux'),timeout=60)
        s.update(start_ns=start,visible_ns=end,latency_ms=(end-start)/1e6,
                 visible_screen=list(r.client.screen.display),resource_at_endpoint=resource_sample([r.server,r.client.process.pid]))
        s['after_layout']=r.layout(windows,count)
        assert s['after_layout']==s['before_layout'],(s['before_layout'],s['after_layout'])
        assert len(r.windows())==windows
        s['after_history']=r.histories(windows,count,'after')
        assert {k:v['sha256'] for k,v in s['before_history'].items()}=={k:v['sha256'] for k,v in s['after_history'].items()}
        # All panes must be new live shells, not merely journal-replayed screens.
        live=[]
        for w in range(1,windows+1):
            r.window(w)
            for p in r.slots(count):
                r.select_pane(p,w);nonce='COLD_LIVE_'+str(time.perf_counter_ns())
                r.shell(nonce_command(nonce),nonce)
                live.append({'window':w,'slot':p['slot'],'nonce':nonce,'screen':list(r.client.screen.display)})
        s['live_shells']=live;s['correct']=True
    except Exception as e:s.update(error=str(e),traceback=traceback.format_exc())
    finally:
        try:r.cleanup()
        except Exception as e:s.update(cleanup_error=str(e),correct=False)
        (r.directory/'sample.json').write_text(json.dumps(s,indent=2))
    return s


def main():
    p=argparse.ArgumentParser();p.add_argument('--output',type=Path,required=True)
    p.add_argument('--windows',type=int,choices=(1,3,6),required=True)
    p.add_argument('--trials',type=int,default=20);p.add_argument('--smoke',action='store_true');a=p.parse_args()
    assert a.smoke or a.trials>=20
    assert command(['git','-C',str(SOURCE_ROOT),'rev-parse','HEAD'])==SOURCE_COMMIT
    assert os.environ['BENCH_NIXPKGS_REV']
    a.output=a.output.resolve();a.output.mkdir(parents=True)
    env={'source_commit':SOURCE_COMMIT,'harness_commit':command(['git','rev-parse','HEAD']),
         'nixpkgs':os.environ['BENCH_NIXPKGS_REV'],'smoke':a.smoke,'trials':a.trials,'windows':a.windows,
         'cache_policy':'cold processes, warm filesystem cache; no drop_caches',
         'runner':{k:os.environ.get(k) for k in ('ImageVersion','GITHUB_RUN_ID','RUNNER_NAME')},
         'versions':{k:command([k,'-V' if k=='tmux' else '--version']) for k in ('rustc','cargo','python3','tmux','bash')},
         'cpuinfo':Path('/proc/cpuinfo').read_text(),'meminfo':Path('/proc/meminfo').read_text(),
         'lock':json.loads((ROOT/'.nix/flake.lock').read_text())}
    (a.output/'environment.json').write_text(json.dumps(env,indent=2))
    with (a.output/'build.log').open('w') as log:subprocess.run(['cargo','build','--locked','--release'],cwd=SOURCE_ROOT,stdout=log,stderr=subprocess.STDOUT,check=True)
    samples=[];rng=random.Random(20260930);count=dict(SCALES)[a.windows]
    for n in range(1 if a.smoke else a.trials+1):
        variants=list(VARIANTS);rng.shuffle(variants)
        for v in variants:
            s=trial(v,n,a.windows,count,a.output);samples.append(s)
            print(a.windows,n,v,'PASS' if s['correct'] else s.get('error'),flush=True)
            if not s['correct']:raise SystemExit('Whole scale rejected: inspect sample.json')
    summary={v:stats([s['latency_ms'] for s in samples if s['variant']==v and s['trial']>0]) for v in VARIANTS if not a.smoke}
    (a.output/'summary.json').write_text(json.dumps(summary,indent=2))

if __name__=='__main__':main()
