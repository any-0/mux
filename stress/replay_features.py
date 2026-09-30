#!/usr/bin/env python3
"""Recheck action-owned feature frames and output bodies from retained bytes."""
import argparse
import hashlib
import json
from pathlib import Path
import re
from pane_checks import cell
from output_workloads import large_fixture
from replay import ClientReplay,PaneReplay,replay


def replay_features(root):
    clients={}
    results=[]
    for line in (root/'actions.jsonl').read_text().splitlines():
        action=json.loads(line)
        kind=action['kind']
        if kind in ('pane-rectangle','synchronized-presentation'):
            name=action['client']
            if name not in clients:clients[name]=ClientReplay(root,name)
            checkpoint=dict(action,bar=action.get('left',action.get('bar')))
            actual=clients[name].advance(checkpoint,action['client_offset'])
            if kind=='pane-rectangle':
                expected=[[cell(action['identity'],x) for x in range(action['width'])] for _ in range(action['rows'])]
                passed=actual['cells']==expected
                if action['active']:
                    passed=passed and actual['cursor']==[1,2] and not actual['hidden'] and actual['cursor_shape']=='underline'
            else:
                source=PaneReplay('underline')
                completed=source.advance(root/'capture'/action['capture_file'],action['completed_capture_offset'])
                passed=actual==completed and action['response_hex']==b'\x1b[?2026;1$y'.hex()
            results.append(dict(kind=kind,index=action['index'],passed=passed))
        elif kind=='output-coverage':
            with (root/'capture'/action['capture_file']).open() as stream:
                stream.seek(action['capture_start'])
                records=stream.read(action['capture_end']-action['capture_start']).splitlines()
            source=b''.join(bytes.fromhex(e['hex']) for e in map(json.loads,records) if e['event']=='output').replace(b'\r\n',b'\n')
            large=large_fixture()
            fixture={'large':large,'head':b''.join(large.splitlines(keepends=True)[:37]),'sample':'alpha\n界 e\u0301\n\nlast\n'.encode()}[action['workload_kind']]
            rows=len(re.findall(rb'ROW\d{5} ',source)) if action['workload_kind']!='sample' else fixture.count(b'\n') if source.count(fixture)==1 else 0
            passed=(source.count(fixture)==1 and rows==action['observed_source_logical_rows']==action['requested_logical_rows']
                    and hashlib.sha256(fixture).hexdigest()==action['expected_sha256']
                    and action['completion_hex']==(f"{action['workload_kind']}-{action['generation']}:0\n").encode().hex())
            results.append(dict(kind=kind,index=action['index'],passed=passed,observed_source_logical_rows=rows))
    if (root/'capture').exists() and any((root/'capture').glob('*.jsonl')):
        results.extend(replay(root))
    return results


if __name__=='__main__':
    parser=argparse.ArgumentParser()
    parser.add_argument('directory',type=Path)
    results=replay_features(parser.parse_args().directory)
    print(json.dumps(results,indent=2))
    raise SystemExit(0 if results and all(r['passed'] for r in results) else 1)
