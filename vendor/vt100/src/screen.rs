use crate::term::BufWrite as _;
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
        match self {
            Self::Ascii => c,
            Self::Uk => {
                if c == '#' {
                    '£'
                } else {
                    c
                }
            }
            Self::DecGraphics => match c {
                '_' => ' ',
                '`' => '◆',
                'a' => '▒',
                'b' => '␉',
                'c' => '␌',
                'd' => '␍',
                'e' => '␊',
                'f' => '°',
                'g' => '±',
                'h' => '␤',
                'i' => '␋',
                'j' => '┘',
                'k' => '┐',
                'l' => '┌',
                'm' => '└',
                'n' => '┼',
                'o' => '⎺',
                'p' => '⎻',
                'q' => '─',
                'r' => '⎼',
                's' => '⎽',
                't' => '├',
                'u' => '┤',
                'v' => '┴',
                'w' => '┬',
                'x' => '│',
                'y' => '≤',
                'z' => '≥',
                '{' => 'π',
                '|' => '≠',
                '}' => '£',
                '~' => '·',
                _ => c,
            },
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
    fn translate(&self, c: char) -> char {
        if c.is_ascii() {
            self.designated[usize::from(self.shifted_out)].translate(c)
        } else {
            c
        }
    }
}

fn default_tab_stops(cols: u16) -> Vec<bool> {
    (0..cols).map(|col| col > 0 && col % 8 == 0).collect()
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

    // Highlight,
    /// Mouse button events should be reported on button press and release, as
    /// well as when the mouse moves between cells while a button is held
    /// down.
    ButtonMotion,

    /// Mouse button events should be reported on button press and release,
    /// and mouse motion events should be reported when the mouse moves
    /// between cells regardless of whether a button is held down or not.
    AnyMotion,
    // DecLocator,
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
    // Urxvt,
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
    last_drawn: Option<crate::grid::Pos>,

    modes: u8,
    mouse_protocol_mode: MouseProtocolMode,
    mouse_protocol_encoding: MouseProtocolEncoding,
}

impl Screen {
    pub(crate) fn new(
        size: crate::grid::Size,
        scrollback_len: usize,
    ) -> Self {
        let mut grid = crate::grid::Grid::new(size, scrollback_len);
        grid.allocate_rows();
        grid.set_reflow(true);
        Self {
            grid,
            alternate_grid: crate::grid::Grid::new(size, 0),

            attrs: crate::attrs::Attrs::default(),
            saved_attrs: [crate::attrs::Attrs::default(); 2],

            charsets: Charsets::default(),
            saved_charsets: [Charsets::default(); 2],
            tab_stops: default_tab_stops(size.cols),
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
        self.tab_stops.resize(usize::from(cols), false);
        for col in old_cols..usize::from(cols) {
            self.tab_stops[col] = col > 0 && col % 8 == 0;
        }
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
        self.write_contents(&mut contents);
        contents
    }

    fn write_contents(&self, contents: &mut String) {
        self.grid().write_contents(contents);
    }

    /// Returns the text contents of the terminal by row, restricted to the
    /// given subset of columns.
    ///
    /// This will not include any formatting information, and will be in plain
    /// text format.
    ///
    /// Newlines will not be included.
    pub fn rows(
        &self,
        start: u16,
        width: u16,
    ) -> impl Iterator<Item = String> + '_ {
        self.grid().visible_rows().map(move |row| {
            let mut contents = String::new();
            row.write_contents(&mut contents, start, width, false);
            contents
        })
    }

    /// Returns the text contents of the terminal logically between two cells.
    /// This will include the remainder of the starting row after `start_col`,
    /// followed by the entire contents of the rows between `start_row` and
    /// `end_row`, followed by the beginning of the `end_row` up until
    /// `end_col`. This is useful for things like determining the contents of
    /// a clipboard selection.
    #[must_use]
    pub fn contents_between(
        &self,
        start_row: u16,
        start_col: u16,
        end_row: u16,
        end_col: u16,
    ) -> String {
        match start_row.cmp(&end_row) {
            std::cmp::Ordering::Less => {
                let (_, cols) = self.size();
                let mut contents = String::new();
                for (i, row) in self
                    .grid()
                    .visible_rows()
                    .enumerate()
                    .skip(usize::from(start_row))
                    .take(usize::from(end_row) - usize::from(start_row) + 1)
                {
                    if i == usize::from(start_row) {
                        row.write_contents(
                            &mut contents,
                            start_col,
                            cols - start_col,
                            false,
                        );
                        if !row.wrapped() {
                            contents.push('\n');
                        }
                    } else if i == usize::from(end_row) {
                        row.write_contents(&mut contents, 0, end_col, false);
                    } else {
                        row.write_contents(&mut contents, 0, cols, false);
                        if !row.wrapped() {
                            contents.push('\n');
                        }
                    }
                }
                contents
            }
            std::cmp::Ordering::Equal => {
                if start_col < end_col {
                    self.rows(start_col, end_col - start_col)
                        .nth(usize::from(start_row))
                        .unwrap_or_default()
                } else {
                    String::new()
                }
            }
            std::cmp::Ordering::Greater => String::new(),
        }
    }

