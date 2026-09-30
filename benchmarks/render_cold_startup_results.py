#!/usr/bin/env python3
"""Verify retained raw startup artifact and reproduce accepted scale tables."""
import argparse
import hashlib
import json
import re
from pathlib import Path
import tempfile
import zipfile
from audit_cold_startup import audit
from audit_hosted import stats


def render(directory):
    manifest=json.loads((directory/'manifest.json').read_text())
    assert manifest['status']=='completed'
    archive=directory/manifest['raw_file']
    assert hashlib.sha256(archive.read_bytes()).hexdigest()==manifest['artifact_sha256']
    with tempfile.TemporaryDirectory(prefix='mux-cold-audit-') as temp:
        root=Path(temp)
        with zipfile.ZipFile(archive) as z:
            assert all(not Path(n).is_absolute() and '..' not in Path(n).parts for n in z.namelist())
            z.extractall(root)
        audits={};runner=None;harness=None;envs=[];memory={};chronology=[]
        for w in (1,3,6):
            group=root/f'cold-w{w}';result=audit(group)
            assert result==json.loads((group/'audit.json').read_text())
            assert result==json.loads((directory/f'w{w}-audit.json').read_text())
            env=json.loads((group/'environment.json').read_text());envs.append(env)
            assert env['nixpkgs']=='2fc6539b481e1d2569f25f8799236694180c0993'
            assert env['versions']['rustc'].startswith('rustc 1.93.0')
            assert env['versions']['tmux']=='tmux 3.6a'
            assert env['runner']['GITHUB_RUN_ID']==str(manifest['run_id'])
            if runner is None:runner=env['runner'];harness=env['harness_commit']
            assert env['runner']==runner and env['harness_commit']==harness
            # Independently check logical positions in the timed visible frame.
            count={1:1,3:2,6:4}[w]
            memory[w]={v:[] for v in ('mux','tmux','tmux-persistence')}
            for path in group.glob('w*/sample.json'):
                s=json.loads(path.read_text());screen=s['visible_screen']
                for phase in ('before','after'):
                    for label,evidence in s[phase+'_history'].items():
                        history=(path.parent/evidence['file']).read_text()
                        all_records=re.findall(r'W\d{2}P\d{2}-H\d{5} x{12}',history)
                        assert all_records==[f'{label}-H{i:05d} '+'x'*12 for i in range(1000)], 'Foreign pane history'
                commands=json.loads((path.parent/'commands.json').read_text())
                launches=[e for e in commands if 'launch' in e]
                assert len(launches)==2 and launches[1]['start_ns']==s['start_ns']
                assert sum(bool(e.get('all_old_processes_ended')) for e in commands)==2
                chronology.append((launches[0]['start_ns'],max(e.get('end_ns',e.get('start_ns',0)) for e in commands),w,s['trial'],s['variant']))
                resource=s['resource_at_endpoint'];assert resource['processes']
                assert resource['pss_kib']==sum(p['Pss'] for p in resource['processes'])
                assert resource['rss_kib']==sum(p['Rss'] for p in resource['processes'])
                if s['trial']>0:memory[w][s['variant']].append(resource['pss_kib']/1024)
                offset=5 if s['variant']=='mux' else 0
                for pi in range(1,count+1):
                    tag=f'W01P{pi:02d}-H00999 '+'x'*12
                    positions=[(y,line.index(tag)-offset) for y,line in enumerate(screen) if tag in line]
                    assert len(positions)==1,(path,tag,positions)
                    y,x=positions[0]
                    assert x==(50 if count==4 and pi%2==0 else 0),(path,tag,positions)
                    assert (0<=y<=39 if count==1 else 0<=y<=18 if (count==2 and pi==1) or (count==4 and pi<=2) else 20<=y<=39),(path,tag,positions)
            audits[w]=result
        chronology.sort()
        assert [e[2] for e in chronology]==[1]*63+[3]*63+[6]*63
        assert all(a[1]<b[0] for a,b in zip(chronology,chronology[1:])), 'Overlapping process trials'
        for w in (1,3,6):
            assert [e[3] for e in chronology if e[2]==w]==[n for n in range(21) for _ in range(3)]
        assert sum(a['accepted_trials'] for a in audits.values())==180
        assert harness==manifest['harness_commit']
        model=next(x.split(':',1)[1].strip() for x in envs[0]['cpuinfo'].splitlines() if x.startswith('model name'))
        lines=['### Cold saved-layout scale startup (hosted CI, 2026-09-30)','',
               f'Actual [run {manifest["run_id"]}](https://github.com/any-0/mux/actions/runs/{manifest["run_id"]}): **180 accepted process trials**, 20 per variant/scale; nine warm-ups excluded. All complete-layout, history and fresh live-shell gates passed. Runtime remains `d6dd228054231e77772bd17a412d8f0d07871835`.', '',
               '| Windows × panes | mux clean restored startup, ms | tmux + persistence clean restored startup, ms | plain tmux fresh provisioning, ms |',
               '| --- | ---: | ---: | ---: |']
        for w in (1,3,6):
            summary=audits[w]['summary']
            def cell(v):
                s=summary[v];return f'{s["median"]:.3f} / {s["p95"]:.3f}'
            lines.append(f'| {w} × {dict([(1,1),(3,2),(6,4)])[w]} | {cell("mux")} | {cell("tmux-persistence")} | {cell("tmux")} |')
        lines+=['','| Windows × panes | mux endpoint PSS, MiB | stack endpoint PSS, MiB | plain fresh provisioning endpoint PSS, MiB |','| --- | ---: | ---: | ---: |']
        for w in (1,3,6):
            cells=[]
            for v in ('mux','tmux-persistence','tmux'):
                values=stats(memory[w][v]);cells.append(f'{values["median"]:.3f} / {values["p95"]:.3f}')
            lines.append(f'| {w} × {dict([(1,1),(3,2),(6,4)])[w]} | '+ ' | '.join(cells)+' |')
        lines+=['','Endpoint PSS includes daemon/client/shell descendants and live helpers observed just after the timed frame. It is a snapshot, not peak RAM; transient exited helpers are absent. RSS/process records are retained.', '', 'Values are median / nearest-rank p95. **Plain tmux persistence is unsupported**; fresh provisioning includes generating the matched history and controller preparation waits, and has no restore-speed ratio.', '',
                f'Paired groups ran sequentially on one {model} hosted VM (`{runner["RUNNER_NAME"]}`, image `{runner["ImageVersion"]}`). Executed harness `{harness}`, branch candidate `{manifest["branch_head"]}`. Project Nix pin/Rust/tmux/plugins match the earlier benchmark environment; filesystem caches remain warm.', '',
                'The endpoint is cold client/daemon launch to the first selected window’s complete retained viewport and fresh prompts. Every hidden window’s geometry/cwd/history and fresh shell response are independently verified after timing; this is not a timed tour of all windows. Clean saves are separate from crash recovery. No default-period crash claim is made. Additional motion and differing-size client-contention cases remain unmeasured.', '']
        return '\n'.join(lines)

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('directory',type=Path);a=p.parse_args();print(render(a.directory))
