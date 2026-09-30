"""Correctness validation, never performance data.

Pure tests use stdlib only. Attached-PTY tests require pyte; run via Nix for the
reproducible validation environment. Host execution is explicitly labeled.
"""
import importlib.util
import io
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

from recovery import (BASE_ROWS, INTERVAL_SECONDS, AGES, Recovery, descendants, digest,
                      history_fidelity, metadata_gate, proc_identity, signal_owned,
                      snapshot_manifest, verify_resurrect_snapshot, recovery_trial, nonce_command, seed_command)
from recovery_workload import record
from run import Terminal
from testable import distribution, summarize_recovery


def fixture(label, count=BASE_ROWS, start=0, color=False):
    rows = [record(label, i) for i in range(start, start + count)]
    if color:
        rows = ['\x1b[31m' + row + '\x1b[0m' for row in rows]
    return '\n'.join(rows + [f'WRAP{label}|' + 'Z' * 120])


def snapshot(path, omit=None, corrupt=None):
    lines = []
    for window, pane in ((1, 1), (1, 2), (2, 1)):
        fields = ['pane', 'bench', str(window), '1', ':*', str(pane)]
        lines.append('\t'.join(fields))
    lines += ['window\tbench\t1', 'window\tbench\t2']
    (path / 'snapshot.txt').write_text('\n'.join(lines))
    (path / 'last').symlink_to('snapshot.txt')
    with tarfile.open(path / 'pane_contents.tar.gz', 'w:gz') as archive:
        for window, pane in ((1, 1), (1, 2), (2, 1)):
            label = f'w{window}p{pane}'
            if label == omit:
                continue
            text = fixture(label, color=True)
            if label == corrupt:
                text = text.replace(record(label, 5), 'CORRUPTED_ROW')
            content = text.encode()
            item = tarfile.TarInfo(f'./pane_contents/pane-bench:{window}.{pane}')
            item.size = len(content)
            archive.addfile(item, io.BytesIO(content))


