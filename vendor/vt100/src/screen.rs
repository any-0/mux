use crate::grid::Pos;
use unicode_width::UnicodeWidthChar as _;

const MODE_APPLICATION_KEYPAD: u8 = 0b0000_0001;
const MODE_APPLICATION_CURSOR: u8 = 0b0000_0010;
const MODE_HIDE_CURSOR: u8 = 0b0000_0100;
const MODE_ALTERNATE_SCREEN: u8 = 0b0000_1000;
const MODE_BRACKETED_PASTE: u8 = 0b0001_0000;
const MODE_INSERT: u8 = 0b0010_0000;
const MODE_NO_AUTOWRAP: u8 = 0b0100_0000;
const MODE_FOCUS_REPORTING: u8 = 0b1000_0000;

/// A character set that can be designated into G0 or G1.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
enum Charset {
    #[default]
    Ascii,
    /// DEC Special Graphics: the line-drawing set curses selects with
    /// `smacs`.
    DecGraphics,
    /// The United Kingdom set, where `#` is a pound sign.
    Uk,
}

impl Charset {
    fn translate(self, c: char) -> char {
        match (self, c) {
            (Self::Uk, '#') => '£',
            (Self::DecGraphics, '_'..='~') => {
                const GRAPHICS: [char; 32] = [
                    ' ', '◆', '▒', '␉', '␌', '␍', '␊', '°', '±', '␤', '␋', '┘', '┐', '┌', '└', '┼',
                    '⎺', '⎻', '─', '⎼', '⎽', '├', '┤', '┴', '┬', '│', '≤', '≥', 'π', '≠', '£', '·',
                ];
                GRAPHICS[usize::from(u8::try_from(c).unwrap() - b'_')]
            }
            _ => c,
        }
    }
}

/// G0 and G1, and which of them is invoked into GL.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
struct Charsets {
    designated: [Charset; 2],
    shifted_out: bool,
}

impl Charsets {
    fn translate(self, c: char) -> char {
        if c.is_ascii() {
            self.designated[usize::from(self.shifted_out)].translate(c)
        } else {
            c
        }
    }
}

fn default_tab_stop(col: usize) -> bool {
    col > 0 && col % 8 == 0
}

/// The xterm mouse handling mode currently in use.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum MouseProtocolMode {
    /// Mouse handling is disabled.
    #[default]
    None,

    /// Mouse button events should be reported on button press. Also known as
    /// X10 mouse mode.
    Press,

    /// Mouse button events should be reported on button press and release.
    /// Also known as VT200 mouse mode.
    PressRelease,

    /// Mouse button events should be reported on button press and release, as
    /// well as when the mouse moves between cells while a button is held
    /// down.
    ButtonMotion,

    /// Mouse button events should be reported on button press and release,
    /// and mouse motion events should be reported when the mouse moves
    /// between cells regardless of whether a button is held down or not.
    AnyMotion,
}

/// The encoding to use for the enabled [`MouseProtocolMode`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub enum MouseProtocolEncoding {
    /// Default single-printable-byte encoding.
    #[default]
    Default,

    /// UTF-8-based encoding.
    Utf8,

    /// SGR-like encoding.
    Sgr,
}

/// How an extended colour SGR (38, 48 or 58) turned out.
enum ExtendedColor {
    Color(crate::Color),
    /// Not understood; the rest of the SGR still applies.
    Unhandled,
    /// Malformed; the rest of the SGR is dropped.
    Abort,
    /// Not understood, and the rest of the SGR is dropped.
    UnhandledAbort,
}

/// Represents the overall terminal state.
#[derive(Clone, Debug)]
pub struct Screen {
    grid: crate::grid::Grid,
    alternate_grid: crate::grid::Grid,

    attrs: crate::attrs::Attrs,
    /// What DECSC saved, one slot per buffer as in xterm.
    saved_attrs: [crate::attrs::Attrs; 2],

    charsets: Charsets,
    saved_charsets: [Charsets; 2],
    tab_stops: Vec<bool>,
    /// The last graphic character printed, which REP repeats.
    last_char: Option<char>,
    /// Where the last character with a width was drawn. Without autowrap the
    /// cursor stays on the last column after drawing there, and a combining
    /// mark that follows belongs to that cell rather than the one before it.
    last_drawn: Option<Pos>,

    modes: u8,
    mouse_protocol_mode: MouseProtocolMode,
    mouse_protocol_encoding: MouseProtocolEncoding,
}

impl Screen {
    pub(crate) fn new(size: crate::grid::Size, scrollback_len: usize) -> Self {
        let mut grid = crate::grid::Grid::new(size, scrollback_len, true);
        grid.allocate_rows();
        Self {
            grid,
            alternate_grid: crate::grid::Grid::new(size, 0, false),

            attrs: crate::attrs::Attrs::default(),
            saved_attrs: [crate::attrs::Attrs::default(); 2],

            charsets: Charsets::default(),
            saved_charsets: [Charsets::default(); 2],
            tab_stops: (0..usize::from(size.cols)).map(default_tab_stop).collect(),
            last_char: None,
            last_drawn: None,

            modes: 0,
            mouse_protocol_mode: MouseProtocolMode::default(),
            mouse_protocol_encoding: MouseProtocolEncoding::default(),
        }
    }

