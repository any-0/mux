"""Action-owned command completion and source-byte output coverage."""
import hashlib
import json
import re
import shlex
import time


def large_fixture():
    return ''.join(f'\x1b[1;2;3;4:{n%6};58:2::13:97:211;38;2;40;180;90mROW{n:05} 界 e\u0301 '
                   + 'x'*(n%121) + '\x1b[22;23;24;59;39m plain\n' for n in range(2400)).encode()


def output_command(session, command, expected, kind, generation):
    session.settle()
    start = session.capture_offset-len(session.capture_pending.encode())
    proof = session.root/'work/output-completed.txt'
    token = f'{kind}-{generation}'
    status = '$status' if session.shell == 'fish' else '$?'
    request = f"{command}; printf '%s:%s\\n' {shlex.quote(token)} \"{status}\" > {shlex.quote(str(proof))}"
    session.input(request.encode()+b'\r')
    deadline = time.monotonic()+15
    while not proof.exists() or not proof.read_bytes().startswith((token+':').encode()) or not proof.read_bytes().endswith(b'\n'):
        session.pump()
        assert time.monotonic() < deadline, f'output-coverage {token}: command completion missing'
    completion = proof.read_bytes()
    session.settle()
    with session.capture_path.open() as stream:
        stream.seek(start)
        records = stream.read(session.capture_offset-start).splitlines()
    source = b''.join(bytes.fromhex(event['hex']) for event in map(json.loads,records) if event['event']=='output')
    # ONLCR is a PTY transport translation, not an additional logical row.
    normalized = source.replace(b'\r\n',b'\n')
    count = normalized.count(expected)
    observed_rows = len(re.findall(rb'ROW\d{5} ',normalized)) if kind != 'sample' else (expected.count(b'\n') if count == 1 else 0)
    requested_rows = expected.count(b'\n')
    coverage = dict(workload_kind=kind,generation=generation,command=command,
                    requested_logical_rows=requested_rows,observed_source_logical_rows=observed_rows,
                    expected_bytes=len(expected),expected_sha256=hashlib.sha256(expected).hexdigest(),
                    expected_body_occurrences=count,completion_hex=completion.hex(),
                    capture_file=session.capture_path.name,capture_start=start,capture_end=session.capture_offset)
    session.action('output-coverage', **coverage)
    assert completion == (token+':0\n').encode(), f'output-coverage {token}: nonzero or incorrect completion {completion!r}'
    assert count == 1 and observed_rows == requested_rows, f'output-coverage {token}: expected one complete {requested_rows}-row body, observed {observed_rows} rows/{count} complete bodies'
    if not hasattr(session,'output_coverage'):
        session.output_coverage=[]
    session.output_coverage.append(coverage)
    return coverage
