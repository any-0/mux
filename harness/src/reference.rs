//! The independent reference emulator: alacritty's terminal, which tracks
//! xterm closely and shares no code with mux.

use alacritty_terminal::{
    event::VoidListener,
    grid::Dimensions,
    index::{Column, Line},
    term::{Config, Term, TermMode, cell::Cell, test::TermSize},
    vte::ansi::{Processor, StdSyncHandler},
};

use crate::screen::Snapshot;

pub struct Reference {
    term: Term<VoidListener>,
    parser: Processor<StdSyncHandler>,
    /// A mirror of the tab stops, for the one tab operation alacritty gets
    /// wrong (see [`Self::feed_token`]).
    tabs: Vec<bool>,
    /// A mirror of the scroll region, top and bottom rows inclusive.
    region: (usize, usize),
    /// Origin mode, and the value DECSC saved, which alacritty's DECRC does
    /// not restore.
    origin: bool,
    /// Whether G1 is shifted in; alacritty keeps the shift out of its saved
    /// cursor.
    shifted: bool,
    /// What DECSC saved of both, per buffer: `(origin, shifted)`.
    saved: [(bool, bool); 2],
    alternate: bool,
    /// Set when alacritty marked a wrap pending while autowrap was off.
    phantom_wrap: bool,
}

fn default_tabs(cols: usize) -> Vec<bool> {
    (0..cols).map(|col| col > 0 && col % 8 == 0).collect()
}

/// `(params, final)` of a lone CSI sequence without intermediates.
fn csi(token: &[u8]) -> Option<(Vec<u16>, u8)> {
    let body = token.strip_prefix(b"\x1b[")?;
    let (&last, params) = body.split_last()?;
    if !params.iter().all(|b| b.is_ascii_digit() || *b == b';') {
        return None;
    }
    let params = std::str::from_utf8(params)
        .ok()?
        .split(';')
        .filter(|p| !p.is_empty())
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    Some((params, last))
}

impl Reference {
    pub fn new(rows: usize, cols: usize) -> Self {
        Self::with_history(rows, cols, 0)
    }