    /// Resizes the terminal.
    pub fn set_size(&mut self, rows: u16, cols: u16) {
        self.grid.set_size(crate::grid::Size { rows, cols });
        self.alternate_grid
            .set_size(crate::grid::Size { rows, cols });
        let old_cols = self.tab_stops.len();
        self.tab_stops.truncate(usize::from(cols));
        self.tab_stops
            .extend((old_cols..usize::from(cols)).map(default_tab_stop));
    }

    /// Returns the current size of the terminal.
    ///
    /// The return value will be (rows, cols).
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        let size = self.grid().size();
        (size.rows, size.cols)
    }

    /// Scrolls to the given position in the scrollback.
    ///
    /// This position indicates the offset from the top of the screen, and
    /// should be `0` to put the normal screen in view.
    ///
    /// This affects the return values of methods called on the screen: for
    /// instance, `screen.cell(0, 0)` will return the top left corner of the
    /// screen after taking the scrollback offset into account.
    ///
    /// The value given will be clamped to the actual size of the scrollback.
    pub fn set_scrollback(&mut self, rows: usize) {
        self.grid_mut().set_scrollback(rows);
    }

    /// Returns the current position in the scrollback.
    ///
    /// This position indicates the offset from the top of the screen, and is
    /// `0` when the normal screen is in view.
    #[must_use]
    pub fn scrollback(&self) -> usize {
        self.grid().scrollback()
    }

    /// Returns the bytes retained by scrollback rows and their contents.
    #[must_use]
    pub fn history_bytes(&self) -> usize {
        self.grid().history_bytes()
    }

    /// Moves immutable scrollback blocks to `file` on a background writer.
    pub fn set_history_backing(&mut self, file: std::fs::File) {
        self.grid.set_history_backing(file);
    }

    /// Waits until all immutable scrollback blocks have reached their backing.
    pub fn flush_history_backing(&self) {
        self.grid().flush_history_backing();
    }

    /// Returns the text contents of the terminal.
    ///
    /// This will not include any formatting information, and will be in plain
    /// text format.
    #[must_use]
    pub fn contents(&self) -> String {
        let mut contents = String::new();
        self.grid().write_contents(&mut contents);
        contents
    }

    /// Returns the text contents of the terminal by row, restricted to the
    /// given subset of columns.
    ///
    /// This will not include any formatting information, and will be in plain
    /// text format.
    ///
    /// Newlines will not be included.
    pub fn rows(&self, start: u16, width: u16) -> impl Iterator<Item = String> + '_ {
        self.grid().visible_rows().map(move |row| {
            let mut contents = String::new();
            row.write_contents(&mut contents, start, width, false);
            contents
        })
    }

    /// Return escape codes sufficient to reproduce the entire contents of the
    /// current terminal state: its visible contents and its input modes
    /// (application keypad and cursor, bracketed paste and xterm mouse
    /// support).
    #[must_use]
    pub fn state_formatted(&self) -> Vec<u8> {
        let mut contents = self.contents_formatted();
        self.write_input_mode(&mut contents, None);
        contents
    }

    /// Return escape codes sufficient to turn the terminal state of the
    /// screen `prev` into the current terminal state. This is the
    /// [`contents_diff`](Self::contents_diff) followed by whatever input
    /// modes changed.
    #[must_use]
    pub fn state_diff(&self, prev: &Self) -> Vec<u8> {
        let mut contents = self.contents_diff(prev);
        self.write_input_mode(&mut contents, Some(prev));
        contents
    }

    /// The current drawing attributes as SGR sequences, relative to the
    /// default attributes.
    #[must_use]
    pub fn attributes_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        self.attrs
            .write_escape_code_diff(&mut contents, &crate::attrs::Attrs::default());
        contents
    }

    /// Returns the formatted visible contents of the terminal.
    ///
    /// Formatting information will be included inline as terminal escape
    /// codes. The result will be suitable for feeding directly to a raw
    /// terminal parser, and will result in the same visual output.
    #[must_use]
    pub fn contents_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        crate::term::hide_cursor(&mut contents, self.hide_cursor());
        let prev_attrs = self.grid().write_contents_formatted(&mut contents);
        self.attrs
            .write_escape_code_diff(&mut contents, &prev_attrs);
        contents
    }

    /// Returns a terminal byte stream sufficient to turn the visible contents
    /// of the screen described by `prev` into the visible contents of the
    /// screen described by `self`.
    ///
    /// The result of rendering `prev.contents_formatted()` followed by
    /// `self.contents_diff(prev)` should be equivalent to the result of
    /// rendering `self.contents_formatted()`. This is primarily useful when
    /// you already have a terminal parser whose state is described by `prev`,
    /// since the diff will likely require less memory and cause less
    /// flickering than redrawing the entire screen contents.
    #[must_use]
    pub fn contents_diff(&self, prev: &Self) -> Vec<u8> {
        let mut contents = vec![];
        if self.hide_cursor() != prev.hide_cursor() {
            crate::term::hide_cursor(&mut contents, self.hide_cursor());
        }
        let prev_attrs = self
            .grid()
            .write_contents_diff(&mut contents, prev.grid(), prev.attrs);
        self.attrs
            .write_escape_code_diff(&mut contents, &prev_attrs);
        contents
    }

    /// Writes the input modes that differ from `prev`, or all of them.
    fn write_input_mode(&self, contents: &mut Vec<u8>, prev: Option<&Self>) {
        let modes: [(u8, &[u8], &[u8]); 3] = [
            (MODE_APPLICATION_KEYPAD, b"\x1b=", b"\x1b>"),
            (MODE_APPLICATION_CURSOR, b"\x1b[?1h", b"\x1b[?1l"),
            (MODE_BRACKETED_PASTE, b"\x1b[?2004h", b"\x1b[?2004l"),
        ];
        for (mode, set, reset) in modes {
            let on = self.mode(mode);
            if prev.is_none_or(|prev| prev.mode(mode) != on) {
                contents.extend_from_slice(if on { set } else { reset });
            }
        }
        crate::term::mouse_protocol_mode(
            contents,
            self.mouse_protocol_mode,
            prev.map_or(MouseProtocolMode::None, |prev| prev.mouse_protocol_mode),
        );
        crate::term::mouse_protocol_encoding(
            contents,
            self.mouse_protocol_encoding,
            prev.map_or(MouseProtocolEncoding::Default, |prev| {
                prev.mouse_protocol_encoding
            }),
        );
    }

    /// Returns the current cursor position of the terminal.
    ///
    /// The return value will be (row, col).
    #[must_use]
    pub fn cursor_position(&self) -> (u16, u16) {
        let pos = self.grid().pos();
        (pos.row, pos.col)
    }

    /// Returns the [`Cell`](crate::Cell) object at the given location in the
    /// terminal, if it exists.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<crate::Cell> {
        self.grid().visible_cell(Pos { row, col })
    }

    /// Returns the cells in one visible row.
    pub fn row_cells(&self, row: u16) -> impl Iterator<Item = crate::Cell> + '_ {
        self.grid()
            .visible_row(row)
            .into_iter()
            .flat_map(crate::row::Row::cells)
    }

    /// Packs the scrollback for storage, without unpacking a single row.
    #[must_use]
    pub fn encode_history(&self) -> Vec<u8> {
        self.grid().encode_history()
    }

    /// Puts a packed scrollback back. Returns whether it could be read; a
    /// screen whose history cannot be read keeps the empty one it had.
    pub fn restore_history(&mut self, packed: &[u8]) -> bool {
        self.grid_mut().restore_history(packed)
    }

    /// How many rows of scrollback the screen is holding.
    #[must_use]
    pub fn history_rows(&self) -> usize {
        self.grid().history_rows()
    }

    /// Every row in view, from the top of the scrollback to the last screen
    /// row, without decoding any of them.
    pub fn all_rows(&self) -> impl Iterator<Item = &crate::row::Row> {
        self.grid().all_rows()
    }

    /// How many cells of row `row` are worth reading; the rest are blank.
    #[must_use]
    pub fn row_used_cells(&self, row: u16) -> u16 {
        self.grid()
            .visible_row(row)
            .map_or(0, crate::row::Row::used_cells)
    }

    /// Returns whether the text in row `row` should wrap to the next line.
    #[must_use]
    pub fn row_wrapped(&self, row: u16) -> bool {
        self.grid()
            .visible_row(row)
            .is_some_and(crate::row::Row::wrapped)
    }

    /// Returns whether the alternate screen is currently in use.
    #[must_use]
    pub fn alternate_screen(&self) -> bool {
        self.mode(MODE_ALTERNATE_SCREEN)
    }

    /// Returns whether the terminal should be in application cursor mode.
    #[must_use]
    pub fn application_cursor(&self) -> bool {
        self.mode(MODE_APPLICATION_CURSOR)
    }

    /// Returns whether the terminal should be in hide cursor mode.
    #[must_use]
    pub fn hide_cursor(&self) -> bool {
        self.mode(MODE_HIDE_CURSOR)
    }

    /// Returns whether the terminal should be in bracketed paste mode.
    #[must_use]
    pub fn bracketed_paste(&self) -> bool {
        self.mode(MODE_BRACKETED_PASTE)
    }

    /// Whether the program asked to be told when the terminal gains or loses
    /// focus (mode 1004).
    #[must_use]
    pub fn focus_reporting(&self) -> bool {
        self.mode(MODE_FOCUS_REPORTING)
    }

    /// Returns the currently active [`MouseProtocolMode`].
    #[must_use]
    pub fn mouse_protocol_mode(&self) -> MouseProtocolMode {
        self.mouse_protocol_mode
    }

    /// Returns the currently active [`MouseProtocolEncoding`].
    #[must_use]
    pub fn mouse_protocol_encoding(&self) -> MouseProtocolEncoding {
        self.mouse_protocol_encoding
    }

    pub(crate) fn grid(&self) -> &crate::grid::Grid {
        if self.mode(MODE_ALTERNATE_SCREEN) {
            &self.alternate_grid
        } else {
            &self.grid
        }
    }

    fn grid_mut(&mut self) -> &mut crate::grid::Grid {
        if self.mode(MODE_ALTERNATE_SCREEN) {
            &mut self.alternate_grid
        } else {
            &mut self.grid
        }
    }

    // There is one cursor, one pair of margins and one origin mode, shared
    // by both buffers: switching screens leaves them as they were.
    fn enter_alternate_grid(&mut self) {
        if self.mode(MODE_ALTERNATE_SCREEN) {
            return;
        }
        self.grid_mut().set_scrollback(0);
        let shared = self.grid.shared_state();
        self.set_mode(MODE_ALTERNATE_SCREEN);
        self.alternate_grid.allocate_rows();
        self.alternate_grid.set_shared_state(shared);
    }

    fn exit_alternate_grid(&mut self) {
        if !self.mode(MODE_ALTERNATE_SCREEN) {
            return;
        }
        let shared = self.alternate_grid.shared_state();
        self.clear_mode(MODE_ALTERNATE_SCREEN);
        self.grid.set_shared_state(shared);
    }

    /// Keeps both grids filling new blanks with the current background.
    fn sync_fill(&mut self) {
        let fill = self.erase_attrs();
        self.grid.set_fill(fill);
        self.alternate_grid.set_fill(fill);
    }

    /// What erased cells become: the current background and nothing else.
    fn erase_attrs(&self) -> crate::attrs::Attrs {
        crate::attrs::Attrs {
            bgcolor: self.attrs.bgcolor,
            ..Default::default()
        }
    }

    /// Which buffer is showing: 0 for the normal screen, 1 for the
    /// alternate one.
    fn buffer(&self) -> usize {
        usize::from(self.mode(MODE_ALTERNATE_SCREEN))
    }

    fn set_mode(&mut self, mode: u8) {
        self.modes |= mode;
    }

    fn clear_mode(&mut self, mode: u8) {
        self.modes &= !mode;
    }

    fn mode(&self, mode: u8) -> bool {
        self.modes & mode != 0
    }

    fn clear_mouse_mode(&mut self, mode: MouseProtocolMode) {
        if self.mouse_protocol_mode == mode {
            self.mouse_protocol_mode = MouseProtocolMode::default();
        }
    }

    fn clear_mouse_encoding(&mut self, encoding: MouseProtocolEncoding) {
        if self.mouse_protocol_encoding == encoding {
            self.mouse_protocol_encoding = MouseProtocolEncoding::default();
        }
    }
}

