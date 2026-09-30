"""Independent pyte cell oracle, extended for ECMA-48 SGR and DEC 1049.

No mux modules, formatted-screen snapshots, or implementation expectations.
Resize clips at the top left; callers compare only after explicit repaint.
"""
import codecs
import copy
import re
from collections import namedtuple

import pyte
from pyte import graphics, modes

Char = namedtuple('Char', pyte.screens.Char._fields + ('dim', 'underline_style', 'underline_color'),
                  defaults=pyte.screens.Char.__new__.__defaults__ + (False, 0, 'default'))


class Screen(pyte.Screen):
    @property
    def default_char(self):
        return Char(' ', reverse=modes.DECSCNM in self.mode)

    def reset(self):
        super().reset()
        self.cursor.attrs = self.default_char

    def report_device_attributes(self, *args, **kwargs):
        pass  # Client capability negotiation is outside the cell oracle.

    def report_device_status(self, *args, **kwargs):
        pass  # The pane's actual recorded response is the source of truth.

    def sgr(self, parameters):
        # Preserve colon subparameters; pyte's parser otherwise discards them.
        tokens = parameters.split(';') if parameters else ['0']
        i = 0
        while i < len(tokens):
            fields = tokens[i].split(':')
            n = int(fields[0] or 0)
            i += 1
            attrs = self.cursor.attrs
            changes = {}
            if n == 0:
                self.cursor.attrs = self.default_char
                continue
            if n == 2:
                changes['dim'] = True
            elif n == 22:
                changes.update(dim=False, bold=False)
            elif n in (4, 21, 24):
                style = (int(fields[1] or 0) if len(fields) > 1 else 1) if n == 4 else (2 if n == 21 else 0)
                changes.update(underscore=bool(style), underline_style=style)
            elif n == 59:
                changes['underline_color'] = 'default'
            elif n in (38, 48, 58):
                key = {38: 'fg', 48: 'bg', 58: 'underline_color'}[n]
                if len(fields) > 1:
                    values = fields[1:]
                    mode = int(values.pop(0))
                    if mode == 2 and len(values) == 4:
                        values.pop(0)  # omitted/zero color-space identifier
                else:
                    mode = int(tokens[i])
                    i += 1
                    count = 3 if mode == 2 else 1
                    values = tokens[i:i + count]
                    i += count
                if mode == 2:
                    changes[key] = ''.join(f'{int(v):02x}' for v in values)
                elif mode == 5:
                    changes[key] = graphics.FG_BG_256[int(values[0])]
            else:
                super().select_graphic_rendition(n)
                continue
            self.cursor.attrs = attrs._replace(**changes)

    def alternate(self, enabled):
        if enabled and not hasattr(self, 'primary'):
            self.primary = (copy.deepcopy(self.buffer), copy.deepcopy(self.cursor), self.margins)
            self.buffer.clear()
            self.cursor_position()
            self.margins = None
        elif not enabled and hasattr(self, 'primary'):
            self.buffer, self.cursor, self.margins = self.primary
            del self.primary

    def scroll_up(self, count=1):
        saved = self.cursor.y
        top, bottom = self.margins or (0, self.lines - 1)
        self.cursor.y = bottom
        for _ in range(count or 1):
            self.index()
        self.cursor.y = saved

    def scroll_down(self, count=1):
        saved = self.cursor.y
        top, bottom = self.margins or (0, self.lines - 1)
        self.cursor.y = top
        for _ in range(count or 1):
            self.reverse_index()
        self.cursor.y = saved


class Stream(pyte.Stream):
    csi = pyte.Stream.csi | {'S': 'scroll_up', 'T': 'scroll_down'}
    events = pyte.Stream.events | {'scroll_up', 'scroll_down'}


class Terminal:
    def __init__(self, rows, cols):
        self.screen = Screen(cols, rows)
        self.stream = Stream(self.screen)
        self.decoder = codecs.getincrementaldecoder('utf8')('strict')
        self.pending = ''

    def feed(self, data):
        self.pending += self.decoder.decode(data)
        # Separate complete control strings before feeding pyte. Keep partial
        # strings, including UTF-8, across arbitrary PTY read boundaries.
        while self.pending:
            start = self.pending.find('\x1b')
            if start < 0:
                self.stream.feed(self.pending)
                self.pending = ''
                break
            if start:
                self.stream.feed(self.pending[:start])
                self.pending = self.pending[start:]
            if len(self.pending) < 2:
                break
            if self.pending[1] == '[':
                match = re.match(r'\x1b\[([0-?]*)([ -/]*)([@-~])', self.pending)
                if not match:
                    break
                raw, intermediate, final = match.groups()
                seq = match.group(0)
                if final == 'm' and not intermediate:
                    self.screen.sgr(raw)
                elif raw == '?1049' and final in 'hl':
                    self.screen.alternate(final == 'h')
                else:
                    self.stream.feed(seq)
                self.pending = self.pending[len(seq):]
            elif self.pending[1] in ']P^_':
                match = re.search(r'\x07|\x1b\\', self.pending[2:])
                if not match:
                    break
                end = 2 + match.end()
                if self.pending[1] == ']':
                    self.stream.feed(self.pending[:end])
                self.pending = self.pending[end:]
            else:
                end = 3 if self.pending[1] in '()*+#%' else 2
                if len(self.pending) < end:
                    break
                self.stream.feed(self.pending[:end])
                self.pending = self.pending[end:]

    def resize(self, rows, cols):
        self.screen.resize(lines=rows, columns=cols)

    def snapshot(self, left=0, width=None):
        screen = self.screen
        width = screen.columns - left if width is None else width
        fields = ('data', 'fg', 'bg', 'bold', 'dim', 'italics', 'reverse', 'underline_style', 'underline_color')
        cells = []
        for y in range(screen.lines):
            row = []
            for x in range(left, left + width):
                cell = screen.buffer[y][x]
                values = [getattr(cell, f) for f in fields]
                # pyte normalizes combining marks to NFC; compare consistently.
                import unicodedata
                values[0] = unicodedata.normalize('NFC', values[0])
                row.append(values)
            cells.append(row)
        return {'cells': cells, 'cursor': [screen.cursor.y, min(screen.cursor.x, screen.columns - 1) - left],
                'hidden': screen.cursor.hidden}
