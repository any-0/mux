//! A cell buffer for one client's screen.
//!
//! Renderers paint cells instead of writing escape sequences directly, and
//! [`Frame::diff`] compares the painted frame against the one the client is
//! already showing. Only the cells that actually changed are sent, so a frame
//! that repeats its predecessor costs no bytes at all.

use unicode_width::UnicodeWidthChar;

/// Inline capacity for one cell's text. A `vt100` cell holds at most 22 bytes,
/// so this never truncates terminal content.
const CELL_TEXT_BYTES: usize = 24;

/// Unchanged cells cheaper to repaint than to skip with a cursor move.
const RUN_GAP: u16 = 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CellText {
    bytes: [u8; CELL_TEXT_BYTES],
    length: u8,
}

impl CellText {
    /// As much of `text` as fits, cut at a character boundary.
    fn new(text: &str) -> Self {
        let mut cell = Self::empty();
        for character in text.chars() {
            if !cell.push(character) {
                break;
            }
        }
        cell
    }

    const fn empty() -> Self {
        Self {
            bytes: [0; CELL_TEXT_BYTES],
            length: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.length as usize]
    }

    /// Appends `character` if it fits, returning whether it did.
    fn push(&mut self, character: char) -> bool {
        let start = self.length as usize;
        let end = start + character.len_utf8();
        if end > CELL_TEXT_BYTES {
            return false;
        }
        character.encode_utf8(&mut self.bytes[start..end]);
        self.length = end as u8;
        true
    }
}