impl Screen {
    pub(crate) fn text(&mut self, c: char) {
        let c = self.charsets.translate(c);
        if c.width().is_some_and(|width| width > 0) {
            self.last_char = Some(c);
        }
        self.put_char(c);
    }

    // The grid keeps the cursor row valid, and col_wrap() leaves room for
    // the character at the cursor, so the unwraps below cannot fail.
    fn put_char(&mut self, c: char) {
        let pos = self.grid().pos();
        let size = self.grid().size();
        let attrs = self.attrs;

        let width = c.width();
        if width.is_none() && (u32::from(c)) < 256 {
            // don't even try to draw control characters
            return;
        }
        // width() can only return 0, 1, or 2
        let width: u16 = width.unwrap_or(1).try_into().unwrap();
        if width > size.cols {
            // A grid narrower than the character has no pair of columns to
            // hold it. Everything below assumes the second half of a wide
            // character has somewhere to go, so there is nothing to draw.
            return;
        }

        if width > 0 && self.mode(MODE_NO_AUTOWRAP) && pos.col > size.cols - width {
            // Without autowrap, text at the right margin overwrites the last
            // column rather than moving to the next row, and a wide character
            // that no longer fits is dropped (as tmux and alacritty do).
            if width > 1 {
                self.grid_mut().clear_pending_wrap();
                return;
            }
            self.grid_mut().col_set(size.cols - width);
        }
        let pos = self.grid().pos();
        // it doesn't make any sense to wrap if the last column in a row
        // didn't already have contents. don't try to handle the case where a
        // character wraps because there was only one column left in the
        // previous row - literally everything handles this case differently,
        // and this is tmux behavior (and also the simplest).
        let mut wrap = false;
        if pos.col > size.cols - width {
            let last_cell = self.grid().drawing_cell(Pos {
                row: pos.row,
                col: size.cols - 1,
            });
            // A wide character that does not fit in the last column continues
            // the same line on the next row, so that row is wrapped too.
            if pos.col < size.cols
                || last_cell.is_some_and(|cell| cell.has_contents() || cell.is_wide_continuation())
            {
                wrap = true;
            }
        }
        if width > 1 && pos.col < size.cols && pos.col > size.cols - width {
            // A wide character that does not fit wraps whole, and the column
            // it left behind is blanked, as if a space had been written there,
            // rather than keeping a stale glyph.
            for col in pos.col..size.cols {
                let grid = self.grid_mut();
                let Some(cell) = grid.drawing_cell_mut(Pos { row: pos.row, col }) else {
                    continue;
                };
                let continuation = cell.is_wide_continuation();
                cell.clear(attrs);
                if continuation && col > 0 {
                    if let Some(base) = grid.drawing_cell_mut(Pos {
                        row: pos.row,
                        col: col - 1,
                    }) {
                        base.clear(attrs);
                    }
                }
            }
        }
        self.grid_mut().col_wrap(width, wrap);
        let pos = self.grid().pos();

        if width == 0 {
            if self.mode(MODE_NO_AUTOWRAP)
                && pos.col + 1 == size.cols
                && self.last_drawn == Some(pos)
            {
                self.append_combining(pos, c);
            } else if pos.col > 0 {
                self.append_combining(
                    Pos {
                        row: pos.row,
                        col: pos.col - 1,
                    },
                    c,
                );
            } else if pos.row > 0 && self.grid().drawing_row(pos.row - 1).unwrap().wrapped() {
                self.append_combining(
                    Pos {
                        row: pos.row - 1,
                        col: size.cols - 1,
                    },
                    c,
                );
            }
            return;
        }

        if self.mode(MODE_INSERT) {
            self.grid_mut().insert_cells(width);
        }
        let grid = self.grid_mut();
        let cell = grid.drawing_cell(pos).unwrap();
        if cell.is_wide_continuation() {
            // The orphaned first half becomes a blank in its own colours.
            // A continuation with nothing before it should not exist, but
            // must never be a reason to panic the terminal.
            if let Some(prev_cell) = pos
                .col
                .checked_sub(1)
                .and_then(|col| grid.drawing_cell_mut(Pos { row: pos.row, col }))
            {
                let own = *prev_cell.attrs();
                prev_cell.clear(own);
            }
        }
        if cell.is_wide() {
            // The second half normally follows; a one-column screen keeps
            // it on the next row instead, where it is left alone.
            if let Some(next_cell) = grid.drawing_cell_mut(Pos {
                row: pos.row,
                col: pos.col + 1,
            }) {
                let own = *next_cell.attrs();
                next_cell.clear(own);
            }
        }

        grid.drawing_cell_mut(pos).unwrap().set(c, attrs);
        grid.col_inc(1);
        if width > 1 {
            // col_wrap() took the width into account, so this is still on
            // the grid.
            let pos = grid.pos();
            if grid.drawing_cell(pos).unwrap().is_wide() {
                let next_next_pos = Pos {
                    row: pos.row,
                    col: pos.col + 1,
                };
                if let Some(next_next_cell) = grid.drawing_cell_mut(next_next_pos) {
                    next_next_cell.clear(attrs);
                }
                if next_next_pos.col == size.cols - 1 {
                    grid.drawing_row_mut(pos.row).unwrap().wrap(false);
                }
            }
            let next_cell = grid.drawing_cell_mut(pos).unwrap();
            // The right half carries the character's rendition, which is
            // what it shows if the left half is ever overwritten.
            next_cell.clear(attrs);
            next_cell.set_wide_continuation();
            grid.col_inc(1);
        }
        if self.mode(MODE_NO_AUTOWRAP) {
            // Without autowrap there is no pending wrap: the cursor stays
            // on the last column.
            self.grid_mut().clear_pending_wrap();
        }
        self.last_drawn = Some(pos);
    }

