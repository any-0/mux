"""External-spec presentation oracle for transient real-PTY ?2026 batches.

https://github.com/contour-terminal/vt-extensions/blob/master/synchronized-output.md
Last completed cells/cursor/styles remain presented until ESU. No timeout model.
"""
import os
import shlex
import time
from run import ROOT, write_json


def synchronized(session):
    work=session.root/'work'
    os.mkfifo(work/'sync.fifo')
    fifo=os.open(work/'sync.fifo',os.O_RDWR|os.O_NONBLOCK)
    session.input(f'python3 {shlex.quote(str(ROOT/"stress/sync_probe.py"))} {shlex.quote(str(work))}'.encode()+b'\r')
    deadline=time.monotonic()+5
    while not (work/'sync-base.ready').exists():
        session.pump()
        assert time.monotonic()<deadline, 'synchronized fixture startup timed out'
    session.checkpoint('sync-base')
    completed=session.expected.snapshot()
    completed_offset=session.capture_offset
    write_json(session.root/'sync-completed-presentation.json',completed)
    try:
        for command,stage in ((b'B','begin'),(b'R','repeat')):
            started=time.monotonic()
            os.write(fifo,command)
            proof=work/('sync-'+stage+'.response')
            while not proof.exists():
                session.pump(.005)
                assert time.monotonic()-started<.8, 'synchronized setup exceeded pre-expiry budget'
            assert proof.read_bytes()==b'\x1b[?2026;1$y', 'synchronized mode acknowledgement differs from DECRQM specification'
            session.pump(0)
            assert session.expected.synchronized_output_pending, 'source BSU was not recorded'
            client=session.attach('sync-'+stage)
            while True:
                session.pump(.005)
                terminal=client['terminal']
                if client['offset'] and not terminal.pending and not terminal.synchronized_output_pending:
                    actual=terminal.snapshot(session.bar,session.cols-session.bar)
                    if any(c[0] in ('B','N') for row in actual['cells'] for c in row):
                        break
                assert time.monotonic()-started<.8, 'synchronized fresh-frame barrier exceeded pre-expiry budget'
            write_json(session.root/f'sync-{stage}-actual.json',actual)
            assert actual==completed, 'synchronized prebatch presentation: cells/cursor/styles must retain completed state'
            session.action('synchronized-presentation',stage=stage,elapsed_seconds=time.monotonic()-started,
                           client=client['name'],client_offset=client['offset'],bar=session.bar,
                           rows=session.rows,cols=session.cols,capture_file=session.capture_path.name,
                           completed_capture_offset=completed_offset,response_hex=proof.read_bytes().hex())
        os.write(fifo,b'E')
        deadline=time.monotonic()+5
        while not (work/'sync-end.response').exists():
            session.pump()
            assert time.monotonic()<deadline, 'synchronized ESU response missing'
        assert (work/'sync-end.response').read_bytes()==b'\x1b[?2026;2$y', 'ESU must reset synchronized mode'
        session.checkpoint('sync-released')
        assert session.expected.snapshot()!=completed, 'fixture must change cells/cursor/styles'
        os.write(fifo,b'X')
        session.settle()
    finally:
        os.close(fifo)
