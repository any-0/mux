#!/usr/bin/env python3
"""Audit complete declared groups and shard retained raw evidence for transfer."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import traceback
from audit_interactive import audit


def main():
    p=argparse.ArgumentParser();p.add_argument('source',type=Path);p.add_argument('output',type=Path);a=p.parse_args()
    a.output.mkdir(parents=True,exist_ok=False);manifest={'source_run':36762906986,'groups':{}}
    performance=a.source/'interactive-performance'
    for windows in (1,3,6):
        for load in ('idle','busy'):
            key=f'w{windows}-{load}';directory=a.output/key;directory.mkdir()
            raw=directory/'interactive-performance';raw.mkdir()
            for path in performance.iterdir():
                if path.is_file():shutil.copy2(path,raw/path.name)
                elif path.name.startswith(f'w{windows}-') and f'-{load}-' in path.name:
                    shutil.copytree(path,raw/path.name,symlinks=True)
            try:
                result=audit(raw,windows,load)
                (directory/'audit.json').write_text(json.dumps(result,indent=2))
                manifest['groups'][key]={'audit':'passed','measured_trials':result['measured_trials']}
                print(key,'AUDIT PASSED',result['measured_trials'],flush=True)
            except Exception as error:
                (directory/'audit-failure.txt').write_text(traceback.format_exc())
                manifest['groups'][key]={'audit':'blocked','reason':str(error)}
                print(key,'AUDIT BLOCKED',str(error),flush=True)
            # Per-file digests preserve evidence after GitHub artifact expiration.
            digests={str(path.relative_to(directory)):hashlib.sha256(path.read_bytes()).hexdigest()
                     for path in directory.rglob('*') if path.is_file() and not path.is_symlink()}
            (directory/'sha256.json').write_text(json.dumps(digests,sort_keys=True,indent=2))
    if (a.source/'interactive-smoke').exists():
        shutil.copytree(a.source/'interactive-smoke',a.output/'diagnostic-preflight',symlinks=True)
    (a.output/'manifest.json').write_text(json.dumps(manifest,indent=2))


if __name__=='__main__':main()