    /// Attaches the combining mark `c` to the character at `pos`, or whose
    /// second half is at `pos`.
    fn append_combining(&mut self, mut pos: Pos, c: char) {
        let grid = self.grid_mut();
        if pos.col > 0 && grid.drawing_cell(pos).unwrap().is_wide_continuation() {
            pos.col -= 1;
        }
        // The second half of a wide character whose first half is on
        // another row (a one-column screen) has no character to give the
        // mark to, and must never hold text.
        let cell = grid.drawing_cell_mut(pos).unwrap();
        if !cell.is_wide_continuation() {
            cell.append(c);
        }
    }

    /// Designates `charset` (the final byte of `ESC (` or `ESC )`) into G0
    /// or G1. Returns false for a set this emulator does not know.
    pub(crate) fn designate_charset(&mut self, slot: usize, charset: u8) -> bool {
        self.charsets.designated[slot] = match charset {
            b'0' => Charset::DecGraphics,
            b'A' => Charset::Uk,
            b'B' => Charset::Ascii,
            _ => return false,
        };
        true
    }

    // SO and SI
    pub(crate) fn shift_out(&mut self, out: bool) {
        self.charsets.shifted_out = out;
    }

    // CSI b
    pub(crate) fn rep(&mut self, count: u16) {
        let Some(c) = self.last_char else {
            return;
        };
        let (rows, cols) = self.size();
        // Anything beyond a screenful only scrolls the same line past.
        let count = usize::from(count).min(usize::from(rows) * usize::from(cols));
        for _ in 0..count {
            self.put_char(c);
        }
    }

