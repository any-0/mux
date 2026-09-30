"""External BSU/ESU semantics: do not mistake logical buffer for presentation."""
import unittest
from oracle import Terminal


class SynchronizedBoundary(unittest.TestCase):
    def test_arbitrary_byte_cuts_and_repeated_begin(self):
        terminal=Terminal(4,20)
        terminal.feed(b'BASE')
        completed=terminal.snapshot()
        for byte in b'\x1b[?2026h\x1b[2J\x1b[2;3H\x1b[1;2;4:3mNEXT\x1b[?2026h':
            terminal.feed(bytes([byte]))
        with self.assertRaisesRegex(AssertionError,'in-progress or timed'):
            terminal.snapshot()
        self.assertNotEqual(terminal.snapshot(allow_uncommitted=True),completed)
        for byte in b'\x1b[?2026l':terminal.feed(bytes([byte]))
        self.assertEqual(terminal.snapshot()['cells'][1][2][0],'N')
        terminal.feed(b'\x1b[?2026h\x1bc')
        self.assertEqual(terminal.snapshot()['cells'][0][0][0],' ')