class FidelityTests(unittest.TestCase):
    def test_full_utf8_digest(self):
        gate = history_fidelity(fixture('w1p1'), 'w1p1', BASE_ROWS)
        self.assertTrue(gate['full_history_correct'])
        self.assertTrue(gate['wrap_correct'])
        self.assertEqual(gate['lost_rows'], 0)
        self.assertEqual(gate['expected_sha256'], gate['observed_sha256'])

    def test_expected_crash_prefix_loss(self):
        gate = history_fidelity(fixture('w1p1', 180), 'w1p1', 200)
        self.assertTrue(gate['prefix_correct'])
        self.assertFalse(gate['full_history_correct'])
        self.assertEqual(gate['lost_rows'], 20)
        self.assertEqual(gate['lost_tagged_utf8_bytes'], sum(len(record('w1p1', i).encode()) for i in range(180, 200)))

    def test_history_gap_is_not_a_prefix(self):
        text = fixture('w1p1').replace(record('w1p1', 40), '')
        gate = history_fidelity(text, 'w1p1', 200)
        self.assertFalse(gate['prefix_correct'])
        self.assertIsNone(gate['lost_rows'])

    def test_duplicate_is_not_accepted(self):
        text = fixture('w1p1') + '\n' + record('w1p1', 0)
        self.assertFalse(history_fidelity(text, 'w1p1', 200)['prefix_correct'])

    def test_out_of_order_is_not_accepted(self):
        text = fixture('w1p1').replace(record('w1p1', 5), record('w1p1', 6))
        self.assertFalse(history_fidelity(text, 'w1p1', 200)['prefix_correct'])

    def test_unicode_corruption_is_not_accepted(self):
        text = fixture('w1p1').replace('α漢', '??', 1)
        self.assertFalse(history_fidelity(text, 'w1p1', 200)['prefix_correct'])

    def test_corrupt_final_row_is_not_reported_as_valid_prefix_loss(self):
        text = fixture('w1p1').replace(record('w1p1', 199), record('w1p1', 199).replace('α漢', '??'))
        gate = history_fidelity(text, 'w1p1', 200)
        self.assertFalse(gate['prefix_correct'])
        self.assertEqual(gate['malformed_tagged_rows'], 1)

    def test_mixed_pane_capture_is_rejected(self):
        text = fixture('w1p1') + record('w1p2', 0)
        self.assertFalse(history_fidelity(text, 'w1p1', 200)['prefix_correct'])

    def test_wrong_pane_is_not_accepted(self):
        self.assertFalse(history_fidelity(fixture('w1p2'), 'w1p1', 200)['full_history_correct'])

    def test_wrapped_probe_survives_newlines(self):
        text = fixture('w1p1').replace('Z' * 120, 'Z' * 83 + '\r\n' + 'Z' * 37)
        self.assertTrue(history_fidelity(text, 'w1p1', 200)['wrap_correct'])
        self.assertFalse(history_fidelity(text.replace('Z' * 37, 'Z' * 36), 'w1p1', 200)['wrap_correct'])

    def test_metadata_gate_includes_geometry_selection_and_cwd(self):
        before = {'selected_window': 1, 'panes': [{'rows': 19, 'cols': 100, 'active': True, 'cwd': '/fixture/a'}]}
        self.assertTrue(metadata_gate(before, json.loads(json.dumps(before))))
        for field, value in (('rows', 20), ('cols', 99), ('active', False), ('cwd', '/fixture/b')):
            after = json.loads(json.dumps(before))
            after['panes'][0][field] = value
            self.assertFalse(metadata_gate(before, after))

    def test_record_fixture_process_emits_exact_sequence(self):
        output = subprocess.check_output([sys.executable, str(Path(__file__).with_name('recovery_workload.py')),
                                          'w1p1', '--start', '20', '--count', '3', '--probe']).decode()
        self.assertIn('\x1b[31m' + record('w1p1', 20) + '\x1b[0m\r\n', output)
        self.assertIn(record('w1p1', 22), output)
        self.assertTrue(output.endswith('REC_DONE_w1p1_23\r\n'))

    def test_crash_buckets_use_real_default_interval_and_fit_history(self):
        self.assertEqual(INTERVAL_SECONDS, 900)
        self.assertEqual(AGES, (0.0, .25, .5, .75, .99))
        # One completion marker per burst + seeded rows + wrap/probe overhead.
        self.assertLess(BASE_ROWS + int(max(AGES) * INTERVAL_SECONDS) * 21 + 100, 20000)