    // control codes

    pub(crate) fn bs(&mut self) {
        self.grid_mut().clear_pending_wrap();
        self.grid_mut().col_dec(1);
    }

    // LF, VT, FF and IND. A line feed keeps a pending wrap, as in tmux and
    // alacritty: the next character still goes to the start of the
    // following row.
    pub(crate) fn lf(&mut self) {
        self.grid_mut().row_inc_scroll(1);
    }

    // ESC E
    pub(crate) fn nel(&mut self) {
        self.cr();
        self.lf();
    }

    // ESC H
    pub(crate) fn hts(&mut self) {
        let col = self.grid().pos().col.min(self.grid().size().cols - 1);
        if let Some(stop) = self.tab_stops.get_mut(usize::from(col)) {
            *stop = true;
        }
    }

    // CSI g
    pub(crate) fn tbc(&mut self, mode: u16) {
        match mode {
            0 => {
                let col = self.grid().pos().col;
                if let Some(stop) = self.tab_stops.get_mut(usize::from(col)) {
                    *stop = false;
                }
            }
            3 => self.tab_stops.fill(false),
            _ => {}
        }
    }

    // HT and CSI I
    pub(crate) fn cht(&mut self, count: u16) {
        let cols = self.grid().size().cols;
        // At the right margin a tab has nowhere to go, and it leaves a
        // pending wrap pending (tmux does the same).
        if self.grid().pos().col + 1 >= cols {
            return;
        }
        let mut col = self.grid().pos().col;
        for _ in 0..count {
            col = (col + 1..cols)
                .find(|col| self.tab_stops[usize::from(*col)])
                .unwrap_or(cols - 1);
        }
        self.grid_mut().col_set(col);
    }

