#!/usr/bin/env python3
"""Mux-specific visible UI operations; no tmux performance comparison."""
import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time
import traceback

from interactive import Interactive
from run import ROOT, SOURCE_ROOT, SOURCE_COMMIT, command
from audit_hosted import stats


def tab_background(terminal, name):
    for y, line in enumerate(terminal.screen.display):
        if name in line:
            return terminal.screen.buffer[y][line.index(name)].bg
    return None


def exercise(root, number):
    r=Interactive('mux',root/f'trial-{number:03d}')
    result={'trial':number,'correct':False,'started_ns':time.perf_counter_ns()}
    try:
        themes=r.directory/'themes';themes.mkdir()
        for name,background,selection in [('Alpha','101010','222222'),('Beta','202040','334455')]:
            directory=themes/name;directory.mkdir()
            (directory/'mux.toml').write_text('[palette]\nbackground = "'+background+'"\nsurface = "'+background+'"\nselection = "'+selection+'"\n')
        switch=r.directory/'switch-theme'
        switch.write_text('#!'+os.environ['BENCH_SHELL']+'\nset -eu\nexec '+shlex.quote(r.mux)+' set-theme '+shlex.quote(str(themes))+'/"$1"/mux.toml\n')
        switch.chmod(0o700)
        with r.config.open('a') as f:
            f.write('theme_directory = '+json.dumps(str(themes))+'\ntheme_command = '+json.dumps([str(switch)])+'\n')
        r.launch()
        first=r.panes()[0]
        r.columns+=100-first['cols'];r.client.size(40,r.columns)
        r.wait(lambda:r.panes()[0]['cols']==100)
        r.shell_marker('UI_ONE',1000)
        r.cli('new-window');r.shell_marker('UI_TWO',1000)
        r.window(1);r.client.until(lambda:r.client.contains('UI_ONE>'))
        r.action('tree_open',b'\x1bs',lambda:r.client.contains(' sessions') and r.client.contains('▸'))
        r.action('tree_expand',b'l',lambda:r.client.contains('▾'))
        r.action('tree_preview_second_window',b'jjj',lambda:r.client.contains('UI_TWO>') and not r.client.contains('UI_ONE>'))
        offset=r.columns-100
        r.action('tree_choose_preview',b'\r',lambda:not r.client.contains(' sessions') and any(line.startswith(' '*offset+'UI_TWO>') for line in r.client.screen.display),lambda:r.windows())
        assert r.selected_window()==2
        r.keys(b'\x1bs');r.keys(b'l')
        r.action('tree_collapse',b'h',lambda:r.client.contains('▸') and not r.client.contains('▾'))
        r.action('tree_cancel',b'\x1b',lambda:not r.client.contains(' sessions') and r.client.contains('UI_TWO>'))
        r.action('theme_picker_open',b'\x1bc',lambda:r.client.contains('2 themes') and tab_background(r.client,'Alpha')=='222222')
        r.action('theme_preview_next',b'\x1b[C',lambda:tab_background(r.client,'Beta')=='334455',style_probe=lambda:tab_background(r.client,'Beta'))
        r.action('theme_preview_previous',b'\x1b[D',lambda:tab_background(r.client,'Alpha')=='222222',style_probe=lambda:tab_background(r.client,'Alpha'))
        r.action('theme_picker_cancel',b'\x1b',lambda:not r.client.contains('2 themes') and r.client.contains('UI_TWO>'))
        r.keys(b'\x1bc');r.keys(b'\x1b[C')
        r.action('theme_apply_palette',b'\r',lambda:not r.client.contains('2 themes') and r.client.contains('UI_TWO>') and r.client.screen.buffer[0][0].bg=='202040')
        with tempfile.TemporaryDirectory(prefix='mux-ui-root-') as directory:
            r.shell('cd '+shlex.quote(directory)+'; printf "ROOT_READY\\n"','ROOT_READY')
            r.client.until(lambda:r.client.contains('UI_TWO>'))
            r.action('session_root_from_cwd',b'\x1bR',lambda:r.client.contains('session root: '+directory))
            sessions=json.loads(r.cli('list-sessions','--json'))
            assert any(s['current'] and s['root']==directory for s in sessions),sessions
            r.measurements[-1]['metadata']=sessions
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
    p=argparse.ArgumentParser();p.add_argument('--output',type=Path,required=True);p.add_argument('--smoke',action='store_true');a=p.parse_args()
    assert os.environ.get('BENCH_NIXPKGS_REV') and command(['git','-C',str(SOURCE_ROOT),'rev-parse','HEAD'])==SOURCE_COMMIT
    a.output=a.output.resolve();a.output.mkdir(parents=True,exist_ok=False)
    env={'source_commit':SOURCE_COMMIT,'harness_commit':command(['git','rev-parse','HEAD']),
         'nixpkgs':os.environ['BENCH_NIXPKGS_REV'],'execution_context':'hosted-ci-mux-ui-correctness' if a.smoke else 'hosted-ci-mux-ui-performance',
         'runner':{key:os.environ.get(key) for key in ('ImageVersion','GITHUB_RUN_ID','RUNNER_NAME')},
         'versions':{key:command([key,'--version']) for key in ('rustc','cargo','python3','bash')},
         'cpuinfo':Path('/proc/cpuinfo').read_text(),'meminfo':Path('/proc/meminfo').read_text(),
         'lock':json.loads((ROOT/'.nix/flake.lock').read_text()),'comparability':'mux-only; tmux choose-tree/theme/root policies differ'}
    (a.output/'environment.json').write_text(json.dumps(env,indent=2))
    with (a.output/'build.log').open('w') as f:subprocess.run(['cargo','build','--locked','--release'],cwd=SOURCE_ROOT,stdout=f,stderr=subprocess.STDOUT,check=True)
    samples=[]
    for n in range(1 if a.smoke else 21):
        s=exercise(a.output,n);samples.append(s);print(n,'PASS' if s['correct'] else s['error'],flush=True)
        if not s['correct']:raise SystemExit('Mux UI correctness gate failed')
    if not a.smoke:
        names=[m['name'] for m in samples[0]['measurements']]
        assert all([m['name'] for m in s['measurements']]==names for s in samples)
        summary={name:stats([m['latency_ms'] for s in samples[1:] for m in s['measurements'] if m['name']==name]) for name in names}
        (a.output/'summary.json').write_text(json.dumps(summary,indent=2))


if __name__=='__main__':main()