impl Default for CellText {
    fn default() -> Self {
        Self::new(" ")
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct FrameCell {
    text: CellText,
    attributes: CellAttributes,
    /// The right half of a wide character; the terminal draws it implicitly.
    continuation: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CellAttributes {
    pub foreground: vt100::Color,
    pub background: vt100::Color,
    pub underline_color: vt100::Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: vt100::UnderlineStyle,
    pub inverse: bool,
    pub strikethrough: bool,
    pub blink: bool,
    pub hidden: bool,
    pub overline: bool,
}

impl CellAttributes {
    pub fn foreground(color: Rgb) -> Self {
        Self {
            foreground: rgb(color),
            ..Self::default()
        }
    }

    pub fn colors(foreground: Rgb, background: Rgb) -> Self {
        Self {
            foreground: rgb(foreground),
            background: rgb(background),
            ..Self::default()
        }
    }

    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub fn dim(mut self) -> Self {
        self.dim = true;
        self
    }

    pub fn with_background(mut self, color: Rgb) -> Self {
        self.background = rgb(color);
        self
    }
}

impl From<&vt100::Cell> for CellAttributes {
    fn from(cell: &vt100::Cell) -> Self {
        Self {
            foreground: cell.fgcolor(),
            background: cell.bgcolor(),
            underline_color: cell.underline_color(),
            bold: cell.bold(),
            dim: cell.dim(),
            italic: cell.italic(),
            underline: cell.underline_style(),
            inverse: cell.inverse(),
            strikethrough: cell.strikethrough(),
            blink: cell.blink(),
            hidden: cell.hidden(),
            overline: cell.overline(),
        }
    }
}

pub type Rgb = (u8, u8, u8);

pub fn rgb((red, green, blue): Rgb) -> vt100::Color {
    vt100::Color::Rgb(red, green, blue)
}

/// What a client's terminal understands beyond what every xterm-compatible
/// terminal does. Frames only use what the terminal has said it supports,
/// so an older or simpler terminal gets a plainer picture rather than escape
/// sequences it would misread.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TerminalFeatures {
    /// 24-bit colour. Frames are always painted in it, since the theme and the
    /// programs in the panes both speak it; without it, colours go out as the
    /// nearest 256-colour entry.
    pub truecolor: bool,
    /// Curly, dotted, dashed and double underlines (`4:n`) and underline
    /// colours (SGR 58). A terminal without them parses `58;2;r;g;b` as faint
    /// and a string of unrelated attributes, so they are left out instead and
    /// every styled underline becomes a plain one.
    pub styled_underlines: bool,
}

impl TerminalFeatures {
    /// Everything: what mux's own panes understand.
    pub const FULL: Self = Self {
        truecolor: true,
        styled_underlines: true,
    };
}

impl Default for TerminalFeatures {
    fn default() -> Self {
        Self::FULL
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorShape {
    Block,
    Underline,
    #[default]
    Bar,
}

/// Where the terminal's cursor rests at the end of a frame, in one-based screen
/// coordinates.
///
/// A frame always has one, even when nothing wants to show it. Painting leaves
/// the terminal's cursor wherever the last run ended, which is whichever piece
/// of interface drew last, so every frame moves it back to the place the
/// contents imply. `visible` only decides whether it is drawn there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameCursor {
    pub row: u16,
    pub col: u16,
    pub shape: CursorShape,
    pub visible: bool,
}

impl Default for FrameCursor {
    fn default() -> Self {
        Self {
            row: 1,
            col: 1,
            shape: CursorShape::Block,
            visible: false,
        }
    }
}

/// One client's screen. Coordinates are one-based, matching terminal
/// addressing: the top left cell is `(1, 1)`. Painting outside the frame is
/// silently dropped so callers can clip by construction.
#[derive(Debug, Default)]
pub struct Frame {
    rows: u16,
    cols: u16,
    cells: Vec<FrameCell>,
    cursor: FrameCursor,
    /// The colour the terminal's cursor is painted (OSC 12); `None` leaves
    /// the terminal's own.
    cursor_color: Option<Rgb>,
    /// What `cursor_color` returns to whenever the cursor is placed.
    default_cursor_color: Option<Rgb>,
    changed: Vec<bool>,
}

impl Frame {
    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Resizes to `rows` × `cols` and clears every cell, reusing the existing
    /// allocation so steady-state rendering does not allocate.
    pub fn reset(&mut self, rows: u16, cols: u16) {
        self.rows = rows;
        self.cols = cols;
        self.cells.clear();
        self.cells
            .resize(rows as usize * cols as usize, FrameCell::default());
        self.changed.clear();
        self.changed.resize(cols as usize, false);
        self.cursor = FrameCursor::default();
        self.cursor_color = None;
        self.default_cursor_color = None;
    }

    fn index(&self, row: u16, col: u16) -> Option<usize> {
        if row == 0 || col == 0 || row > self.rows || col > self.cols {
            return None;
        }
        Some((row as usize - 1) * self.cols as usize + (col as usize - 1))
    }

    /// The index of the cell right of `(row, col)`, if there is one.
    fn next_index(&self, row: u16, col: u16) -> Option<usize> {
        self.index(row, col.checked_add(1)?)
    }

    fn cell(&self, row: u16, col: u16) -> Option<&FrameCell> {
        self.index(row, col).map(|index| &self.cells[index])
    }

    pub fn set_cell(&mut self, row: u16, col: u16, text: &str, attributes: CellAttributes) {
        let Some(index) = self.index(row, col) else {
            return;
        };
        // Overwriting either half of a wide character erases the other half.
        if self.cells[index].continuation && index > 0 {
            self.cells[index - 1] = FrameCell::default();
        }
        if let Some(next) = self
            .next_index(row, col)
            .filter(|next| self.cells[*next].continuation)
        {
            self.cells[next] = FrameCell::default();
        }
        self.cells[index] = FrameCell {
            text: CellText::new(text),
            attributes,
            continuation: false,
        };
    }

    /// Paints a double-width character, reserving the cell to its right.
    pub fn set_wide_cell(&mut self, row: u16, col: u16, text: &str, attributes: CellAttributes) {
        let Some(next) = self.next_index(row, col) else {
            return;
        };
        // The new continuation may overwrite the base of another wide glyph.
        // Clear its old continuation before installing this pair.
        self.set_cell(row, col + 1, "", attributes);
        self.set_cell(row, col, text, attributes);
        self.cells[next] = FrameCell {
            text: CellText::empty(),
            attributes,
            continuation: true,
        };
    }

    /// Paints `text` by terminal cell width and returns the column after it.
    pub fn set_text(&mut self, row: u16, col: u16, text: &str, attributes: CellAttributes) -> u16 {
        let mut col = col;
        let mut encoded = [0; 4];
        for character in text.chars() {
            let text = character.encode_utf8(&mut encoded);
            match character.width() {
                None => {}
                Some(0) => {
                    let previous = col.saturating_sub(1);
                    if let Some(index) = self.index(row, previous) {
                        let base = if self.cells[index].continuation {
                            index.saturating_sub(1)
                        } else {
                            index
                        };
                        self.cells[base].text.push(character);
                    }
                }
                Some(1) => {
                    self.set_cell(row, col, text, attributes);
                    col = col.saturating_add(1);
                }
                // A wide character that does not fit is dropped.
                Some(_) if self.next_index(row, col).is_some() => {
                    self.set_wide_cell(row, col, text, attributes);
                    col = col.saturating_add(2);
                }
                Some(_) => {}
            }
        }
        col
    }

    pub fn fill(&mut self, row: u16, col: u16, width: u16, attributes: CellAttributes) {
        for offset in 0..width {
            self.set_cell(row, col.saturating_add(offset), " ", attributes);
        }
    }

    /// Places the cursor, in the default cursor colour.
    pub fn set_cursor(&mut self, cursor: FrameCursor) {
        self.cursor = cursor;
        self.cursor_color = self.default_cursor_color;
    }

    pub fn set_default_cursor_color(&mut self, color: Option<Rgb>) {
        self.default_cursor_color = color;
        self.cursor_color = color;
    }

    /// Colours the cursor last placed, until it is placed again.
    pub fn set_cursor_color(&mut self, color: Option<Rgb>) {
        self.cursor_color = color;
    }

    /// Appends the escape sequences that turn `previous` into this frame.
    ///
    /// Nothing is appended when the two frames are identical, so an idle
    /// terminal receives no output at all.
    pub fn diff(&mut self, previous: &Frame, terminal: TerminalFeatures, output: &mut Vec<u8>) {
        let incremental = previous.rows == self.rows && previous.cols == self.cols;
        let start = output.len();
        output.extend_from_slice(b"\x1b[?2026h\x1b[?25l");
        if !incremental {
            // Autowrap off first: then nothing drawn at the right edge can wrap,
            // and the bottom right cell can never scroll the screen, even for a
            // glyph the terminal thinks is wider than mux does. A full repaint
            // asserts it again in case something reset the terminal.
            output.extend_from_slice(b"\x1b[?7l\x1b[0m\x1b[2J");
        }
        let mut painted = !incremental;
        let mut attributes = None;
        for row in 1..=self.rows {
            self.mark_changed_cells(previous, row, incremental);
            painted |= self.paint_row(output, row, terminal, &mut attributes);
        }
        // Repositioning matters only once something has painted over the old
        // spot, or once the cursor is on show. A pane that keeps moving a hidden
        // cursor without touching a cell still costs nothing.
        let settled =
            previous.cursor == self.cursor || (!previous.cursor.visible && !self.cursor.visible);
        let recolored = !incremental || previous.cursor_color != self.cursor_color;
        if !painted && settled && !recolored {
            output.truncate(start);
            return;
        }
        // Unconditional, even for a cursor nothing will draw: painting parks the
        // terminal's cursor against the last cell of the last run, and leaving
        // it there puts it inside whichever bar, panel, or popup happened to
        // paint last.
        move_to(output, self.cursor.row, self.cursor.col);
        if recolored {
            write_cursor_color(output, self.cursor_color);
        }
        if self.cursor.visible {
            write_steady_cursor_shape(output, self.cursor.shape);
            output.extend_from_slice(b"\x1b[?25h");
        }
        output.extend_from_slice(b"\x1b[?2026l");
    }

    /// Flags the cells of `row` that need repainting, keeping the two halves of
    /// a wide character together so a run never starts mid-character.
    fn mark_changed_cells(&mut self, previous: &Frame, row: u16, incremental: bool) {
        let width = self.cols as usize;
        let offset = (row as usize - 1) * width;
        for col in 0..width {
            self.changed[col] =
                !incremental || self.cells[offset + col] != previous.cells[offset + col];
        }
        for col in 1..width {
            if self.changed[col] && self.cells[offset + col].continuation {
                self.changed[col - 1] = true;
            }
        }
        for col in 0..width.saturating_sub(1) {
            if self.changed[col] && self.cells[offset + col + 1].continuation {
                self.changed[col + 1] = true;
            }
        }
    }

    /// Emits the flagged cells of `row` as as few positioned runs as possible.
    fn paint_row(
        &self,
        output: &mut Vec<u8>,
        row: u16,
        terminal: TerminalFeatures,
        attributes: &mut Option<CellAttributes>,
    ) -> bool {
        let mut painted = false;
        let mut col = 1;
        while col <= self.cols {
            if !self.changed[col as usize - 1] {
                col += 1;
                continue;
            }
            let mut end = col;
            let mut probe = col;
            while probe <= self.cols {
                if self.changed[probe as usize - 1] {
                    end = probe;
                    probe += 1;
                    continue;
                }
                let gap_start = probe;
                while probe <= self.cols && !self.changed[probe as usize - 1] {
                    probe += 1;
                }
                if probe > self.cols || probe - gap_start > RUN_GAP {
                    break;
                }
            }
            if self
                .cell(row, end + 1)
                .is_some_and(|cell| cell.continuation)
            {
                end += 1;
            }
            let mut run_start = col;
            if run_start > 1
                && self
                    .cell(row, run_start)
                    .is_some_and(|cell| cell.continuation)
            {
                run_start -= 1;
            }
            move_to(output, row, run_start);
            let mut resync = false;
            for current in run_start..=end {
                let Some(cell) = self.cell(row, current) else {
                    continue;
                };
                if cell.continuation {
                    continue;
                }
                // The terminal and mux can disagree on how wide a character is
                // (emoji presentation, ambiguous East Asian width, a different
                // Unicode version). Placing the next cell explicitly after any
                // non-ASCII one keeps a disagreement from shifting the rest.
                if resync {
                    move_to_column(output, current);
                }
                if *attributes != Some(cell.attributes) {
                    write_cell_attributes(output, cell.attributes, terminal);
                    *attributes = Some(cell.attributes);
                }
                let text = cell.text.as_bytes();
                output.extend_from_slice(text);
                resync = width_is_uncertain(text);
            }
            painted = true;
            col = end + 1;
        }
        painted
    }
}

fn move_to(output: &mut Vec<u8>, row: u16, col: u16) {
    output.extend_from_slice(format!("\x1b[{row};{col}H").as_bytes());
}

/// Whether terminals may disagree on how many columns `text` takes: a
/// character with a variation selector, joiner or combining mark after it,
/// an East Asian ambiguous one (box drawing, many symbols), or one that can
/// be drawn as an emoji. Plain accented letters and CJK ideographs are the same
/// width everywhere and need no help.
fn width_is_uncertain(text: &[u8]) -> bool {
    if text.is_ascii() {
        return false;
    }
    let Ok(text) = std::str::from_utf8(text) else {
        return true;
    };
    let mut characters = text.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if characters.next().is_some() {
        return true;
    }
    first.width() != first.width_cjk()
        || matches!(
            u32::from(first),
            0x2190..=0x21FF
                | 0x2300..=0x23FF
                | 0x2460..=0x24FF
                | 0x25A0..=0x27BF
                | 0x2900..=0x297F
                | 0x2B00..=0x2BFF
                | 0x3030
                | 0x303D
                | 0x3297
                | 0x3299
                | 0x1F000..=0x1FAFF
        )
}

fn move_to_column(output: &mut Vec<u8>, col: u16) {
    output.extend_from_slice(format!("\x1b[{col}G").as_bytes());
}

fn write_cursor_color(output: &mut Vec<u8>, color: Option<Rgb>) {
    match color {
        Some((red, green, blue)) => output
            .extend_from_slice(format!("\x1b]12;#{red:02x}{green:02x}{blue:02x}\x1b\\").as_bytes()),
        None => output.extend_from_slice(b"\x1b]112\x1b\\"),
    }
}

fn write_steady_cursor_shape(output: &mut Vec<u8>, shape: CursorShape) {
    output.extend_from_slice(b"\x1b[?12l");
    output.extend_from_slice(match shape {
        CursorShape::Block => b"\x1b[2 q",
        CursorShape::Underline => b"\x1b[4 q",
        CursorShape::Bar => b"\x1b[6 q",
    });
}

fn write_cell_attributes(
    output: &mut Vec<u8>,
    attributes: CellAttributes,
    terminal: TerminalFeatures,
) {
    let flags = |output: &mut Vec<u8>, flags: &[(bool, &[u8])]| {
        for (on, code) in flags {
            if *on {
                output.extend_from_slice(code);
            }
        }
    };
    output.extend_from_slice(b"\x1b[0");
    flags(
        output,
        &[
            (attributes.bold, b";1"),
            (attributes.dim, b";2"),
            (attributes.italic, b";3"),
        ],
    );
    match attributes.underline {
        vt100::UnderlineStyle::None => {}
        vt100::UnderlineStyle::Straight => output.extend_from_slice(b";4"),
        _ if !terminal.styled_underlines => output.extend_from_slice(b";4"),
        style => {
            output.extend_from_slice(b";4:");
            output.push(b'0' + style as u8);
        }
    }
    flags(
        output,
        &[
            (attributes.blink, b";5"),
            (attributes.inverse, b";7"),
            (attributes.hidden, b";8"),
            (attributes.strikethrough, b";9"),
            (attributes.overline, b";53"),
        ],
    );
    write_color(output, attributes.foreground, 38, terminal.truecolor);
    write_color(output, attributes.background, 48, terminal.truecolor);
    if attributes.underline_color != vt100::Color::Default && terminal.styled_underlines {
        write_color(output, attributes.underline_color, 58, terminal.truecolor);
    }
    output.push(b'm');
}

/// Writes one SGR colour for `parameter`: 38 (foreground), 48 (background) or
/// 58 (underline).
fn write_color(output: &mut Vec<u8>, color: vt100::Color, parameter: u8, truecolor: bool) {
    let color = match color {
        vt100::Color::Rgb(red, green, blue) if !truecolor => {
            vt100::Color::Idx(nearest_palette_index(red, green, blue))
        }
        color => color,
    };
    let text = match color {
        vt100::Color::Default => format!(";{}", parameter + 1),
        // The sixteen basic colours have their own codes, which every terminal
        // understands, including ones without a 256-colour palette.
        vt100::Color::Idx(index @ 0..=15) if parameter != 58 => {
            let base = match (parameter, index < 8) {
                (38, true) => 30,
                (38, false) => 90 - 8,
                (_, true) => 40,
                (_, false) => 100 - 8,
            };
            format!(";{}", base + u16::from(index))
        }
        // The underline colour goes out in its colon form, which a terminal
        // that does not know SGR 58 skips as one unit rather than reading the
        // numbers after it as separate attributes.
        vt100::Color::Idx(index) if parameter == 58 => format!(";58:5:{index}"),
        vt100::Color::Idx(index) => format!(";{parameter};5;{index}"),
        vt100::Color::Rgb(red, green, blue) if parameter == 58 => {
            format!(";58:2::{red}:{green}:{blue}")
        }
        vt100::Color::Rgb(red, green, blue) => format!(";{parameter};2;{red};{green};{blue}"),
    };
    output.extend_from_slice(text.as_bytes());
}

/// The entry of the xterm 256-colour palette closest to an exact colour.
///
/// Only the 6×6×6 cube and the grey ramp are considered. The sixteen colours
/// below them are whatever the terminal's own theme sets them to, so matching
/// against their nominal values would land somewhere unpredictable.
fn nearest_palette_index(red: u8, green: u8, blue: u8) -> u8 {
    /// The values the cube's axes actually take.
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let distance = |from: Rgb| {
        let channel = |left: u8, right: u8| {
            let difference = i32::from(left) - i32::from(right);
            difference * difference
        };
        channel(from.0, red) + channel(from.1, green) + channel(from.2, blue)
    };
    let axis = |value: u8| {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, level)| level.abs_diff(value))
            .map(|(index, _)| index)
            .unwrap()
    };
    let (red_axis, green_axis, blue_axis) = (axis(red), axis(green), axis(blue));
    let cube = 16 + 36 * red_axis + 6 * green_axis + blue_axis;
    let cube_distance = distance((LEVELS[red_axis], LEVELS[green_axis], LEVELS[blue_axis]));

    // The grey ramp runs 8, 18, .. 238 and is much finer than the cube's grey
    // diagonal, so a near-grey colour usually belongs there instead.
    let average = (u32::from(red) + u32::from(green) + u32::from(blue)) / 3;
    let step = ((average as i32 - 8 + 5) / 10).clamp(0, 23);
    let grey = (8 + 10 * step) as u8;
    let grey_distance = distance((grey, grey, grey));

    if grey_distance < cube_distance {
        (232 + step) as u8
    } else {
        cube as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: CellAttributes = CellAttributes {
        foreground: vt100::Color::Default,
        background: vt100::Color::Default,
        underline_color: vt100::Color::Default,
        bold: false,
        dim: false,
        italic: false,
        underline: vt100::UnderlineStyle::None,
        inverse: false,
        strikethrough: false,
        blink: false,
        hidden: false,
        overline: false,
    };

    fn frame(rows: u16, cols: u16) -> Frame {
        let mut frame = Frame::default();
        frame.reset(rows, cols);
        frame
    }

    fn cursor(row: u16, col: u16, shape: CursorShape, visible: bool) -> FrameCursor {
        FrameCursor {
            row,
            col,
            shape,
            visible,
        }
    }

    fn diff_for(current: &mut Frame, previous: &Frame, terminal: TerminalFeatures) -> String {
        let mut output = Vec::new();
        current.diff(previous, terminal, &mut output);
        String::from_utf8(output).unwrap()
    }

    fn diff(current: &mut Frame, previous: &Frame) -> String {
        diff_for(current, previous, TerminalFeatures::FULL)
    }

    /// What a terminal shows after receiving `current` as a first frame.
    fn render(current: &mut Frame) -> vt100::Parser {
        let mut parser = vt100::Parser::new(current.rows(), current.cols(), 0);
        parser.process(diff(current, &Frame::default()).as_bytes());
        parser
    }

    fn attributes_written(attributes: CellAttributes, terminal: TerminalFeatures) -> String {
        let mut output = Vec::new();
        write_cell_attributes(&mut output, attributes, terminal);
        String::from_utf8(output).unwrap()
    }

    /// Compares what two terminals show. A cell that was never written and one
    /// holding a blank look the same, so blanks are normalized before comparing.
    fn assert_screens_match(left: &vt100::Screen, right: &vt100::Screen, context: &str) {
        fn cell(screen: &vt100::Screen, row: u16, col: u16) -> impl PartialEq + std::fmt::Debug {
            let cell = screen.cell(row, col).unwrap();
            let text = if cell.has_contents() {
                cell.contents().to_string()
            } else {
                " ".into()
            };
            let attributes = CellAttributes::from(&cell);
            (
                text,
                attributes,
                cell.is_wide(),
                cell.is_wide_continuation(),
            )
        }
        let (rows, cols) = left.size();
        assert_eq!(right.size(), (rows, cols), "{context}: size");
        for row in 0..rows {
            for col in 0..cols {
                assert_eq!(
                    cell(left, row, col),
                    cell(right, row, col),
                    "{context}: cell at row {row} column {col}"
                );
            }
        }
        assert_eq!(
            left.cursor_position(),
            right.cursor_position(),
            "{context}: cursor"
        );
    }

    /// Paints a screen's worth of varied content: colors, attributes, wide
    /// characters and an overlay, keyed on `step` so successive frames differ in
    /// every way a real render can.
    fn paint_sample(rows: u16, cols: u16, step: usize) -> Frame {
        let mut frame = frame(rows, cols);
        let accent = CellAttributes::colors((203, 163, 210), (46, 39, 57)).bold();
        let dim = CellAttributes::foreground((150, 138, 166)).dim();
        for row in 1..=rows {
            frame.set_text(
                row,
                1,
                &format!("row {row} step {step} ~ some terminal output"),
                if row % 3 == 0 { dim } else { PLAIN },
            );
        }
        frame.set_text(1, 1, &format!(" {} ", step % 10), accent);
        frame.set_wide_cell(2, 6 + (step % 4) as u16, "世", PLAIN);
        frame.set_wide_cell(2, 8 + (step % 4) as u16, "界", accent);
        frame.set_text(4, 4, &format!("╭{}╮", "─".repeat(8)), accent);
        frame.set_text(5, 4, &format!("│ {:6} │", step), accent);
        frame.fill(6, 4, 10, accent);
        let shape = if step.is_multiple_of(2) {
            CursorShape::Block
        } else {
            CursorShape::Bar
        };
        frame.set_cursor(cursor(
            2 + (step % 5) as u16,
            3 + (step % 7) as u16,
            shape,
            true,
        ));
        frame
    }

    #[test]
    fn incremental_diffs_land_exactly_where_a_full_repaint_would() {
        let (rows, cols) = (12, 48);
        // One terminal receives diffs; the other is repainted from scratch each
        // step. A real vt100 parser decides whether they agree.
        let mut incremental = vt100::Parser::new(rows, cols, 0);
        let mut previous = frame(rows, cols);
        for step in 0..12 {
            let mut current = paint_sample(rows, cols, step);
            incremental.process(diff(&mut current, &previous).as_bytes());
            let fresh = render(&mut paint_sample(rows, cols, step));
            assert_screens_match(
                incremental.screen(),
                fresh.screen(),
                &format!("step {step}"),
            );
            previous = current;
        }
    }

    #[test]
    fn a_resize_diff_lands_where_a_full_repaint_would() {
        let mut previous = paint_sample(6, 30, 1);
        let mut incremental = render(&mut previous);
        // The client's terminal grew; the daemon paints a bigger frame.
        incremental.screen_mut().set_size(6, 40);
        let mut current = paint_sample(6, 40, 2);
        let output = diff(&mut current, &previous);
        assert!(output.contains("\x1b[2J"), "{output:?}");
        incremental.process(output.as_bytes());
        let fresh = render(&mut paint_sample(6, 40, 2));
        assert_screens_match(incremental.screen(), fresh.screen(), "after resize");
    }

    #[test]
    fn a_first_frame_turns_autowrap_off_clears_and_paints_everything() {
        let mut current = frame(2, 3);
        current.set_text(2, 1, "abc", PLAIN);
        let output = diff(&mut current, &Frame::default());
        assert!(output.starts_with("\x1b[?2026h\x1b[?25l\x1b[?7l\x1b[0m\x1b[2J"));
        assert!(output.contains("abc"), "{output:?}");
        assert!(output.ends_with("\x1b[?2026l"), "{output:?}");
    }

    #[test]
    fn a_terminal_without_truecolor_is_sent_palette_entries() {
        let mut current = frame(1, 3);
        let attributes = CellAttributes::colors((203, 163, 210), (46, 39, 57));
        current.set_text(1, 1, "abc", attributes);
        let palette = TerminalFeatures {
            truecolor: false,
            ..TerminalFeatures::FULL
        };
        let output = diff_for(&mut current, &Frame::default(), palette);
        assert!(!output.contains(";2;"), "no 24-bit colour: {output:?}");
        assert!(output.contains(";38;5;182;48;5;236"), "{output:?}");
    }

    #[test]
    fn palette_entries_come_from_the_cube_or_the_finer_grey_ramp() {
        // Exact cube corners and their axis values.
        assert_eq!(nearest_palette_index(0, 0, 0), 16);
        assert_eq!(nearest_palette_index(255, 255, 255), 231);
        assert_eq!(nearest_palette_index(255, 0, 0), 196);
        assert_eq!(nearest_palette_index(0, 255, 0), 46);
        assert_eq!(nearest_palette_index(0, 0, 255), 21);
        // A grey the cube can only round to 135 sits on the ramp instead, which
        // steps every ten values.
        assert_eq!(nearest_palette_index(128, 128, 128), 244);
        assert_eq!(nearest_palette_index(8, 8, 8), 232);
        assert_eq!(nearest_palette_index(238, 238, 238), 255);
        // Far enough off the diagonal and the cube wins again.
        assert_eq!(nearest_palette_index(128, 40, 200), 92);
    }

    #[test]
    fn an_unchanged_frame_sends_no_bytes() {
        let paint = || {
            let mut frame = frame(4, 8);
            frame.set_text(2, 2, "hello", PLAIN);
            frame.set_cursor(cursor(2, 7, CursorShape::Block, true));
            frame
        };
        assert!(diff(&mut paint(), &paint()).is_empty());
        // Nor does a hidden cursor that moves without anything painting.
        let mut moved = paint();
        moved.set_cursor(cursor(3, 1, CursorShape::Bar, false));
        let mut hidden = paint();
        hidden.set_cursor(cursor(1, 1, CursorShape::Block, false));
        assert!(diff(&mut moved, &hidden).is_empty());
    }

    #[test]
    fn only_changed_cells_are_repainted() {
        let text = "unchanged text on this row";
        let mut previous = frame(3, 40);
        previous.set_text(2, 1, text, PLAIN);
        let mut current = frame(3, 40);
        current.set_text(2, 1, text, PLAIN);
        current.set_cell(2, 5, "X", PLAIN);
        let output = diff(&mut current, &previous);
        assert!(output.contains("\x1b[2;5H\x1b[0;39;49mX"), "{output:?}");
        assert!(!output.contains("unchanged"), "{output:?}");
        assert!(!output.contains("\x1b[2J"), "{output:?}");
    }

    #[test]
    fn nearby_runs_merge_instead_of_repositioning() {
        let previous = frame(2, 20);
        let mut current = frame(2, 20);
        current.set_cell(1, 1, "a", PLAIN);
        current.set_cell(1, 4, "b", PLAIN);
        // Parked off the painted row, so the only move onto it is the run's.
        current.set_cursor(cursor(2, 1, CursorShape::Block, false));
        let output = diff(&mut current, &previous);
        assert_eq!(output.matches("\x1b[1;").count(), 1, "{output:?}");
        assert!(output.contains("a  b"), "{output:?}");
    }

    #[test]
    fn placing_the_cursor_returns_it_to_the_default_colour() {
        let mut frame = frame(2, 4);
        frame.set_default_cursor_color(Some((1, 2, 3)));
        frame.set_cursor(cursor(1, 1, CursorShape::Block, true));
        frame.set_cursor_color(Some((9, 9, 9)));
        assert_eq!(frame.cursor_color, Some((9, 9, 9)));
        // A popup placing its own cursor afterwards gets the default back.
        frame.set_cursor(cursor(2, 2, CursorShape::Bar, true));
        assert_eq!(frame.cursor_color, Some((1, 2, 3)));
    }

    #[test]
    fn the_cursor_colour_is_sent_on_a_full_repaint_and_when_it_changes() {
        let paint = |color| {
            let mut frame = frame(2, 4);
            frame.set_cursor_color(color);
            frame
        };
        let red = Some((0xff, 0x00, 0x10));
        let first = diff(&mut paint(red), &Frame::default());
        assert!(first.contains("\x1b]12;#ff0010\x1b\\"), "{first:?}");
        assert!(diff(&mut paint(red), &paint(red)).is_empty());
        let blue = diff(&mut paint(Some((0, 0, 0xff))), &paint(red));
        assert!(blue.contains("\x1b]12;#0000ff\x1b\\"), "{blue:?}");
        let reset = diff(&mut paint(None), &paint(red));
        assert!(reset.contains("\x1b]112\x1b\\"), "{reset:?}");
    }

    #[test]
    fn a_moved_cursor_alone_is_repositioned() {
        let mut previous = frame(3, 10);
        previous.set_cursor(cursor(1, 1, CursorShape::Block, true));
        let mut current = frame(3, 10);
        current.set_cursor(cursor(3, 4, CursorShape::Bar, true));
        let output = diff(&mut current, &previous);
        assert!(output.ends_with("\x1b[3;4H\x1b[?12l\x1b[6 q\x1b[?25h\x1b[?2026l"));
    }

    #[test]
    fn a_hidden_cursor_stays_hidden_but_still_takes_its_position() {
        let mut previous = frame(2, 4);
        previous.set_cursor(cursor(1, 1, CursorShape::Block, true));
        let mut current = frame(2, 4);
        current.set_cursor(cursor(2, 3, CursorShape::Block, false));
        let output = diff(&mut current, &previous);
        assert!(!output.contains("\x1b[?25h"), "{output:?}");
        // Invisible, but still moved: painting must not leave it on whatever
        // drew last.
        assert!(output.ends_with("\x1b[2;3H\x1b[?2026l"), "{output:?}");
    }

    #[test]
    fn painting_never_leaves_the_cursor_on_the_last_painted_cell() {
        let previous = frame(3, 20);
        let mut current = frame(3, 20);
        // A bar across the bottom, the way an overlay paints last.
        current.set_text(3, 1, "status bar", PLAIN);
        current.set_cursor(cursor(1, 5, CursorShape::Block, true));
        let output = diff(&mut current, &previous);
        assert!(output.ends_with("\x1b[1;5H\x1b[?12l\x1b[2 q\x1b[?25h\x1b[?2026l"));
    }

    #[test]
    fn wide_characters_repaint_as_one_unit() {
        let mut current = frame(1, 6);
        current.set_wide_cell(1, 3, "世", PLAIN);
        let mut later = frame(1, 6);
        later.set_wide_cell(1, 3, "界", PLAIN);
        let output = diff(&mut later, &current);
        assert!(
            output.contains("\x1b[1;3H\x1b[0;39;49m界\x1b"),
            "{output:?}"
        );
    }

    #[test]
    fn attributes_are_written_once_per_run() {
        let mut current = frame(1, 4);
        current.set_text(1, 1, "abcd", CellAttributes::colors((1, 2, 3), (4, 5, 6)));
        let output = diff(&mut current, &Frame::default());
        assert_eq!(output.matches("38;2;1;2;3").count(), 1, "{output:?}");
    }

    #[test]
    fn interface_text_uses_terminal_cell_widths_and_drops_controls() {
        let mut current = frame(1, 6);
        assert_eq!(current.set_text(1, 1, "A\x1b界\ne\u{301}", PLAIN), 5);
        let parser = render(&mut current);
        assert_eq!(parser.screen().contents().trim_end(), "A界e\u{301}");
        assert!(parser.screen().cell(0, 1).unwrap().is_wide());
        assert!(parser.screen().cell(0, 2).unwrap().is_wide_continuation());
    }

    #[test]
    fn overwriting_either_half_clears_the_old_wide_character() {
        let mut current = frame(1, 4);
        let cell = |frame: &Frame, col| frame.cells[frame.index(1, col).unwrap()];
        current.set_wide_cell(1, 2, "界", PLAIN);
        current.set_cell(1, 2, "a", PLAIN);
        assert!(!cell(&current, 3).continuation);

        current.set_wide_cell(1, 2, "界", PLAIN);
        current.set_cell(1, 3, "b", PLAIN);
        assert_eq!(cell(&current, 2), FrameCell::default());

        // An overlapping wide glyph clears the previous one's trailing half.
        current.set_wide_cell(1, 2, "界", PLAIN);
        current.set_wide_cell(1, 1, "語", PLAIN);
        assert_eq!(cell(&current, 3), FrameCell::default());
        assert_eq!(render(&mut current).screen().contents().trim_end(), "語");
    }

    #[test]
    fn a_wide_character_is_not_painted_without_two_cells() {
        let mut current = frame(1, 2);
        assert_eq!(current.set_text(1, 2, "界", PLAIN), 2);
        assert_eq!(current.cells[1], FrameCell::default());
    }

    #[test]
    fn only_characters_of_uncertain_width_are_followed_by_a_reposition() {
        assert!(!width_is_uncertain(b"a"));
        assert!(!width_is_uncertain("é".as_bytes()));
        assert!(!width_is_uncertain("中".as_bytes()));
        assert!(width_is_uncertain("─".as_bytes()));
        assert!(width_is_uncertain("•".as_bytes()));
        assert!(width_is_uncertain("😀".as_bytes()));
        assert!(width_is_uncertain("❤\u{fe0f}".as_bytes()));
        assert!(width_is_uncertain("e\u{301}".as_bytes()));
        assert!(width_is_uncertain("\u{e010}".as_bytes()));

        let mut current = frame(1, 8);
        current.set_text(1, 1, "a─b中c", PLAIN);
        let output = diff(&mut current, &Frame::default());
        assert!(output.contains("─\x1b[3Gb"), "{output:?}");
        assert!(output.contains("中c"), "{output:?}");
    }

    #[test]
    fn styled_underlines_are_sent_only_to_terminals_that_draw_them() {
        // Parsed the way a pane's program would send it.
        let mut parser = vt100::Parser::new(1, 5, 0);
        parser.process(b"\x1b[4:3;58:2::255:0:0mwave");
        let attributes = CellAttributes::from(&parser.screen().cell(0, 0).unwrap());
        let plain = TerminalFeatures {
            styled_underlines: false,
            ..TerminalFeatures::FULL
        };
        assert_eq!(attributes_written(attributes, plain), "\x1b[0;4;39;49m");
        assert_eq!(
            attributes_written(attributes, TerminalFeatures::FULL),
            "\x1b[0;4:3;39;49;58:2::255:0:0m"
        );
        let indexed = CellAttributes {
            underline_color: vt100::Color::Idx(200),
            ..attributes
        };
        assert_eq!(
            attributes_written(indexed, TerminalFeatures::FULL),
            "\x1b[0;4:3;39;49;58:5:200m"
        );
    }

    #[test]
    fn every_attribute_and_basic_colour_has_its_own_code() {
        let attributes = CellAttributes {
            foreground: vt100::Color::Idx(1),
            background: vt100::Color::Idx(12),
            bold: true,
            dim: true,
            italic: true,
            underline: vt100::UnderlineStyle::Straight,
            inverse: true,
            strikethrough: true,
            blink: true,
            hidden: true,
            overline: true,
            ..PLAIN
        };
        assert_eq!(
            attributes_written(attributes, TerminalFeatures::FULL),
            "\x1b[0;1;2;3;4;5;7;8;9;53;31;104m"
        );
        let indexed = CellAttributes {
            foreground: vt100::Color::Idx(100),
            ..PLAIN
        };
        assert_eq!(
            attributes_written(indexed, TerminalFeatures::FULL),
            "\x1b[0;38;5;100;49m"
        );
    }
}