class SnapshotTests(unittest.TestCase):
    def test_complete_layout_and_archive_accepted(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot(root)
            gates = verify_resurrect_snapshot(root)
            self.assertEqual(set(gates), {'w1p1', 'w1p2', 'w2p1'})
            self.assertTrue(all(g['full_history_correct'] for g in gates.values()))

    def test_missing_pane_content_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot(root, omit='w1p2')
            with self.assertRaisesRegex(RuntimeError, 'missing'):
                verify_resurrect_snapshot(root)

    def test_corrupt_history_rejected_even_with_complete_hook(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot(root, corrupt='w1p1')
            with self.assertRaisesRegex(RuntimeError, 'fidelity'):
                verify_resurrect_snapshot(root)

    def test_duplicate_layout_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot(root)
            with (root / 'snapshot.txt').open('a') as handle:
                handle.write('\npane\tbench\t1\t1\t:*\t1')
            with self.assertRaisesRegex(RuntimeError, 'duplicate'):
                verify_resurrect_snapshot(root)

    def test_truncated_gzip_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot(root)
            archive = root / 'pane_contents.tar.gz'
            archive.write_bytes(archive.read_bytes()[:40])
            with self.assertRaises((EOFError, tarfile.TarError)):
                verify_resurrect_snapshot(root)

    def test_manifest_detects_changed_bytes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'state').write_bytes(b'one')
            before = snapshot_manifest(root)
            (root / 'state').write_bytes(b'two')
            after = snapshot_manifest(root)
            self.assertEqual(before['state']['bytes'], after['state']['bytes'])
            self.assertNotEqual(before['state']['sha256'], after['state']['sha256'])

    def test_hook_without_archive_cannot_count_as_save(self):
        with tempfile.TemporaryDirectory() as tmp:
            run = object.__new__(Recovery)
            run.state = Path(tmp)
            run.stamp = run.state / 'save-completed.txt'
            root = run.state / 'resurrect'
            root.mkdir()
            snapshot(root)
            self.assertFalse(run.completed_snapshot())
            run.stamp.write_text('123.45')
            (root / 'pane_contents.tar.gz').unlink()
            with self.assertRaises(FileNotFoundError):
                run.completed_snapshot()


class AggregationTests(unittest.TestCase):
    def sample(self, trial=1, correct=True, variant='mux', **kwargs):
        return {'variant': variant, 'trial': trial, 'mode': 'clean', 'age_fraction': 0.0,
                'recovery_supported': variant != 'tmux', 'correct': correct, 'lost_rows': 0,
                'restart_attach_live_ms': 10, **kwargs}

    def test_nearest_rank_p95(self):
        stats = distribution(list(range(1, 21)))
        self.assertEqual(stats, {'n': 20, 'median': 10.5, 'p95': 19})

    def test_warmup_excluded(self):
        samples = [self.sample(trial=0, restart_attach_live_ms=1000), self.sample()]
        summary = summarize_recovery(samples)['clean/0.00/mux']
        self.assertEqual(summary['timings']['restart_attach_live_ms']['median'], 10)
        self.assertEqual(summary['trials'], 1)

    def test_single_failed_trial_suppresses_all_timing_aggregates(self):
        summary = summarize_recovery([self.sample(), self.sample(trial=2, correct=False, lost_rows=5)])['clean/0.00/mux']
        self.assertNotIn('timings', summary)
        self.assertEqual(summary['failed_trials'], 1)
        self.assertEqual(summary['observed_loss_rows']['median'], 2.5)

    def test_tmux_baseline_never_gets_successful_restore_time(self):
        summary = summarize_recovery([self.sample(variant='tmux')])['clean/0.00/tmux']
        self.assertNotIn('timings', summary)
        self.assertIn('unsupported', summary['restore'])

    def test_missing_loss_is_visible(self):
        summary = summarize_recovery([self.sample(correct=False, lost_rows=None, error='restore failed')])['clean/0.00/mux']
        self.assertIsNone(summary['observed_loss_rows'])
        self.assertEqual(summary['loss_samples_missing'], 1)


class ProcessSafetyTests(unittest.TestCase):
    def test_our_identity_and_tree_are_readable(self):
        self.assertEqual(proc_identity(os.getpid())[0], os.getpid())
        self.assertIn(os.getpid(), descendants(os.getpid()))
        self.assertIsNone(proc_identity(999999999))

    def test_recycled_pid_is_not_signaled(self):
        with patch('recovery.proc_identity', return_value=(123, 'new-start-time')), patch('recovery.os.kill') as kill:
            signal_owned({123: (123, 'old-start-time')}, 9)
            kill.assert_not_called()

    def test_only_matching_owned_pid_is_signaled(self):
        with patch('recovery.proc_identity', return_value=(123, 'same')), patch('recovery.os.kill') as kill:
            signal_owned({123: (123, 'same'), 456: None}, 9)
            kill.assert_called_once_with(123, 9)


class RecoveryControllerTests(unittest.TestCase):
    def test_restore_cli_ack_without_script_completion_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            run = object.__new__(Recovery)
            run.variant = 'tmux-persistence'
            run.directory = Path(tmp)
            run.launch = lambda restarting: 0
            run.cli = lambda *args: '/fixture/restore-script' if args[0] == 'show-option' else ''
            run.shell = unittest.mock.Mock()

            def require_completion(predicate):
                self.assertFalse(predicate())
                raise RuntimeError('script completion missing despite CLI ack')

            run.wait = require_completion
            with self.assertRaisesRegex(RuntimeError, 'completion missing'):
                run.restore()
            run.shell.assert_not_called()

    def backend(self, calls, change_metadata=False, lose_formatting=False, overwrite_snapshot=False):
        class FixtureBackend:
            def __init__(self, variant, directory):
                self.variant = variant
                self.events = []
                self.directory = directory
                directory.mkdir()
                self.state = directory / 'state'
                self.state.mkdir()
                self.stamp = directory / 'stamp'
                self.stamp.write_text('123.45')
                self.initial_start_monotonic = 0
                self.metadata_calls = self.capture_calls = 0

            def launch(self, restarting=False):
                calls.append(('launch', restarting))
                return 0

            def seed(self):
                calls.append(('seed',))

            def metadata(self):
                self.metadata_calls += 1
                return {'window': 2 if change_metadata and self.metadata_calls > 1 else 1}

            def capture_fidelity(self, totals):
                self.capture_calls += 1
                result = {}
                for label, count in totals.items():
                    gate = history_fidelity(fixture(label, count), label, count)
                    gate['styles'] = [{'fg': 'white' if lose_formatting and self.capture_calls > 1 else 'red'}]
                    result[label] = gate
                return result

            def select(self, *args):
                calls.append(('select', *args))

            def clean_save(self):
                calls.append(('clean_save',))
                return {'save_ms': None}

            def completed_snapshot(self):
                calls.append(('real_snapshot_completion_checked',))
                return True

            def wait(self, predicate, timeout=30):
                calls.append(('wait_for_scheduled_snapshot', timeout))
                assert predicate()

            def wait_duration(self, seconds):
                calls.append(('wait_duration', seconds))

            def shell(self, text, marker):
                calls.append(('attached_input', text, marker))
                if overwrite_snapshot:
                    self.stamp.write_text('456.78')

            def stop(self, crash=False):
                calls.append(('stop', crash))
                return 0  # Synthetic control fixture, never a benchmark sample.

            def restore(self):
                calls.append(('restore',))
                return {}

            def confirm_shells(self):
                return ['w1p1', 'w1p2', 'w2p1']

            def cli(self, *args):
                return 'bootstrap'

            def cleanup(self):
                calls.append(('cleanup',))
        return FixtureBackend

    def run_fixture(self, variant, mode, **options):
        calls = []
        with tempfile.TemporaryDirectory() as tmp, patch('recovery.Recovery', self.backend(calls, **options)):
            result = recovery_trial(variant, 1, mode, 0.0, Path(tmp))
        return result, calls

    def test_clean_controller_saves_then_stops_then_restores(self):
        result, calls = self.run_fixture('tmux-persistence', 'clean')
        names = [call[0] for call in calls]
        self.assertTrue(result['correct'])
        self.assertLess(names.index('clean_save'), names.index('stop'))
        self.assertLess(names.index('stop'), names.index('restore'))
        self.assertIn(('stop', False), calls)
        self.assertNotIn('wait_for_scheduled_snapshot', names)

    def test_crash_controller_waits_for_real_scheduler_and_never_manual_saves(self):
        result, calls = self.run_fixture('tmux-persistence', 'crash')
        names = [call[0] for call in calls]
        self.assertTrue(result['correct'])
        self.assertNotIn('clean_save', names)
        self.assertIn('wait_for_scheduled_snapshot', names)
        self.assertLess(names.index('wait_for_scheduled_snapshot'), names.index('attached_input'))
        self.assertIn(('stop', True), calls)
        self.assertEqual(result['after_fidelity']['w1p2']['recovered_rows'], BASE_ROWS + 20)

    def test_overwritten_scheduled_snapshot_aborts_age_bucket(self):
        result, calls = self.run_fixture('tmux-persistence', 'crash', overwrite_snapshot=True)
        self.assertFalse(result['correct'])
        self.assertIn('another periodic save', result['error'])
        self.assertNotIn('restore', [call[0] for call in calls])
        self.assertIn(('cleanup',), calls)

    def test_restored_metadata_mismatch_fails_gate(self):
        result, _ = self.run_fixture('mux', 'clean', change_metadata=True)
        self.assertFalse(result['correct'])
        self.assertFalse(result['metadata_correct'])

    def test_restored_color_loss_fails_gate_even_with_full_history(self):
        result, _ = self.run_fixture('mux', 'clean', lose_formatting=True)
        self.assertFalse(result['correct'])
        self.assertFalse(result['formatting_correct'])
        self.assertEqual(result['lost_rows'], 0)

    def test_baseline_reports_unavailable_and_all_rows_lost(self):
        result, _ = self.run_fixture('tmux', 'clean')
        self.assertTrue(result['correct'])
        self.assertFalse(result['recovery_supported'])
        self.assertTrue(result['expected_unavailable'])
        self.assertEqual(result['lost_rows'], BASE_ROWS * 3)


@unittest.skipUnless(importlib.util.find_spec('pyte'), 'pyte absent; run validation in Nix shell')
class AttachedPTYTests(unittest.TestCase):
    def test_scroll_commands_preserve_cursor_and_rows_outside_margins(self):
        program = r'''
import os, tty
tty.setraw(0)
os.write(1, b'AA\r\nBB\r\nCC\r\nDD\r\nEE\x1b[2;4r\x1b[3;2H\x1b[S\x1b]2;UP\x07')
os.read(0, 1)
os.write(1, b'\x1b[T\x1b]2;DOWN\x07')
os.read(0, 1)
'''
        with tempfile.TemporaryDirectory() as tmp:
            terminal = Terminal([sys.executable, '-c', program], dict(os.environ), Path(tmp), 8, 100)
            try:
                terminal.until(lambda: terminal.screen.title == 'UP', timeout=3)
                self.assertEqual([x[:2] for x in terminal.screen.display[:5]], ['AA', 'CC', 'DD', '  ', 'EE'])
                self.assertEqual((terminal.screen.cursor.x, terminal.screen.cursor.y), (1, 2))
                terminal.input(b'X')
                terminal.until(lambda: terminal.screen.title == 'DOWN', timeout=3)
                self.assertEqual([x[:2] for x in terminal.screen.display[:5]], ['AA', '  ', 'CC', 'DD', 'EE'])
                self.assertEqual((terminal.screen.cursor.x, terminal.screen.cursor.y), (1, 2))
            finally:
                terminal.close()

    def test_secondary_identity_query_cannot_inject_primary_reply_into_shell(self):
        program = r'''
import os, tty
tty.setraw(0)
os.write(1, b'\x1b[>cWAIT_IDENTITY')
reply = os.read(0, 20)
os.write(1, b'IDENTITY_OK' if reply == b'X' else b'IDENTITY_FAILED')
os.read(0, 1)
'''
        with tempfile.TemporaryDirectory() as tmp:
            terminal = Terminal([sys.executable, '-c', program], dict(os.environ), Path(tmp), 8, 100)
            try:
                terminal.until(lambda: terminal.contains('WAIT_IDENTITY'), timeout=3)
                terminal.input(b'X')
                terminal.until(lambda: terminal.contains('IDENTITY_OK'), timeout=3)
            finally:
                terminal.close()

    def test_overwritten_wide_leading_cell_renders_orphan_stub_as_blank(self):
        program = "import os; os.write(1, '漢\\x1b[1GA'.encode()); input()"
        with tempfile.TemporaryDirectory() as tmp:
            terminal = Terminal([sys.executable, '-c', program], dict(os.environ), Path(tmp), 8, 100)
            try:
                terminal.until(lambda: terminal.screen.buffer[0][0].data == 'A', timeout=3)
                self.assertEqual(terminal.screen.display[0][:3], 'A  ')
            finally:
                terminal.close()

    def test_private_cursor_query_receives_actual_pty_reply(self):
        program = r'''
import os, tty
tty.setraw(0)
os.write(1, b'\x1b[3;5H\x1b[?6n')
reply = b''
while not reply.endswith(b'R'):
    reply += os.read(0, 1)
os.write(1, b'\r\nQUERY_OK' if reply == b'\x1b[?3;5R' else b'QUERY_FAILED')
os.read(0, 1)
'''
        with tempfile.TemporaryDirectory() as tmp:
            terminal = Terminal([sys.executable, '-c', program], dict(os.environ), Path(tmp), 8, 100)
            try:
                terminal.until(lambda: terminal.contains('QUERY_OK'), timeout=3)
                self.assertFalse(terminal.contains('QUERY_FAILED'))
            finally:
                terminal.close()

    def test_seed_command_reaches_shell_and_renders_real_colored_utf8_fixture(self):
        with tempfile.TemporaryDirectory() as tmp:
            env = dict(os.environ, PS1='VALIDATION_PROMPT> ')
            terminal = Terminal([shutil.which('bash'), '--noprofile', '--norc', '-i'], env, Path(tmp), 19, 100)
            try:
                terminal.until(lambda: terminal.contains('VALIDATION_PROMPT>'), timeout=3)
                argv = [sys.executable, str(Path(__file__).with_name('recovery_workload.py')),
                        'w1p1', '--count', '3', '--probe']
                text = seed_command(tmp, argv)
                self.assertNotIn('\x1b', text)
                terminal.input((text + '\n').encode())
                terminal.until(lambda: terminal.contains('REC_DONE_w1p1_3'), timeout=3)
                self.assertIn(record('w1p1', 0), terminal.screen.display[0])
                self.assertEqual(terminal.screen.buffer[0][2].fg, 'red')
            finally:
                terminal.close()

    def test_live_shell_gate_cannot_be_satisfied_by_echoed_command(self):
        with tempfile.TemporaryDirectory() as tmp:
            env = dict(os.environ, PS1='VALIDATION_PROMPT> ')
            terminal = Terminal([shutil.which('bash'), '--noprofile', '--norc', '-i'], env, Path(tmp), 8, 100)
            try:
                terminal.until(lambda: terminal.contains('VALIDATION_PROMPT>'), timeout=3)
                nonce = 'REC_SHELL_UNIQUE_VALIDATION_NONCE'
                # Bash echoes the command, then waits for an explicit input
                # before printing the response. That echo must not pass.
                text = 'read -r reply; ' + nonce_command(nonce)
                terminal.input((text + '\n').encode())
                terminal.drain()
                self.assertFalse(terminal.contains(nonce))
                terminal.input(b'release\n')
                terminal.until(lambda: terminal.contains(nonce), timeout=3)
            finally:
                terminal.close()

    def test_render_gate_ignores_erased_raw_marker_and_decodes_utf8_color(self):
        program = r'''
import os, termios
settings = termios.tcgetattr(0)
settings[3] &= ~termios.ECHO
termios.tcsetattr(0, termios.TCSANOW, settings)
os.write(1, b'ERASED_TOKEN\x1b[2J\x1b[HWAIT')
os.read(0, 100)
os.write(1, '\x1b[31mREAL_α漢_TOKEN\x1b[0m'.encode())
os.read(0, 100)
'''
        with tempfile.TemporaryDirectory() as tmp:
            terminal = Terminal([sys.executable, '-c', program], dict(os.environ), Path(tmp), 8, 100)
            try:
                terminal.until(lambda: terminal.contains('WAIT'), timeout=3)
                self.assertFalse(terminal.contains('ERASED_TOKEN'))
                terminal.input(b'go\n')
                terminal.until(lambda: terminal.contains('REAL_α漢_TOKEN'), timeout=3)
                self.assertEqual(terminal.screen.buffer[0][4].fg, 'red')
                terminal.raw.flush()
                self.assertIn(b'ERASED_TOKEN', (Path(tmp) / 'client.ansi').read_bytes())
            finally:
                terminal.close()

    def test_dimensions_are_applied_to_actual_pty(self):
        program = 'import os; print("PTY_SIZE_" + str(os.get_terminal_size(0)), flush=True); input()'
        with tempfile.TemporaryDirectory() as tmp:
            terminal = Terminal([sys.executable, '-c', program], dict(os.environ), Path(tmp), 19, 100)
            try:
                terminal.until(lambda: terminal.contains('columns=100, lines=19'), timeout=3)
            finally:
                terminal.close()


if __name__ == '__main__':
    unittest.main()