    // CSI Z
    pub(crate) fn cbt(&mut self, count: u16) {
        let cols = self.grid().size().cols;
        let mut col = self.grid().pos().col.min(cols - 1);
        for _ in 0..count {
            col = (0..col)
                .rev()
                .find(|col| self.tab_stops[usize::from(*col)])
                .unwrap_or(0);
        }
        self.grid_mut().col_set(col);
    }

    pub(crate) fn cr(&mut self) {
        self.grid_mut().col_set(0);
    }

    // escape codes

    // ESC 7 and CSI s
    pub(crate) fn decsc(&mut self) {
        self.grid_mut().save_cursor();
        let slot = self.buffer();
        self.saved_attrs[slot] = self.attrs;
        self.saved_charsets[slot] = self.charsets;
    }

    // ESC 8 and CSI u
    pub(crate) fn decrc(&mut self) {
        self.grid_mut().restore_cursor();
        let slot = self.buffer();
        self.attrs = self.saved_attrs[slot];
        self.charsets = self.saved_charsets[slot];
        self.sync_fill();
    }

    // ESC = and ESC >
    pub(crate) fn set_application_keypad(&mut self, on: bool) {
        if on {
            self.set_mode(MODE_APPLICATION_KEYPAD);
        } else {
            self.clear_mode(MODE_APPLICATION_KEYPAD);
        }
    }

    // ESC M
    pub(crate) fn ri(&mut self) {
        self.grid_mut().row_dec_scroll(1);
    }

    // ESC c
    pub(crate) fn ris(&mut self) {
        // A full reset clears the screen and every mode, but, like xterm,
        // keeps the lines already scrolled off: `reset` in a shell must not
        // throw the pane's history away.
        let size = self.grid.size();
        let mut fresh = Self::new(size, 0);
        std::mem::swap(&mut fresh.grid, &mut self.grid);
        fresh.grid.clear();
        *self = fresh;
        self.sync_fill();
    }

    // CSI ! p
    pub(crate) fn decstr(&mut self) {
        self.clear_mode(
            MODE_INSERT | MODE_HIDE_CURSOR | MODE_APPLICATION_CURSOR | MODE_APPLICATION_KEYPAD,
        );
        self.grid_mut().soft_reset();
        self.attrs = crate::attrs::Attrs::default();
        self.charsets = Charsets::default();
        let slot = self.buffer();
        self.saved_charsets[slot] = Charsets::default();
        self.saved_attrs[slot] = crate::attrs::Attrs::default();
        self.sync_fill();
    }

    // csi codes

    // CSI @
    pub(crate) fn ich(&mut self, count: u16) {
        self.grid_mut().insert_cells(count);
    }

    // CSI A
    pub(crate) fn cuu(&mut self, offset: u16) {
        self.grid_mut().clear_pending_wrap();
        self.grid_mut().row_dec_clamp(offset);
    }

    // CSI B
    pub(crate) fn cud(&mut self, offset: u16) {
        self.grid_mut().clear_pending_wrap();
        self.grid_mut().row_inc_clamp(offset);
    }

    // CSI C
    pub(crate) fn cuf(&mut self, offset: u16) {
        self.grid_mut().clear_pending_wrap();
        self.grid_mut().col_inc_clamp(offset);
    }

    // CSI D
    pub(crate) fn cub(&mut self, offset: u16) {
        self.grid_mut().clear_pending_wrap();
        self.grid_mut().col_dec(offset);
    }

    // CSI E
    pub(crate) fn cnl(&mut self, offset: u16) {
        self.grid_mut().col_set(0);
        self.grid_mut().row_inc_clamp(offset);
    }

    // CSI F
    pub(crate) fn cpl(&mut self, offset: u16) {
        self.grid_mut().col_set(0);
        self.grid_mut().row_dec_clamp(offset);
    }

    // CSI G
    pub(crate) fn cha(&mut self, col: u16) {
        self.grid_mut().col_set(col - 1);
    }

    // CSI H
    pub(crate) fn cup(&mut self, (row, col): (u16, u16)) {
        self.grid_mut().set_pos(Pos {
            row: row - 1,
            col: col - 1,
        });
    }

