#!/usr/bin/env python3
"""Independent whole-scale audit; reads raw histories and decoded live screens."""
import hashlib
import json
from pathlib import Path
import re
import sys
from audit_hosted import stats

VARIANTS=('mux','tmux','tmux-persistence')


def audit(root):
    env=json.loads((root/'environment.json').read_text())
    assert not env['smoke'] and env['trials']>=20
    assert env['source_commit']=='d6dd228054231e77772bd17a412d8f0d07871835'
    samples=[];fixture=None
    for path in sorted(root.glob('w*/sample.json')):
        s=json.loads(path.read_text());samples.append(s)
        assert s['correct'] and s['old_processes_ended'] and s['source_commit']==env['source_commit']
        w=s['windows'];p=s['panes_per_window'];assert {1:1,3:2,6:4}[w]==p
        assert s['before_layout']==s['after_layout']
        normalized=[]
        for window in s['after_layout']['windows']:
            assert window['name']==f'W{window["window"]:02d}'
            assert [x['slot'] for x in window['panes']]==list(range(1,p+1))
            expected_dims=[(100,40)] if p==1 else [(100,19),(100,20)] if p==2 else [(49,19),(50,19),(49,20),(50,20)]
            assert [(x['cols'],x['rows']) for x in window['panes']]==expected_dims
            for pane in window['panes']:
                assert Path(pane['cwd']).name==f'cwd-W{window["window"]:02d}P{pane["slot"]:02d}'
            normalized.append([(x['slot'],x['cols'],x['rows'],x['active']) for x in window['panes']])
        assert len(normalized)==w and s['after_layout']['selected_window']==1
        if fixture is None:fixture=normalized
        assert normalized==fixture,'Unequal semantic layouts'
        labels={f'W{wi:02d}P{pi:02d}' for wi in range(1,w+1) for pi in range(1,p+1)}
        for phase in ('before','after'):
            evidence=s[phase+'_history'];assert set(evidence)==labels
            for label,entry in evidence.items():
                text=(path.parent/entry['file']).read_text()
                rows=re.findall(label+r'-H\d{5} x{12}',text)
                expected=[f'{label}-H{i:05d} '+'x'*12 for i in range(1000)]
                assert rows==expected and entry['rows']==1000
                assert hashlib.sha256('\n'.join(rows).encode()).hexdigest()==entry['sha256']
        assert len(s['live_shells'])==w*p
        assert {(x['window'],x['slot']) for x in s['live_shells']}=={(wi,pi) for wi in range(1,w+1) for pi in range(1,p+1)}
        assert all(x['nonce'] in '\n'.join(x['screen']) for x in s['live_shells'])
        screen='\n'.join(s['visible_screen'])
        assert all(f'W01P{pi:02d}-H00999 '+'x'*12 in screen for pi in range(1,p+1))
        if s['variant']!='tmux':assert screen.count('BENCH_READY>')>=p
        assert s['visible_ns']>s['start_ns'] and abs((s['visible_ns']-s['start_ns'])/1e6-s['latency_ms'])<1e-9
        assert s['persistence_supported']==(s['variant']!='tmux')
    summary={}
    for v in VARIANTS:
        selected=[s for s in samples if s['variant']==v]
        assert sorted(s['trial'] for s in selected)==list(range(env['trials']+1))
        summary[v]=stats([s['latency_ms'] for s in selected if s['trial']>0])
    assert summary==json.loads((root/'summary.json').read_text())
    return {'accepted_trials':3*env['trials'],'warmups_excluded':3,'windows':env['windows'],
            'summary':summary,'source_commit':env['source_commit'],'all_layout_history_live_gates':True}

if __name__=='__main__':print(json.dumps(audit(Path(sys.argv[1])),indent=2))
