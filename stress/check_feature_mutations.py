#!/usr/bin/env python3
"""Native pinned-Nix sensitivity checks; setup/build errors never count as kills."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
from run import ROOT,write_json


def execute(command,log,env=None,cwd=ROOT):
    with log.open('wb') as stream:
        result=subprocess.run(command,cwd=cwd,env=env,stdout=stream,stderr=subprocess.STDOUT,timeout=180)
    return result.returncode,log.read_text(errors='replace')


def check(output):
    output.mkdir(parents=True,exist_ok=False)
    fixed=output/'mux-fixed'
    shutil.copy2(ROOT/'target/debug/mux',fixed)
    results=[]
    def case(binary,name,label,signature=None,env=None):
        log=output/(label+'.log')
        command=[sys.executable,str(ROOT/'stress/feature_witnesses.py'),'--binary',str(binary),'--case',name,'--output',str(output/label)]
        code,text=execute(command,log,env)
        result=dict(case=name,label=label,exit_code=code,expected_failure=signature is not None,signature=signature)
        results.append(result)
        write_json(output/'results.json',results)
        if signature is None:
            assert code==0,f'fixed {name} failed: {text[-4000:]}'
        else:
            assert code!=0 and signature in text,f'{label}: expected specific assertion {signature!r}, got exit {code}: {text[-4000:]}'
        print(json.dumps(result),flush=True)
    for name in ('focus','split','output','sync'):
        case(fixed,name,'fixed-'+name)
    wrapper=output/'suppress-cat'
    wrapper.mkdir()
    cat=shutil.which('cat')
    import shlex
    (wrapper/'cat').write_text('#!/bin/sh\nif [ "$#" = 1 ] && [ "$1" = large.txt ]; then exit 0; fi\nexec '+shlex.quote(cat)+' "$@"\n')
    (wrapper/'cat').chmod(0o755)
    case(fixed,'output','mutant-output','observed 0 rows/0 complete bodies',os.environ|{'PATH':str(wrapper)+os.pathsep+os.environ['PATH']})
    worktree=output/'mutant-source'
    subprocess.run(['git','worktree','add','--detach',str(worktree),'HEAD'],cwd=ROOT,check=True)
    target=ROOT/'target'
    mutations=[
        ('focus','src/server/mod.rs','window.zoomed = !window.zoomed;','window.zoomed = window.zoomed;','pane visible rectangle F'),
        ('split','src/server/input.rs',
         '        let writer =\n            &mut self.sessions[session_index].windows[window_index].panes[pane_index].writer;',
         '        let pane_index = if std::env::var("STRESS_ROUTE_MUTATION_FILE").ok().is_some_and(|p| std::path::Path::new(&p).exists()) {\n            (pane_index + 1) % self.sessions[session_index].windows[window_index].panes.len()\n        } else { pane_index };\n        let writer =\n            &mut self.sessions[session_index].windows[window_index].panes[pane_index].writer;',
         'pane input routing/file barrier'),
        ('sync','src/server/terminal.rs',
         '        Some(update) => (\n            &update.screen,\n            update.cursor_shape.unwrap_or(default_cursor_shape),\n        ),',
         '        Some(_update) => (\n            parser.screen(),\n            callbacks.cursor_shape.unwrap_or(default_cursor_shape),\n        ),',
         'synchronized prebatch presentation')]
    try:
        for name,filename,old,new,signature in mutations:
            path=worktree/filename
            original=path.read_text()
            assert original.count(old)==1,f'mutation anchor changed: {filename}'
            path.write_text(original.replace(old,new))
            diff=subprocess.check_output(['git','diff','--',filename],cwd=worktree)
            (output/('mutant-'+name+'.patch')).write_bytes(diff)
            code,text=execute(['cargo','build','--locked','--target-dir',str(target)],output/('build-'+name+'.log'),cwd=worktree)
            assert code==0,f'mutant {name} build failed: {text[-3000:]}'
            mutant=output/('mux-mutant-'+name)
            shutil.copy2(target/'debug/mux',mutant)
            env=os.environ|{'STRESS_ROUTE_MUTATION_FILE':str(output/'routing-armed')}
            case(mutant,name,'mutant-'+name,signature,env)
            path.write_text(original)
    finally:
        subprocess.run(['git','worktree','remove','--force',str(worktree)],cwd=ROOT,check=True)
        code,text=execute(['cargo','build','--locked'],output/'restore-fixed-build.log')
        assert code==0,f'fixed rebuild failed: {text[-3000:]}'
    write_json(output/'provenance.json',dict(source_commit=subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),nixpkgs=os.environ.get('STRESS_NIXPKGS_REV'),rust=os.environ.get('STRESS_RUST_VERSION')))


if __name__=='__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('--output',type=Path,required=True)
    check(parser.parse_args().output.resolve())