    // CSI J and CSI ? J
    pub(crate) fn ed(&mut self, mode: u16, mut unhandled: impl FnMut(&mut Self)) {
        let attrs = self.erase_attrs();
        match mode {
            0 => self.grid_mut().erase_all_forward(attrs),
            1 => self.grid_mut().erase_all_backward(attrs),
            2 => self.grid_mut().erase_all(attrs),
            3 => self.grid_mut().erase_history(),
            _ => unhandled(self),
        }
    }

    // CSI K and CSI ? K
    pub(crate) fn el(&mut self, mode: u16, mut unhandled: impl FnMut(&mut Self)) {
        let attrs = self.erase_attrs();
        match mode {
            0 => self.grid_mut().erase_row_forward(attrs),
            1 => self.grid_mut().erase_row_backward(attrs),
            2 => self.grid_mut().erase_row(attrs),
            _ => unhandled(self),
        }
    }

    // CSI L
    pub(crate) fn il(&mut self, count: u16) {
        self.grid_mut().insert_lines(count);
    }

    // CSI M
    pub(crate) fn dl(&mut self, count: u16) {
        self.grid_mut().delete_lines(count);
    }

    // CSI P
    pub(crate) fn dch(&mut self, count: u16) {
        self.grid_mut().delete_cells(count);
    }

    // CSI S
    pub(crate) fn su(&mut self, count: u16) {
        self.grid_mut().scroll_up(count);
    }

    // CSI T
    pub(crate) fn sd(&mut self, count: u16) {
        self.grid_mut().scroll_down(count);
    }

    // CSI X
    pub(crate) fn ech(&mut self, count: u16) {
        let attrs = self.erase_attrs();
        self.grid_mut().erase_cells(count, attrs);
    }

    // CSI d
    pub(crate) fn vpa(&mut self, row: u16) {
        self.grid_mut().clear_pending_wrap();
        self.grid_mut().row_set(row - 1);
    }

    // CSI h / CSI l, the ANSI (non-private) modes.
    pub(crate) fn sm(
        &mut self,
        params: &vte::Params,
        set: bool,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        for param in params {
            match param {
                [4] if set => self.set_mode(MODE_INSERT),
                [4] => self.clear_mode(MODE_INSERT),
                _ => unhandled(self),
            }
        }
    }

    // CSI ? h
    pub(crate) fn decset(&mut self, params: &vte::Params, mut unhandled: impl FnMut(&mut Self)) {
        for param in params {
            match param {
                [1] => self.set_mode(MODE_APPLICATION_CURSOR),
                [6] => self.grid_mut().set_origin_mode(true),
                [7] => self.clear_mode(MODE_NO_AUTOWRAP),
                [9] => self.mouse_protocol_mode = MouseProtocolMode::Press,
                [25] => self.clear_mode(MODE_HIDE_CURSOR),
                [47 | 1047] => self.enter_alternate_grid(),
                [1000] => {
                    self.mouse_protocol_mode = MouseProtocolMode::PressRelease;
                }
                [1002] => {
                    self.mouse_protocol_mode = MouseProtocolMode::ButtonMotion;
                }
                [1003] => {
                    self.mouse_protocol_mode = MouseProtocolMode::AnyMotion;
                }
                [1004] => self.set_mode(MODE_FOCUS_REPORTING),
                [1005] => {
                    self.mouse_protocol_encoding = MouseProtocolEncoding::Utf8;
                }
                [1006] => {
                    self.mouse_protocol_encoding = MouseProtocolEncoding::Sgr;
                }
                [1048] => self.decsc(),
                [1049] => {
                    if !self.mode(MODE_ALTERNATE_SCREEN) {
                        self.decsc();
                        self.alternate_grid.clear_keeping_saved_cursor();
                        self.enter_alternate_grid();
                        // Cleared like ED 2, with the current background.
                        let fill = self.erase_attrs();
                        self.grid_mut().erase_all(fill);
                    }
                }
                [2004] => self.set_mode(MODE_BRACKETED_PASTE),
                _ => unhandled(self),
            }
        }
    }

    // CSI ? l
    pub(crate) fn decrst(&mut self, params: &vte::Params, mut unhandled: impl FnMut(&mut Self)) {
        for param in params {
            match param {
                [1] => self.clear_mode(MODE_APPLICATION_CURSOR),
                [6] => self.grid_mut().set_origin_mode(false),
                [7] => self.set_mode(MODE_NO_AUTOWRAP),
                [9] => self.clear_mouse_mode(MouseProtocolMode::Press),
                [25] => self.set_mode(MODE_HIDE_CURSOR),
                [47] => self.exit_alternate_grid(),
                [1000] => {
                    self.clear_mouse_mode(MouseProtocolMode::PressRelease);
                }
                [1002] => {
                    self.clear_mouse_mode(MouseProtocolMode::ButtonMotion);
                }
                [1003] => {
                    self.clear_mouse_mode(MouseProtocolMode::AnyMotion);
                }
                [1004] => self.clear_mode(MODE_FOCUS_REPORTING),
                [1005] => {
                    self.clear_mouse_encoding(MouseProtocolEncoding::Utf8);
                }
                [1006] => {
                    self.clear_mouse_encoding(MouseProtocolEncoding::Sgr);
                }
                [1047] => {
                    if self.mode(MODE_ALTERNATE_SCREEN) {
                        self.alternate_grid.clear();
                    }
                    self.exit_alternate_grid();
                }
                [1048] => self.decrc(),
                [1049] => {
                    if self.mode(MODE_ALTERNATE_SCREEN) {
                        self.exit_alternate_grid();
                        self.decrc();
                    }
                }
                [2004] => self.clear_mode(MODE_BRACKETED_PASTE),
                _ => unhandled(self),
            }
        }
    }