    /// Return escape codes sufficient to reproduce the entire contents of the
    /// current terminal state. This is a convenience wrapper around
    /// [`contents_formatted`](Self::contents_formatted) and
    /// [`input_mode_formatted`](Self::input_mode_formatted).
    #[must_use]
    pub fn state_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_contents_formatted(&mut contents);
        self.write_input_mode_formatted(&mut contents);
        contents
    }

    /// Return escape codes sufficient to turn the terminal state of the
    /// screen `prev` into the current terminal state. This is a convenience
    /// wrapper around [`contents_diff`](Self::contents_diff) and
    /// [`input_mode_diff`](Self::input_mode_diff).
    #[must_use]
    pub fn state_diff(&self, prev: &Self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_contents_diff(&mut contents, prev);
        self.write_input_mode_diff(&mut contents, prev);
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
        self.write_contents_formatted(&mut contents);
        contents
    }

    fn write_contents_formatted(&self, contents: &mut Vec<u8>) {
        crate::term::HideCursor::new(self.hide_cursor()).write_buf(contents);
        let prev_attrs = self.grid().write_contents_formatted(contents);
        self.attrs.write_escape_code_diff(contents, &prev_attrs);
    }

    /// Returns the formatted visible contents of the terminal by row,
    /// restricted to the given subset of columns.
    ///
    /// Formatting information will be included inline as terminal escape
    /// codes. The result will be suitable for feeding directly to a raw
    /// terminal parser, and will result in the same visual output.
    ///
    /// You are responsible for positioning the cursor before printing each
    /// row, and the final cursor position after displaying each row is
    /// unspecified.
    // the unwraps in this method shouldn't be reachable
    #[allow(clippy::missing_panics_doc)]
    pub fn rows_formatted(
        &self,
        start: u16,
        width: u16,
    ) -> impl Iterator<Item = Vec<u8>> + '_ {
        let mut wrapping = false;
        self.grid().visible_rows().enumerate().map(move |(i, row)| {
            // number of rows in a grid is stored in a u16 (see Size), so
            // visible_rows can never return enough rows to overflow here
            let i = i.try_into().unwrap();
            let mut contents = vec![];
            row.write_contents_formatted(
                &mut contents,
                start,
                width,
                i,
                wrapping,
                None,
                None,
            );
            if start == 0 && width == self.grid.size().cols {
                wrapping = row.wrapped();
            }
            contents
        })
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
        self.write_contents_diff(&mut contents, prev);
        contents
    }

    fn write_contents_diff(&self, contents: &mut Vec<u8>, prev: &Self) {
        if self.hide_cursor() != prev.hide_cursor() {
            crate::term::HideCursor::new(self.hide_cursor())
                .write_buf(contents);
        }
        let prev_attrs = self.grid().write_contents_diff(
            contents,
            prev.grid(),
            prev.attrs,
        );
        self.attrs.write_escape_code_diff(contents, &prev_attrs);
    }

    /// Returns a sequence of terminal byte streams sufficient to turn the
    /// visible contents of the subset of each row from `prev` (as described
    /// by `start` and `width`) into the visible contents of the corresponding
    /// row subset in `self`.
    ///
    /// You are responsible for positioning the cursor before printing each
    /// row, and the final cursor position after displaying each row is
    /// unspecified.
    // the unwraps in this method shouldn't be reachable
    #[allow(clippy::missing_panics_doc)]
    pub fn rows_diff<'a>(
        &'a self,
        prev: &'a Self,
        start: u16,
        width: u16,
    ) -> impl Iterator<Item = Vec<u8>> + 'a {
        self.grid()
            .visible_rows()
            .zip(prev.grid().visible_rows())
            .enumerate()
            .map(move |(i, (row, prev_row))| {
                // number of rows in a grid is stored in a u16 (see Size), so
                // visible_rows can never return enough rows to overflow here
                let i = i.try_into().unwrap();
                let mut contents = vec![];
                row.write_contents_diff(
                    &mut contents,
                    prev_row,
                    start,
                    width,
                    i,
                    false,
                    false,
                    crate::grid::Pos { row: i, col: start },
                    crate::attrs::Attrs::default(),
                );
                contents
            })
    }

    /// Returns terminal escape sequences sufficient to set the current
    /// terminal's input modes.
    ///
    /// Supported modes are:
    /// * application keypad
    /// * application cursor
    /// * bracketed paste
    /// * xterm mouse support
    #[must_use]
    pub fn input_mode_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_input_mode_formatted(&mut contents);
        contents
    }

    fn write_input_mode_formatted(&self, contents: &mut Vec<u8>) {
        crate::term::ApplicationKeypad::new(
            self.mode(MODE_APPLICATION_KEYPAD),
        )
        .write_buf(contents);
        crate::term::ApplicationCursor::new(
            self.mode(MODE_APPLICATION_CURSOR),
        )
        .write_buf(contents);
        crate::term::BracketedPaste::new(self.mode(MODE_BRACKETED_PASTE))
            .write_buf(contents);
        crate::term::MouseProtocolMode::new(
            self.mouse_protocol_mode,
            MouseProtocolMode::None,
        )
        .write_buf(contents);
        crate::term::MouseProtocolEncoding::new(
            self.mouse_protocol_encoding,
            MouseProtocolEncoding::Default,
        )
        .write_buf(contents);
    }

    /// Returns terminal escape sequences sufficient to change the previous
    /// terminal's input modes to the input modes enabled in the current
    /// terminal.
    #[must_use]
    pub fn input_mode_diff(&self, prev: &Self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_input_mode_diff(&mut contents, prev);
        contents
    }

    fn write_input_mode_diff(&self, contents: &mut Vec<u8>, prev: &Self) {
        if self.mode(MODE_APPLICATION_KEYPAD)
            != prev.mode(MODE_APPLICATION_KEYPAD)
        {
            crate::term::ApplicationKeypad::new(
                self.mode(MODE_APPLICATION_KEYPAD),
            )
            .write_buf(contents);
        }
        if self.mode(MODE_APPLICATION_CURSOR)
            != prev.mode(MODE_APPLICATION_CURSOR)
        {
            crate::term::ApplicationCursor::new(
                self.mode(MODE_APPLICATION_CURSOR),
            )
            .write_buf(contents);
        }
        if self.mode(MODE_BRACKETED_PASTE) != prev.mode(MODE_BRACKETED_PASTE)
        {
            crate::term::BracketedPaste::new(self.mode(MODE_BRACKETED_PASTE))
                .write_buf(contents);
        }
        crate::term::MouseProtocolMode::new(
            self.mouse_protocol_mode,
            prev.mouse_protocol_mode,
        )
        .write_buf(contents);
        crate::term::MouseProtocolEncoding::new(
            self.mouse_protocol_encoding,
            prev.mouse_protocol_encoding,
        )
        .write_buf(contents);
    }

    /// Returns terminal escape sequences sufficient to set the current
    /// terminal's drawing attributes.
    ///
    /// Supported drawing attributes are:
    /// * fgcolor
    /// * bgcolor
    /// * bold
    /// * dim
    /// * italic
    /// * underline
    /// * inverse
    ///
    /// This is not typically necessary, since
    /// [`contents_formatted`](Self::contents_formatted) will leave
    /// the current active drawing attributes in the correct state, but this
    /// can be useful in the case of drawing additional things on top of a
    /// terminal output, since you will need to restore the terminal state
    /// without the terminal contents necessarily being the same.
    #[must_use]
    pub fn attributes_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_attributes_formatted(&mut contents);
        contents
    }

    fn write_attributes_formatted(&self, contents: &mut Vec<u8>) {
        crate::term::ClearAttrs.write_buf(contents);
        self.attrs.write_escape_code_diff(
            contents,
            &crate::attrs::Attrs::default(),
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

    /// Returns terminal escape sequences sufficient to set the current
    /// cursor state of the terminal.
    ///
    /// This is not typically necessary, since
    /// [`contents_formatted`](Self::contents_formatted) will leave
    /// the cursor in the correct state, but this can be useful in the case of
    /// drawing additional things on top of a terminal output, since you will
    /// need to restore the terminal state without the terminal contents
    /// necessarily being the same.
    ///
    /// Note that the bytes returned by this function may alter the active
    /// drawing attributes, because it may require redrawing existing cells in
    /// order to position the cursor correctly (for instance, in the case
    /// where the cursor is past the end of a row). Therefore, you should
    /// ensure to reset the active drawing attributes if necessary after
    /// processing this data, for instance by using
    /// [`attributes_formatted`](Self::attributes_formatted).
    #[must_use]
    pub fn cursor_state_formatted(&self) -> Vec<u8> {
        let mut contents = vec![];
        self.write_cursor_state_formatted(&mut contents);
        contents
    }

    fn write_cursor_state_formatted(&self, contents: &mut Vec<u8>) {
        crate::term::HideCursor::new(self.hide_cursor()).write_buf(contents);
        self.grid()
            .write_cursor_position_formatted(contents, None, None);

        // we don't just call write_attributes_formatted here, because that
        // would still be confusing - consider the case where the user sets
        // their own unrelated drawing attributes (on a different parser
        // instance) and then calls cursor_state_formatted. just documenting
        // it and letting the user handle it on their own is more
        // straightforward.
    }

    /// Returns the [`Cell`](crate::Cell) object at the given location in the
    /// terminal, if it exists.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<crate::Cell> {
        self.grid().visible_cell(crate::grid::Pos { row, col })
    }

    /// Returns the cells in one visible row.
    pub fn row_cells(
        &self,
        row: u16,
    ) -> impl Iterator<Item = crate::Cell> + '_ {
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

    /// Returns whether the terminal should be in application keypad mode.
    #[must_use]
    pub fn application_keypad(&self) -> bool {
        self.mode(MODE_APPLICATION_KEYPAD)
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

    /// Returns the currently active foreground color.
    #[must_use]
    pub fn fgcolor(&self) -> crate::Color {
        self.attrs.fgcolor
    }

    /// Returns the currently active background color.
    #[must_use]
    pub fn bgcolor(&self) -> crate::Color {
        self.attrs.bgcolor
    }

    /// Returns whether newly drawn text should be rendered with the bold text
    /// attribute.
    #[must_use]
    pub fn bold(&self) -> bool {
        self.attrs.bold()
    }

    /// Returns whether newly drawn text should be rendered with the dim text
    /// attribute.
    #[must_use]
    pub fn dim(&self) -> bool {
        self.attrs.dim()
    }

    /// Returns whether newly drawn text should be rendered with the italic
    /// text attribute.
    #[must_use]
    pub fn italic(&self) -> bool {
        self.attrs.italic()
    }

    /// Returns whether newly drawn text should be rendered with the
    /// underlined text attribute.
    #[must_use]
    pub fn underline(&self) -> bool {
        self.attrs.underline()
    }

    /// Returns whether newly drawn text should be rendered with the inverse
    /// text attribute.
    #[must_use]
    pub fn inverse(&self) -> bool {
        self.attrs.inverse()
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

    fn save_cursor(&mut self) {
        self.grid_mut().save_cursor();
        let slot = self.buffer();
        self.saved_attrs[slot] = self.attrs;
        self.saved_charsets[slot] = self.charsets;
    }

    fn restore_cursor(&mut self) {
        self.grid_mut().restore_cursor();
        let slot = self.buffer();
        self.attrs = self.saved_attrs[slot];
        self.charsets = self.saved_charsets[slot];
        self.sync_fill();
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

    fn set_mouse_mode(&mut self, mode: MouseProtocolMode) {
        self.mouse_protocol_mode = mode;
    }

    fn clear_mouse_mode(&mut self, mode: MouseProtocolMode) {
        if self.mouse_protocol_mode == mode {
            self.mouse_protocol_mode = MouseProtocolMode::default();
        }
    }

    fn set_mouse_encoding(&mut self, encoding: MouseProtocolEncoding) {
        self.mouse_protocol_encoding = encoding;
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

    fn put_char(&mut self, c: char) {
        let pos = self.grid().pos();
        let size = self.grid().size();
        let attrs = self.attrs;

        let width = c.width();
        if width.is_none() && (u32::from(c)) < 256 {
            // don't even try to draw control characters
            return;
        }
        let width: u16 = width
            .unwrap_or(1)
            .try_into()
            // width() can only return 0, 1, or 2
            .unwrap();
        if width > size.cols {
            // A grid narrower than the character has no pair of columns to
            // hold it. Everything below assumes the second half of a wide
            // character has somewhere to go, so there is nothing to draw.
            return;
        }

        // it doesn't make any sense to wrap if the last column in a row
        // didn't already have contents. don't try to handle the case where a
        // character wraps because there was only one column left in the
        // previous row - literally everything handles this case differently,
        // and this is tmux behavior (and also the simplest). i'm open to
        // reconsidering this behavior, but only with a really good reason
        // (xterm handles this by introducing the concept of triple width
        // cells, which i really don't want to do).
        let mut wrap = false;
        if width > 0
            && self.mode(MODE_NO_AUTOWRAP)
            && pos.col > size.cols.saturating_sub(width)
        {
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
        // A grid narrower than the character being drawn leaves no column for
        // it to have come from, so there is nothing to wrap out of.
        if pos.col > size.cols.saturating_sub(width) {
            let last_cell = self.grid().drawing_cell(crate::grid::Pos {
                row: pos.row,
                col: size.cols.saturating_sub(1),
            });
            // A wide character that does not fit in the last column continues
            // the same line on the next row, so that row is wrapped too.
            if pos.col < size.cols
                || last_cell.is_some_and(|cell| {
                    cell.has_contents() || cell.is_wide_continuation()
                })
            {
                wrap = true;
            }
        }
        if width > 1 && pos.col < size.cols && pos.col > size.cols - width {
            // A wide character that does not fit wraps whole, and the column
            // it left behind is blanked, as if a space had been written there,
            // rather than keeping a stale glyph.
            let fill = attrs;
            for col in pos.col..size.cols {
                let cell = crate::grid::Pos { row: pos.row, col };
                if let Some(cell) = self.grid_mut().drawing_cell_mut(cell) {
                    if cell.is_wide_continuation() {
                        cell.clear(fill);
                        if let Some(base) = col.checked_sub(1).and_then(|col| {
                            self.grid_mut().drawing_cell_mut(crate::grid::Pos {
                                row: pos.row,
                                col,
                            })
                        }) {
                            base.clear(fill);
                        }
                    } else {
                        cell.clear(fill);
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
                let base = if self
                    .grid()
                    .drawing_cell(pos)
                    .is_some_and(|cell| cell.is_wide_continuation())
                {
                    pos.col.checked_sub(1).map(|col| crate::grid::Pos {
                        row: pos.row,
                        col,
                    })
                } else {
                    Some(pos)
                };
                if let Some(cell) =
                    base.and_then(|base| self.grid_mut().drawing_cell_mut(base))
                {
                    cell.append(c);
                }
            } else if pos.col > 0 {
                let mut prev_cell = self
                    .grid_mut()
                    .drawing_cell_mut(crate::grid::Pos {
                        row: pos.row,
                        col: pos.col - 1,
                    })
                    // pos.row is valid, since it comes directly from
                    // self.grid().pos() which we assume to always have a
                    // valid row value. pos.col - 1 is valid because we just
                    // checked for pos.col > 0.
                    .unwrap();
                if prev_cell.is_wide_continuation() && pos.col >= 2 {
                    prev_cell = self
                        .grid_mut()
                        .drawing_cell_mut(crate::grid::Pos {
                            row: pos.row,
                            col: pos.col - 2,
                        })
                        // pos.row is valid, since it comes directly from
                        // self.grid().pos() which we assume to always have a
                        // valid row value. we know pos.col - 2 is valid
                        // because the cell at pos.col - 1 is a wide
                        // continuation character, which means there must be
                        // the first half of the wide character before it.
                        .unwrap();
                }
                // A combining mark belongs to a character. The second half of a
                // wide one whose first half is on another row (a one-column
                // screen) has none to give it, and must never hold text.
                if !prev_cell.is_wide_continuation() {
                    prev_cell.append(c);
                }
            } else if pos.row > 0 {
                let prev_row = self
                    .grid()
                    .drawing_row(pos.row - 1)
                    // pos.row is valid, since it comes directly from
                    // self.grid().pos() which we assume to always have a
                    // valid row value. pos.row - 1 is valid because we just
                    // checked for pos.row > 0.
                    .unwrap();
                if prev_row.wrapped() {
                    let mut prev_cell = self
                        .grid_mut()
                        .drawing_cell_mut(crate::grid::Pos {
                            row: pos.row - 1,
                            col: size.cols - 1,
                        })
                        // pos.row is valid, since it comes directly from
                        // self.grid().pos() which we assume to always have a
                        // valid row value. pos.row - 1 is valid because we
                        // just checked for pos.row > 0. col of size.cols - 1
                        // is always valid.
                        .unwrap();
                    if prev_cell.is_wide_continuation() && size.cols >= 2 {
                        prev_cell = self
                            .grid_mut()
                            .drawing_cell_mut(crate::grid::Pos {
                                row: pos.row - 1,
                                col: size.cols - 2,
                            })
                            // pos.row is valid, since it comes directly from
                            // self.grid().pos() which we assume to always
                            // have a valid row value. pos.row - 1 is valid
                            // because we just checked for pos.row > 0. col of
                            // size.cols - 2 is valid because the cell at
                            // size.cols - 1 is a wide continuation character,
                            // so it must have the first half of the wide
                            // character before it.
                            .unwrap();
                    }
                    // A combining mark belongs to a character. The second half of a
                    // wide one whose first half is on another row (a one-column
                    // screen) has none to give it, and must never hold text.
                    if !prev_cell.is_wide_continuation() {
                        prev_cell.append(c);
                    }
                }
            }
        } else {
            if self.mode(MODE_INSERT) {
                self.grid_mut().insert_cells(width);
            }
            if self
                .grid()
                .drawing_cell(pos)
                // pos.row is valid because we assume self.grid().pos() to
                // always have a valid row value. pos.col is valid because we
                // called col_wrap() immediately before this, which ensures
                // that self.grid().pos().col has a valid value.
                .unwrap()
                .is_wide_continuation()
            {
                // The orphaned first half becomes a blank in its own colours.
                // A continuation with nothing before it should not exist, but
                // must never be a reason to panic the terminal.
                if let Some(prev_cell) =
                    pos.col.checked_sub(1).and_then(|col| {
                        self.grid_mut().drawing_cell_mut(crate::grid::Pos {
                            row: pos.row,
                            col,
                        })
                    })
                {
                    let own = *prev_cell.attrs();
                    prev_cell.clear(own);
                }
            }

            if self
                .grid()
                .drawing_cell(pos)
                // pos.row is valid because we assume self.grid().pos() to
                // always have a valid row value. pos.col is valid because we
                // called col_wrap() immediately before this, which ensures
                // that self.grid().pos().col has a valid value.
                .unwrap()
                .is_wide()
            {
                // The second half normally follows; a one-column screen keeps
                // it on the next row instead, where it is left alone.
                if let Some(next_cell) =
                    self.grid_mut().drawing_cell_mut(crate::grid::Pos {
                        row: pos.row,
                        col: pos.col + 1,
                    })
                {
                    let own = *next_cell.attrs();
                    next_cell.clear(own);
                }
            }

            let cell = self
                .grid_mut()
                .drawing_cell_mut(pos)
                // pos.row is valid because we assume self.grid().pos() to
                // always have a valid row value. pos.col is valid because we
                // called col_wrap() immediately before this, which ensures
                // that self.grid().pos().col has a valid value.
                .unwrap();
            cell.set(c, attrs);
            self.grid_mut().col_inc(1);
            if width > 1 {
                let pos = self.grid().pos();
                if self
                    .grid()
                    .drawing_cell(pos)
                    // pos.row is valid because we assume self.grid().pos() to
                    // always have a valid row value. pos.col is valid because
                    // we called col_wrap() earlier, which ensures that
                    // self.grid().pos().col has a valid value. this is true
                    // even though we just called col_inc, because this branch
                    // only happens if width > 1, and col_wrap takes width
                    // into account.
                    .unwrap()
                    .is_wide()
                {
                    let next_next_pos = crate::grid::Pos {
                        row: pos.row,
                        col: pos.col + 1,
                    };
                    if let Some(next_next_cell) =
                        self.grid_mut().drawing_cell_mut(next_next_pos)
                    {
                        next_next_cell.clear(attrs);
                    }
                    if next_next_pos.col == size.cols - 1 {
                        self.grid_mut()
                            .drawing_row_mut(pos.row)
                            // we assume self.grid().pos().row is always valid
                            .unwrap()
                            .wrap(false);
                    }
                }
                let next_cell = self
                    .grid_mut()
                    .drawing_cell_mut(pos)
                    // pos.row is valid because we assume self.grid().pos() to
                    // always have a valid row value. pos.col is valid because
                    // we called col_wrap() earlier, which ensures that
                    // self.grid().pos().col has a valid value. this is true
                    // even though we just called col_inc, because this branch
                    // only happens if width > 1, and col_wrap takes width
                    // into account.
                    .unwrap();
                // The right half carries the character's rendition, which is
                // what it shows if the left half is ever overwritten.
                next_cell.clear(attrs);
                next_cell.set_wide_continuation(true);
                self.grid_mut().col_inc(1);
            }
            if self.mode(MODE_NO_AUTOWRAP) {
                // Without autowrap there is no pending wrap: the cursor stays
                // on the last column.
                self.grid_mut().clear_pending_wrap();
            }
            self.last_drawn = Some(pos);
        }
    }

    /// Designates `charset` (the final byte of `ESC (` or `ESC )`) into G0
    /// or G1. Returns false for a set this emulator does not know.
    pub(crate) fn designate_charset(
        &mut self,
        slot: usize,
        charset: u8,
    ) -> bool {
        self.charsets.designated[slot] = match charset {
            b'0' => Charset::DecGraphics,
            b'A' => Charset::Uk,
            b'B' => Charset::Ascii,
            _ => return false,
        };
        true
    }

    // SO
    pub(crate) fn shift_out(&mut self) {
        self.charsets.shifted_out = true;
    }

    // SI
    pub(crate) fn shift_in(&mut self) {
        self.charsets.shifted_out = false;
    }

    // CSI b
    pub(crate) fn rep(&mut self, count: u16) {
        let Some(c) = self.last_char else {
            return;
        };
        let (rows, cols) = self.size();
        // Anything beyond a screenful only scrolls the same line past.
        let count =
            usize::from(count).min(usize::from(rows) * usize::from(cols));
        for _ in 0..count {
            self.put_char(c);
        }
    }

    /// Whether the program asked to be told when the terminal gains or loses
    /// focus (mode 1004).
    #[must_use]
    pub fn focus_reporting(&self) -> bool {
        self.mode(MODE_FOCUS_REPORTING)
    }

    // control codes

    pub(crate) fn bs(&mut self) {
        self.grid_mut().clear_pending_wrap();
        self.grid_mut().col_dec(1);
    }

    pub(crate) fn tab(&mut self) {
        self.cht(1);
    }

    // A line feed keeps a pending wrap, as in tmux and alacritty: the next
    // character still goes to the start of the following row.
    pub(crate) fn lf(&mut self) {
        self.grid_mut().row_inc_scroll(1);
    }

    // ESC D
    pub(crate) fn ind(&mut self) {
        self.lf();
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
            3 => self.tab_stops.iter_mut().for_each(|stop| *stop = false),
            _ => {}
        }
    }

    // CSI I
    pub(crate) fn cht(&mut self, count: u16) {
        let cols = self.grid().size().cols;
        // At the right margin a tab has nowhere to go, and it leaves a
        // pending wrap pending (tmux does the same).
        if self.grid().pos().col + 1 >= cols {
            return;
        }
        let mut col = self.grid().pos().col.min(cols - 1);
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

    pub(crate) fn vt(&mut self) {
        self.lf();
    }

    pub(crate) fn ff(&mut self) {
        self.lf();
    }

    pub(crate) fn cr(&mut self) {
        self.grid_mut().col_set(0);
    }

    // escape codes

    // ESC 7
    pub(crate) fn decsc(&mut self) {
        self.save_cursor();
    }

    // ESC 8
    pub(crate) fn decrc(&mut self) {
        self.restore_cursor();
    }

    // ESC =
    pub(crate) fn deckpam(&mut self) {
        self.set_mode(MODE_APPLICATION_KEYPAD);
    }

    // ESC >
    pub(crate) fn deckpnm(&mut self) {
        self.clear_mode(MODE_APPLICATION_KEYPAD);
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
            MODE_INSERT | MODE_HIDE_CURSOR | MODE_APPLICATION_CURSOR,
        );
        self.clear_mode(MODE_APPLICATION_KEYPAD);
        self.grid_mut().set_origin_mode_only(false);
        let rows = self.grid().size().rows;
        self.grid_mut().set_scroll_region_only(0, rows - 1);
        self.attrs = crate::attrs::Attrs::default();
        self.charsets = Charsets::default();
        let slot = self.buffer();
        self.saved_charsets[slot] = Charsets::default();
        self.saved_attrs[slot] = crate::attrs::Attrs::default();
        self.grid_mut().reset_saved_cursor();
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
        self.grid_mut().set_pos(crate::grid::Pos {
            row: row - 1,
            col: col - 1,
        });
    }

    // CSI J
    pub(crate) fn ed(
        &mut self,
        mode: u16,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        let attrs = self.erase_attrs();
        match mode {
            0 => self.grid_mut().erase_all_forward(attrs),
            1 => self.grid_mut().erase_all_backward(attrs),
            2 => self.grid_mut().erase_all(attrs),
            3 => self.grid_mut().erase_history(),
            _ => unhandled(self),
        }
    }

    // CSI ? J
    pub(crate) fn decsed(
        &mut self,
        mode: u16,
        unhandled: impl FnMut(&mut Self),
    ) {
        self.ed(mode, unhandled);
    }

    // CSI K
    pub(crate) fn el(
        &mut self,
        mode: u16,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        let attrs = self.erase_attrs();
        match mode {
            0 => self.grid_mut().erase_row_forward(attrs),
            1 => self.grid_mut().erase_row_backward(attrs),
            2 => self.grid_mut().erase_row(attrs),
            _ => unhandled(self),
        }
    }

    // CSI ? K
    pub(crate) fn decsel(
        &mut self,
        mode: u16,
        unhandled: impl FnMut(&mut Self),
    ) {
        self.el(mode, unhandled);
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

    // CSI s (SCOSC); only without parameters, which is all it means without
    // left and right margins.
    pub(crate) fn scosc(&mut self) {
        self.save_cursor();
    }

    // CSI u (SCORC)
    pub(crate) fn scorc(&mut self) {
        self.restore_cursor();
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
                [4] => {
                    if set {
                        self.set_mode(MODE_INSERT);
                    } else {
                        self.clear_mode(MODE_INSERT);
                    }
                }
                _ => unhandled(self),
            }
        }
    }

    // CSI ? h
    pub(crate) fn decset(
        &mut self,
        params: &vte::Params,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        for param in params {
            match param {
                [1] => self.set_mode(MODE_APPLICATION_CURSOR),
                [6] => self.grid_mut().set_origin_mode(true),
                [7] => self.clear_mode(MODE_NO_AUTOWRAP),
                [1004] => self.set_mode(MODE_FOCUS_REPORTING),
                [1047] => self.enter_alternate_grid(),
                [1048] => self.decsc(),
                [9] => self.set_mouse_mode(MouseProtocolMode::Press),
                [25] => self.clear_mode(MODE_HIDE_CURSOR),
                [47] => self.enter_alternate_grid(),
                [1000] => {
                    self.set_mouse_mode(MouseProtocolMode::PressRelease);
                }
                [1002] => {
                    self.set_mouse_mode(MouseProtocolMode::ButtonMotion);
                }
                [1003] => self.set_mouse_mode(MouseProtocolMode::AnyMotion),
                [1005] => {
                    self.set_mouse_encoding(MouseProtocolEncoding::Utf8);
                }
                [1006] => {
                    self.set_mouse_encoding(MouseProtocolEncoding::Sgr);
                }
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
    pub(crate) fn decrst(
        &mut self,
        params: &vte::Params,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        for param in params {
            match param {
                [1] => self.clear_mode(MODE_APPLICATION_CURSOR),
                [6] => self.grid_mut().set_origin_mode(false),
                [7] => self.set_mode(MODE_NO_AUTOWRAP),
                [1004] => self.clear_mode(MODE_FOCUS_REPORTING),
                [1047] => {
                    if self.mode(MODE_ALTERNATE_SCREEN) {
                        self.alternate_grid.clear();
                    }
                    self.exit_alternate_grid();
                }
                [1048] => self.decrc(),
                [9] => self.clear_mouse_mode(MouseProtocolMode::Press),
                [25] => self.set_mode(MODE_HIDE_CURSOR),
                [47] => {
                    self.exit_alternate_grid();
                }
                [1000] => {
                    self.clear_mouse_mode(MouseProtocolMode::PressRelease);
                }
                [1002] => {
                    self.clear_mouse_mode(MouseProtocolMode::ButtonMotion);
                }
                [1003] => {
                    self.clear_mouse_mode(MouseProtocolMode::AnyMotion);
                }
                [1005] => {
                    self.clear_mouse_encoding(MouseProtocolEncoding::Utf8);
                }
                [1006] => {
                    self.clear_mouse_encoding(MouseProtocolEncoding::Sgr);
                }
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
    pub(crate) fn sgr(
        &mut self,
        params: &vte::Params,
        unhandled: impl FnMut(&mut Self),
    ) {
        self.apply_sgr(params, unhandled);
        self.sync_fill();
    }

    fn apply_sgr(
        &mut self,
        params: &vte::Params,
        mut unhandled: impl FnMut(&mut Self),
    ) {
        // XXX really i want to just be able to pass in a default Params
        // instance with a 0 in it, but vte doesn't allow creating new Params
        // instances
        if params.is_empty() {
            self.attrs = crate::attrs::Attrs::default();
            return;
        }

        let mut iter = params.iter();

        macro_rules! next_param {
            () => {
                match iter.next() {
                    Some(n) => n,
                    _ => return,
                }
            };
        }

        macro_rules! to_u8 {
            ($n:expr) => {
                if let Some(n) = u16_to_u8($n) {
                    n
                } else {
                    return;
                }
            };
        }

        macro_rules! next_param_u8 {
            () => {
                if let &[n] = next_param!() {
                    to_u8!(n)
                } else {
                    return;
                }
            };
        }

        loop {
            match next_param!() {
                [0] => self.attrs = crate::attrs::Attrs::default(),
                [1] => self.attrs.set_bold(),
                [2] => self.attrs.set_dim(),
                [3] => self.attrs.set_italic(true),
                [4] => self.attrs.set_underline(true),
                [4, style] => {
                    if !self.attrs.set_underline_style_sgr(*style) {
                        unhandled(self);
                    }
                }
                [5] | [6] => self.attrs.set_blink(true),
                [7] => self.attrs.set_inverse(true),
                [8] => self.attrs.set_hidden(true),
                [9] => self.attrs.set_strikethrough(true),
                [21] => self.attrs.set_underline_style(
                    crate::attrs::UnderlineStyle::Double,
                ),
                [22] => self.attrs.set_normal_intensity(),
                [23] => self.attrs.set_italic(false),
                [24] => self.attrs.set_underline(false),
                [25] => self.attrs.set_blink(false),
                [27] => self.attrs.set_inverse(false),
                [28] => self.attrs.set_hidden(false),
                [29] => self.attrs.set_strikethrough(false),
                [53] => self.attrs.set_overline(true),
                [55] => self.attrs.set_overline(false),
                [n] if (30..=37).contains(n) => {
                    self.attrs.fgcolor = crate::Color::Idx(to_u8!(*n) - 30);
                }
                [38, 2, r, g, b] => {
                    self.attrs.fgcolor =
                        crate::Color::Rgb(to_u8!(*r), to_u8!(*g), to_u8!(*b));
                }
                [38, 2, _, r, g, b] => {
                    self.attrs.fgcolor =
                        crate::Color::Rgb(to_u8!(*r), to_u8!(*g), to_u8!(*b));
                }
                [38, 5, i] => {
                    self.attrs.fgcolor = crate::Color::Idx(to_u8!(*i));
                }
                [38] => match next_param!() {
                    [2] => {
                        let r = next_param_u8!();
                        let g = next_param_u8!();
                        let b = next_param_u8!();
                        self.attrs.fgcolor = crate::Color::Rgb(r, g, b);
                    }
                    [5] => {
                        self.attrs.fgcolor =
                            crate::Color::Idx(next_param_u8!());
                    }
                    _ => {
                        unhandled(self);
                        return;
                    }
                },
                [39] => {
                    self.attrs.fgcolor = crate::Color::Default;
                }
                [58, 2, r, g, b] => {
                    self.attrs.underline_color =
                        crate::Color::Rgb(to_u8!(*r), to_u8!(*g), to_u8!(*b));
                }
                [58, 2, _, r, g, b] => {
                    self.attrs.underline_color =
                        crate::Color::Rgb(to_u8!(*r), to_u8!(*g), to_u8!(*b));
                }
                [58, 5, i] => {
                    self.attrs.underline_color = crate::Color::Idx(to_u8!(*i));
                }
                [58] => match next_param!() {
                    [2] => {
                        let r = next_param_u8!();
                        let g = next_param_u8!();
                        let b = next_param_u8!();
                        self.attrs.underline_color = crate::Color::Rgb(r, g, b);
                    }
                    [5] => {
                        self.attrs.underline_color = crate::Color::Idx(next_param_u8!());
                    }
                    _ => {
                        unhandled(self);
                        return;
                    }
                },
                [59] => {
                    self.attrs.underline_color = crate::Color::Default;
                }
                [n] if (40..=47).contains(n) => {
                    self.attrs.bgcolor = crate::Color::Idx(to_u8!(*n) - 40);
                }
                [48, 2, r, g, b] => {
                    self.attrs.bgcolor =
                        crate::Color::Rgb(to_u8!(*r), to_u8!(*g), to_u8!(*b));
                }
                [48, 2, _, r, g, b] => {
                    self.attrs.bgcolor =
                        crate::Color::Rgb(to_u8!(*r), to_u8!(*g), to_u8!(*b));
                }
                [48, 5, i] => {
                    self.attrs.bgcolor = crate::Color::Idx(to_u8!(*i));
                }
                [48] => match next_param!() {
                    [2] => {
                        let r = next_param_u8!();
                        let g = next_param_u8!();
                        let b = next_param_u8!();
                        self.attrs.bgcolor = crate::Color::Rgb(r, g, b);
                    }
                    [5] => {
                        self.attrs.bgcolor =
                            crate::Color::Idx(next_param_u8!());
                    }
                    _ => {
                        unhandled(self);
                        return;
                    }
                },
                [49] => {
                    self.attrs.bgcolor = crate::Color::Default;
                }
                [n] if (90..=97).contains(n) => {
                    self.attrs.fgcolor = crate::Color::Idx(to_u8!(*n) - 82);
                }
                [n] if (100..=107).contains(n) => {
                    self.attrs.bgcolor = crate::Color::Idx(to_u8!(*n) - 92);
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

fn u16_to_u8(i: u16) -> Option<u8> {
    if i > u16::from(u8::MAX) {
        None
    } else {
        // safe because we just ensured that the value fits in a u8
        Some(i.try_into().unwrap())
    }
}
