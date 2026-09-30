#!/usr/bin/env python3
"""Reject incomplete, unpaired or incorrect operation datasets before publication."""
import argparse
import hashlib
import json
from pathlib import Path
import random
import re
from audit_hosted import stats

VARIANTS=('mux','tmux','tmux-persistence')
SCALES=((1,1),(3,2),(6,4))


def audit(root):
    env=json.loads((root/'environment.json').read_text())
    assert env['execution_context']=='hosted-ci-interactive-performance'
    assert env['source_commit']=='d6dd228054231e77772bd17a412d8f0d07871835'
    assert env['nixpkgs']== '2fc6539b481e1d2569f25f8799236694180c0993'
    samples=[json.loads(p.read_text()) for p in root.glob('*/sample.json')]
    keyed={(s['windows'],s['panes_per_window'],s['load'],s['trial'],s['variant']):s for s in samples}
    trials=max(s['trial'] for s in samples)
    assert trials>=20 and len(keyed)==len(samples)==len(SCALES)*2*3*(trials+1)
    summary=json.loads((root/'summary.json').read_text())
    rng=random.Random(20260930);last_end=0;comparisons={}
    for windows,panes in SCALES:
        for load in ('idle','busy'):
            geometry=None;names=None
            for n in range(trials+1):
                variants=list(VARIANTS);rng.shuffle(variants)
                for variant in variants:
                    s=keyed[windows,panes,load,n,variant]
                    assert s['correct'],(windows,panes,load,n,variant,s.get('error'))
                    assert s['started_ns']>last_end
                    last_end=s['ended_ns']
                    assert s['history_rows_per_pane']==1000
                    normalized=[[(p['index'],p['cols'],p['rows'],p['history_sha256']) for p in w['panes']] for w in s['fixture']]
                    if geometry is None:geometry=normalized
                    assert normalized==geometry,('unequal initial geometry/history',windows,panes,load,n,variant)
                    directory=root/f'w{windows}-p{panes}-{load}-{n:03d}-{variant}'
                    for w in s['fixture']:
                        for p in w['panes']:
                            label=f'W{w["window"]:02d}P{p["index"]:02d}'
                            found=re.findall(label+r'-H\d{5} x{12}',(directory/(label+'-history.txt')).read_text())
                            expected=[f'{label}-H{i:05d} '+ 'x'*12 for i in range(1000)]
                            assert found==expected
                            assert hashlib.sha256('\n'.join(found).encode()).hexdigest()==p['history_sha256']
                    observed=[m['name'] for m in s['measurements']]
                    if names is None:names=observed
                    assert observed==names
                    inputs={}
                    for path in directory.glob('*/input.json'):
                        inputs.update({e['input_ns']:e['hex'] for e in json.loads(path.read_text())})
                    for m in s['measurements']:
                        assert m['correct'] and m['latency_ms']==(m['decoded_ns']-m['input_ns'])/1e6>0
                        assert s['started_ns']<=m['input_ns']<m['decoded_ns']<=s['ended_ns']
                        if 'input_hex' in m: assert inputs[m['input_ns']]==m['input_hex']
                        if 'post_resource' in m:
                            r=m['post_resource']
                            assert r['pss_kib']==sum(p['Pss'] for p in r['processes'])
                            assert r['rss_kib']==sum(p['Rss'] for p in r['processes'])
                    resource=s['resource_before']
                    assert resource['pss_kib']==sum(p['Pss'] for p in resource['processes'])
            for variant in VARIANTS:
                measured=[keyed[windows,panes,load,n,variant] for n in range(1,trials+1)]
                key=f'w{windows}-p{panes}/{load}/{variant}'
                assert summary[key]=={name:stats([m['latency_ms'] for s in measured for m in s['measurements'] if m['name']==name]) for name in names}
                comparisons[key]={'operations':summary[key], 'pss_kib':stats([s['resource_before']['pss_kib'] for s in measured]),
                                  'rss_kib':stats([s['resource_before']['rss_kib'] for s in measured])}
    return {'audit':'passed','measured_trials':trials*len(SCALES)*2*3,
            'warmups_excluded':len(SCALES)*2*3,'operations_per_trial':len(names),'comparisons':comparisons}

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('dataset',type=Path);a=p.parse_args()
    print(json.dumps(audit(a.dataset),indent=2))