    // CSI m
    pub(crate) fn sgr(&mut self, params: &vte::Params, unhandled: impl FnMut(&mut Self)) {
        self.apply_sgr(params, unhandled);
        self.sync_fill();
    }

    fn apply_sgr(&mut self, params: &vte::Params, mut unhandled: impl FnMut(&mut Self)) {
        // vte gives no parameters at all for a bare `CSI m`
        if params.is_empty() {
            self.attrs = crate::attrs::Attrs::default();
            return;
        }

        let mut iter = params.iter();
        while let Some(param) = iter.next() {
            match param {
                [0] => self.attrs = crate::attrs::Attrs::default(),
                [1] => self.attrs.set_bold(),
                [2] => self.attrs.set_dim(),
                [3] => self.attrs.set_italic(true),
                [4] => self.attrs.set_underline(true),
                [4, style] => match crate::attrs::UnderlineStyle::from_sgr(*style) {
                    Some(style) => self.attrs.set_underline_style(style),
                    None => unhandled(self),
                },
                [5 | 6] => self.attrs.set_blink(true),
                [7] => self.attrs.set_inverse(true),
                [8] => self.attrs.set_hidden(true),
                [9] => self.attrs.set_strikethrough(true),
                [21] => self
                    .attrs
                    .set_underline_style(crate::attrs::UnderlineStyle::Double),
                [22] => self.attrs.set_normal_intensity(),
                [23] => self.attrs.set_italic(false),
                [24] => self.attrs.set_underline(false),
                [25] => self.attrs.set_blink(false),
                [27] => self.attrs.set_inverse(false),
                [28] => self.attrs.set_hidden(false),
                [29] => self.attrs.set_strikethrough(false),
                [53] => self.attrs.set_overline(true),
                [55] => self.attrs.set_overline(false),
                [n @ 30..=37] => self.attrs.fgcolor = indexed(*n - 30),
                [39] => self.attrs.fgcolor = crate::Color::Default,
                [n @ 40..=47] => self.attrs.bgcolor = indexed(*n - 40),
                [49] => self.attrs.bgcolor = crate::Color::Default,
                [59] => self.attrs.underline_color = crate::Color::Default,
                [n @ 90..=97] => self.attrs.fgcolor = indexed(*n - 82),
                [n @ 100..=107] => self.attrs.bgcolor = indexed(*n - 92),
                [selector @ (38 | 48 | 58), rest @ ..] => {
                    let color = match extended_color(rest, &mut iter) {
                        ExtendedColor::Color(color) => color,
                        ExtendedColor::Unhandled => {
                            unhandled(self);
                            continue;
                        }
                        ExtendedColor::Abort => return,
                        ExtendedColor::UnhandledAbort => {
                            unhandled(self);
                            return;
                        }
                    };
                    *match selector {
                        38 => &mut self.attrs.fgcolor,
                        48 => &mut self.attrs.bgcolor,
                        _ => &mut self.attrs.underline_color,
                    } = color;
                }
                _ => unhandled(self),
            }
        }
    }

    // CSI r
    pub(crate) fn decstbm(&mut self, (top, bottom): (u16, u16)) {
        self.grid_mut().set_scroll_region(top - 1, bottom - 1);
    }
}

/// An indexed colour whose index the caller has already bounded.
fn indexed(index: u16) -> crate::Color {
    crate::Color::Idx(u8::try_from(index).unwrap())
}

/// Parses SGR 38, 48 or 58: `rest` holds what follows the selector in
/// colon form (`38:2::r:g:b`, `38:5:i`); when it is empty, the semicolon
/// form takes its parameters from `iter` instead.
fn extended_color<'a>(rest: &[u16], iter: &mut impl Iterator<Item = &'a [u16]>) -> ExtendedColor {
    let byte = |n: u16| u8::try_from(n).ok();
    let rgb = |r: Option<u8>, g: Option<u8>, b: Option<u8>| Some(crate::Color::Rgb(r?, g?, b?));
    let color = match rest {
        [2, r, g, b] | [2, _, r, g, b] => rgb(byte(*r), byte(*g), byte(*b)),
        [5, i] => byte(*i).map(crate::Color::Idx),
        [] => {
            let kind = iter.next();
            let mut next = || match iter.next() {
                Some(&[n]) => byte(n),
                _ => None,
            };
            match kind {
                Some([2]) => rgb(next(), next(), next()),
                Some([5]) => next().map(crate::Color::Idx),
                Some(_) => return ExtendedColor::UnhandledAbort,
                None => None,
            }
        }
        _ => return ExtendedColor::Unhandled,
    };
    color.map_or(ExtendedColor::Abort, ExtendedColor::Color)
}
