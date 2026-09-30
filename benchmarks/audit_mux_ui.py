#!/usr/bin/env python3
"""Independent completeness/input/time audit for explicitly mux-only UI data."""
import argparse
import json
from pathlib import Path
from audit_hosted import stats


def audit(root):
    env=json.loads((root/'environment.json').read_text())
    assert env['execution_context']=='hosted-ci-mux-ui-performance'
    assert env['source_commit']=='d6dd228054231e77772bd17a412d8f0d07871835'
    assert env['nixpkgs']=='2fc6539b481e1d2569f25f8799236694180c0993'
    paths=sorted(root.glob('trial-*/sample.json'))
    assert len(paths)==21
    names=None;samples=[];last=0
    for n,path in enumerate(paths):
        s=json.loads(path.read_text());assert s['correct'] and s['trial']==n
        assert s['started_ns']>last;last=s['ended_ns']
        observed=[m['name'] for m in s['measurements']]
        if names is None:names=observed
        assert observed==names and len(names)==len(set(names))==12
        inputs={}
        for p in path.parent.glob('*/input.json'):
            inputs.update({e['input_ns']:e['hex'] for e in json.loads(p.read_text())})
        for m in s['measurements']:
            assert m['correct'] and m['latency_ms']==(m['decoded_ns']-m['input_ns'])/1e6>0
            assert s['started_ns']<=m['input_ns']<m['decoded_ns']<=s['ended_ns']
            assert inputs[m['input_ns']]==m['input_hex']
            assert m['before']!=m['after'] or m.get('style_before')!=m.get('style_after')
            resource=m['post_resource']
            assert resource['pss_kib']==sum(p['Pss'] for p in resource['processes'])
        samples.append(s)
    expected={name:stats([m['latency_ms'] for s in samples[1:] for m in s['measurements'] if m['name']==name]) for name in names}
    assert json.loads((root/'summary.json').read_text())==expected
    return {'audit':'passed','scope':'mux-only UI; no tmux comparison',
            'measured_trials':20,'warmups_excluded':1,'operations_per_trial':12,'operations':expected}


if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('dataset',type=Path);a=p.parse_args()
    print(json.dumps(audit(a.dataset),indent=2))