    pub fn with_history(rows: usize, cols: usize, history: usize) -> Self {
        let config = Config {
            scrolling_history: history,
            ..Config::default()
        };
        Self {
            term: Term::new(config, &TermSize::new(cols, rows), VoidListener),
            parser: Processor::new(),
            tabs: default_tabs(cols),
            region: (0, rows - 1),
            origin: false,
            shifted: false,
            saved: [(false, false); 2],
            alternate: false,
            phantom_wrap: false,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Feeds one generated token, correcting the places where alacritty
    /// departs from xterm (and tmux) that generated streams reach. Each was
    /// confirmed against alacritty_terminal 0.26's source:
    ///
    /// - a tab at the right margin with a wrap pending breaks the line
    ///   (`put_tab`); elsewhere it does nothing;
    /// - `ED 1` on the second row leaves the first row alone (`cursor.line >
    ///   1` in `clear_screen`);
    /// - `DCH n` past the end of the line blanks the last `n` cells rather
    ///   than only the ones from the cursor on (`delete_chars`);
    /// - with autowrap off a wrap is still marked pending at the last column;
    /// - `CBT` with no tab stop to the left stays put instead of going to the
    ///   first column;
    /// - `CUU`, `CUD`, `CNL` and `CPL` ignore the scroll region's margins
    ///   when the cursor starts inside it;
    /// - `ECH`, `ICH`, `DCH`, `EL 0` and `ED 0` with a wrap pending act on the last
    ///   cell, where tmux (whose cursor is past it) leaves it alone;
    /// - DECSTR (soft reset) is not implemented at all;
    /// - DECRC does not restore the origin mode or the GL shift DECSC saved;
    /// - CHA in origin mode also moves the cursor down by the top margin.
    pub fn feed_token(&mut self, token: &[u8]) {
        // A combining mark needs alacritty's flag to find its base; anything
        // else sees the cursor where xterm keeps it.
        let zero_width = std::str::from_utf8(token).is_ok_and(|text| {
            let mut chars = text.chars();
            chars
                .next()
                .is_some_and(|c| unicode_width::UnicodeWidthChar::width(c) == Some(0))
                && chars.next().is_none()
        });
        if !zero_width && std::mem::take(&mut self.phantom_wrap) {
            self.term.grid_mut().cursor.input_needs_wrap = false;
        }
        self.feed_adjusted(token);
        // Printing with autowrap off leaves the cursor on the last column in
        // xterm; a wrap already pending when autowrap went off stays pending
        // until then.
        let printed = !token.contains(&0x1b) && !token.iter().all(u8::is_ascii_control);
        if !self.term.mode().contains(TermMode::LINE_WRAP)
            && self.term.grid().cursor.input_needs_wrap
            && printed
        {
            self.phantom_wrap = true;
        }
    }

    fn feed_adjusted(&mut self, token: &[u8]) {
        let cols = self.term.columns();
        let (row, col) = self.cursor();
        if token == b"\t" && self.term.grid().cursor.input_needs_wrap {
            return;
        }
        let rows = self.term.screen_lines();
        match token {
            b"\x1bH" => self.tabs[col.min(cols - 1)] = true,
            b"\x1bc" => {
                self.tabs = default_tabs(cols);
                self.region = (0, rows - 1);
                self.origin = false;
                self.shifted = false;
                self.saved = [(false, false); 2];
                self.alternate = false;
            }
            b"\x0e" => self.shifted = true,
            b"\x0f" => self.shifted = false,
            b"\x1b7" | b"\x1b[s" => {
                self.saved[usize::from(self.alternate)] = (self.origin, self.shifted);
            }
            b"\x1b[?1049h" => {
                if !self.alternate {
                    self.saved[0] = (self.origin, self.shifted);
                    self.alternate = true;
                }
            }
            b"\x1b8" | b"\x1b[u" | b"\x1b[?1049l" => {
                if token == b"\x1b[?1049l" {
                    if !self.alternate {
                        self.feed(token);
                        return;
                    }
                    self.alternate = false;
                }
                self.feed(token);
                (self.origin, self.shifted) = self.saved[usize::from(self.alternate)];
                self.feed(if self.shifted { b"\x0e" } else { b"\x0f" });
                let point = self.term.grid().cursor.point;
                let pending = self.term.grid().cursor.input_needs_wrap;
                self.feed(if self.origin {
                    b"\x1b[?6h"
                } else {
                    b"\x1b[?6l"
                });
                let cursor = &mut self.term.grid_mut().cursor;
                cursor.point = point;
                cursor.input_needs_wrap = pending;
                return;
            }
            b"\x1b[!p" => {
                // alacritty has no DECSTR. xterm's soft reset clears the
                // rendition, insert and origin modes, the margins and the
                // character sets, and shows the cursor, all without moving it.
                self.region = (0, rows - 1);
                self.origin = false;
                self.shifted = false;
                self.saved[usize::from(self.alternate)] = (false, false);
                let point = self.term.grid().cursor.point;
                let pending = self.term.grid().cursor.input_needs_wrap;
                // The saved cursor goes home with a plain rendition too.
                self.feed(
                    b"\x1b[m\x1b[4l\x1b[?6l\x1b[r\x1b[?25h\x1b[?1l\x1b>\x1b(B\x1b)B\x0f\x1b[H\x1b7",
                );
                let cursor = &mut self.term.grid_mut().cursor;
                cursor.point = point;
                cursor.input_needs_wrap = pending;
                return;
            }
            _ => {}
        }
        if let Some((params, last)) = csi(token) {
            let first = params.first().copied().unwrap_or(0);
            let pending = self.term.grid().cursor.input_needs_wrap;
            // With a wrap pending the cursor is past the last column, so
            // these touch nothing in tmux; alacritty acts on the last cell.
            if pending
                && (matches!(last, b'X' | b'@' | b'P')
                    || (matches!(last, b'K' | b'J') && first == 0))
            {
                if last == b'J' {
                    // The rows below still clear.
                    let bg = self.term.grid().cursor.template.bg;
                    for line in row + 1..rows {
                        for column in 0..cols {
                            let mut blank = Cell::default();
                            blank.bg = bg;
                            self.term.grid_mut()[Line(line as i32)][Column(column)] = blank;
                        }
                    }
                }
                return;
            }
            match last {
                b'g' if first == 0 => self.tabs[col.min(cols - 1)] = false,
                b'g' if first == 3 => self.tabs.iter_mut().for_each(|t| *t = false),
                b'J' if first == 1 && row == 1 => {
                    self.feed(token);
                    let bg = self.term.grid().cursor.template.bg;
                    for column in 0..cols {
                        let mut blank = Cell::default();
                        blank.bg = bg;
                        self.term.grid_mut()[Line(0)][Column(column)] = blank;
                    }
                    return;
                }
                b'r' => {
                    let top = params.first().copied().filter(|p| *p > 0).unwrap_or(1) as usize;
                    let bottom = params
                        .get(1)
                        .copied()
                        .filter(|p| *p > 0)
                        .unwrap_or(rows as u16) as usize;
                    let bottom = bottom.min(rows);
                    if top < bottom {
                        self.region = (top - 1, bottom - 1);
                    }
                }
                b'A' | b'B' | b'E' | b'F' => {
                    let count = first.max(1) as usize;
                    let (top, bottom) = if (self.region.0..=self.region.1).contains(&row) {
                        self.region
                    } else {
                        (0, rows - 1)
                    };
                    let target = if matches!(last, b'A' | b'F') {
                        row.saturating_sub(count).max(top)
                    } else {
                        (row + count).min(bottom)
                    };
                    let column = if matches!(last, b'E' | b'F') { 0 } else { col };
                    let cursor = &mut self.term.grid_mut().cursor;
                    cursor.point.line = Line(target as i32);
                    cursor.point.column = Column(column);
                    cursor.input_needs_wrap = false;
                    return;
                }
                b'G' | b'`' => {
                    let cursor = &mut self.term.grid_mut().cursor;
                    cursor.point.column = Column((first.max(1) as usize).min(cols) - 1);
                    cursor.input_needs_wrap = false;
                    return;
                }
                b'P' => {
                    let count = (first.max(1) as usize).min(cols - col);
                    self.feed(format!("\x1b[{count}P").as_bytes());
                    return;
                }
                b'Z' => {
                    let mut target = col.min(cols - 1);
                    for _ in 0..first.max(1) {
                        target = (0..target).rev().find(|c| self.tabs[*c]).unwrap_or(0);
                    }
                    // Set directly: alacritty's CHA re-applies the origin
                    // offset to the row in origin mode.
                    let cursor = &mut self.term.grid_mut().cursor;
                    cursor.point.column = Column(target);
                    cursor.input_needs_wrap = false;
                    return;
                }
                _ => {}
            }
        }
        if token.starts_with(b"\x1b[?6h") {
            self.origin = true;
        } else if token.starts_with(b"\x1b[?6l") {
            self.origin = false;
        }
        self.feed(token);
    }

    pub fn resize(&mut self, rows: usize, cols: usize) {
        self.term.resize(TermSize::new(cols, rows));
        self.region = (0, rows - 1);
        let old = self.tabs.len();
        self.tabs.resize(cols, false);
        for col in old..cols {
            self.tabs[col] = col > 0 && col % 8 == 0;
        }
    }

    fn cursor(&self) -> (usize, usize) {
        let point = self.term.grid().cursor.point;
        (point.line.0 as usize, point.column.0)
    }

    /// Whether alacritty has torn a wide character in two: a wide base with
    /// no spacer after it, or a spacer with no base before it. It does this
    /// when insert mode or ICH pushes one off the right edge and when ECH or
    /// EL erases only one half; from then on it is no reference at all.
    pub fn insert_mode(&self) -> bool {
        self.term.mode().contains(TermMode::INSERT)
    }

    pub fn torn(&self) -> bool {
        use alacritty_terminal::term::cell::Flags;
        let grid = self.term.grid();
        let cols = grid.columns();
        (0..grid.screen_lines()).any(|line| {
            let row = &grid[Line(line as i32)];
            (0..cols).any(|col| {
                let flags = row[Column(col)].flags;
                let next = (col + 1 < cols).then(|| row[Column(col + 1)].flags);
                let previous = (col > 0).then(|| row[Column(col - 1)].flags);
                (flags.contains(Flags::WIDE_CHAR)
                    && !next.is_some_and(|next| next.contains(Flags::WIDE_CHAR_SPACER)))
                    || (flags.contains(Flags::WIDE_CHAR_SPACER)
                        && !previous.is_some_and(|previous| previous.contains(Flags::WIDE_CHAR)))
            })
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot::from_alacritty(&self.term)
    }
}
