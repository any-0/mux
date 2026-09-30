"""Protocol vectors make the oracle extensions accountable independently."""
import unittest
import json
from pathlib import Path
from oracle import Terminal


class OracleVectors(unittest.TestCase):
    def test_external_protocol_fixtures_with_every_chunk_boundary(self):
        fixtures = json.loads((Path(__file__).parent / 'fixtures/protocol.json').read_text())
        fields = ['text', 'fg', 'bg', 'bold', 'dim', 'italic', 'inverse', 'underline_style', 'underline_color']
        for fixture in fixtures['fixtures']:
            data = fixture['sequence'].encode()
            for split in range(len(data) + 1):
                with self.subTest(fixture=fixture['name'], split=split):
                    terminal = Terminal(3, 20)
                    terminal.feed(data[:split])
                    terminal.feed(data[split:])
                    snapshot = terminal.snapshot()
                    for cell in fixture['cells']:
                        actual = snapshot['cells'][cell['row']][cell['col']]
                        for field, expected in cell.items():
                            if field in fields:
                                self.assertEqual(actual[fields.index(field)], expected)
                    for key in ('cursor', 'cursor_shape', 'hidden'):
                        if key in fixture:
                            self.assertEqual(snapshot[key], fixture[key])

    def test_sgr_transitions_and_chunk_boundaries(self):
        terminal = Terminal(3, 20)
        data = b'\x1b[1;2;3;4:3;58:2::13:97:211;38;2;40;180;90mX\x1b[22;23;24;59;39mY'
        for byte in data:
            terminal.feed(bytes([byte]))
        x, y = terminal.snapshot()['cells'][0][:2]
        self.assertEqual(x, ['X', '28b45a', 'default', True, True, True, False, 3, '0d61d3'])
        self.assertEqual(y, ['Y', 'default', 'default', False, False, False, False, 0, 'default'])
        self.assertEqual(terminal.snapshot()['cursor'], [0, 2])

    def test_alternate_screen_preserves_primary_cells_and_cursor(self):
        terminal = Terminal(3, 10)
        terminal.feed(b'hello\x1b[?1049h\x1b[2;3Halt\x1b[?1049l')
        self.assertEqual(terminal.snapshot()['cursor'], [0, 5])
        self.assertEqual([c[0] for c in terminal.snapshot()['cells'][0][:5]], list('hello'))

    def test_unicode_combining_continuation_and_cursor(self):
        terminal = Terminal(3, 10)
        for byte in '界e\u0301'.encode():
            terminal.feed(bytes([byte]))
        self.assertEqual([c[0] for c in terminal.snapshot()['cells'][0][:3]], ['界', '', 'é'])
        self.assertEqual(terminal.snapshot()['cursor'], [0, 3])

    def test_equivalent_indexed_color_encodings_and_private_sgr(self):
        terminal = Terminal(2, 20)
        terminal.feed(b'\x1b[36mX\x1b[38;5;6mX\x1b[>4;1mY')
        cells = terminal.snapshot()['cells'][0]
        self.assertEqual(cells[0], cells[1])
        self.assertEqual(cells[2], ['Y', *cells[1][1:]])

    def test_cursor_shape_visibility_and_position_are_separate(self):
        terminal = Terminal(3, 10, default_cursor_shape='underline')
        terminal.feed(b'\x1b[6 q\x1b[?25l\x1b[2;4H')
        self.assertEqual(terminal.snapshot()['cursor_shape'], 'bar')
        self.assertEqual(terminal.snapshot()['cursor'], [1, 3])
        self.assertTrue(terminal.snapshot()['hidden'])
        terminal.feed(b'\x1bc')
        self.assertEqual(terminal.snapshot()['cursor_shape'], 'underline')

    def test_all_underline_styles_reset_and_color(self):
        terminal = Terminal(3, 20)
        for style in range(6):
            terminal.feed(f'\x1b[4:{style};58;5;45mX'.encode())
        self.assertEqual([c[7] for c in terminal.snapshot()['cells'][0][:6]], list(range(6)))
        terminal.feed(b'\x1b[0mY')
        self.assertEqual(terminal.snapshot()['cells'][0][6][7:], [0, 'default'])


if __name__ == '__main__':
    unittest.main()
