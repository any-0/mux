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


def audit(root, requested_windows=None, requested_load=None):
    env=json.loads((root/'environment.json').read_text())
    assert env['execution_context']=='hosted-ci-interactive-performance'
    assert env['source_commit']=='d6dd228054231e77772bd17a412d8f0d07871835'
    assert env['nixpkgs']== '2fc6539b481e1d2569f25f8799236694180c0993'
    selected=requested_windows if requested_windows is not None else env['args'].get('windows')
    selected_load=requested_load if requested_load is not None else env['args'].get('load')
    assert env['args'].get('windows') in (None,selected) and env['args'].get('load') in (None,selected_load)
    assert selected is None or selected in (1,3,6)
    assert selected_load is None or selected_load in ('idle','busy')
    scales=tuple(scale for scale in SCALES if selected is None or scale[0]==selected)
    loads=(selected_load,) if selected_load else ('idle','busy')
    all_samples=[json.loads(p.read_text()) for p in root.glob('*/sample.json')]
    samples=[s for s in all_samples if (selected is None or s['windows']==selected) and (selected_load is None or s['load']==selected_load)]
    keyed={(s['windows'],s['panes_per_window'],s['load'],s['trial'],s['variant']):s for s in samples}
    trials=max(s['trial'] for s in samples)
    assert trials>=20 and len(keyed)==len(samples)==len(scales)*len(loads)*3*(trials+1)
    summary=json.loads((root/'summary.json').read_text())
    rng=random.Random(20260930);last_end=0;comparisons={};orders={}
    # Preserve original declared dispatch order even when auditing one group
    # from a complete multi-group artifact. Never infer order from survivors.
    for w,p in SCALES:
        if env['args'].get('windows') not in (None,w):continue
        for profile in ('idle','busy'):
            if env['args'].get('load') not in (None,profile):continue
            for n in range(trials+1):
                variants=list(VARIANTS);rng.shuffle(variants);orders[w,p,profile,n]=variants
    for windows,panes in scales:
        for load in loads:
            geometry=None;names=None
            for n in range(trials+1):
                for variant in orders[windows,panes,load,n]:
                    s=keyed[windows,panes,load,n,variant]
                    assert s['correct'],(windows,panes,load,n,variant,s.get('error'))
                    assert s['started_ns']>last_end
                    last_end=s['ended_ns']
                    assert s['history_rows_per_pane']==1000
                    normalized=[sorted((p.get('benchmark_index',p['index']),p['cols'],p['rows'],p['history_sha256']) for p in w['panes']) for w in s['fixture']]
                    if geometry is None:geometry=normalized
                    assert normalized==geometry,('unequal initial geometry/history',windows,panes,load,n,variant)
                    directory=root/f'w{windows}-p{panes}-{load}-{n:03d}-{variant}'
                    for w in s['fixture']:
                        for p in w['panes']:
                            label=f'W{w["window"]:02d}P{p.get("benchmark_index",p["index"]):02d}'
                            if 'benchmark_index' in p:
                                slot=p['benchmark_index']
                                expected_position=((18 if slot<=2 else 39),(0 if slot%2 else 50)) if panes==4 else ((18 if slot==1 else 39),0) if panes==2 else (39,0)
                                assert tuple(p['visible_prompt_position'])==expected_position
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
                if all('state_storage_after_profile' in s for s in measured):
                    comparisons[key]['observed_state_logical_bytes']=stats([s['state_storage_after_profile']['storage_bytes'] for s in measured])
                    comparisons[key]['observed_state_allocated_bytes']=stats([s['state_storage_after_profile']['storage_allocated_bytes'] for s in measured])
                if all('profile_cpu' in s for s in measured):
                    for s in measured:
                        cpu=s['profile_cpu']
                        assert cpu['window_seconds']>=3 and cpu['clock_ticks_per_second']>0
                        assert cpu['percent_one_cpu_lower_bound']==100*(cpu['ticks_after']-cpu['ticks_before'])/cpu['clock_ticks_per_second']/cpu['window_seconds']
                    comparisons[key]['profile_cpu_percent_one_cpu_lower_bound']=stats([s['profile_cpu']['percent_one_cpu_lower_bound'] for s in measured])
    return {'audit':'passed','scope':{'scales':scales,'loads':loads},'measured_trials':trials*len(scales)*len(loads)*3,
            'warmups_excluded':len(scales)*len(loads)*3,'operations_per_trial':len(names),'comparisons':comparisons}

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('dataset',type=Path);p.add_argument('--windows',type=int,choices=(1,3,6));p.add_argument('--load',choices=('idle','busy'));a=p.parse_args()
    print(json.dumps(audit(a.dataset,a.windows,a.load),indent=2))
