//! A cell buffer for one client's screen.
//!
//! Renderers paint cells instead of writing escape sequences directly, and
//! [`Frame::diff`] compares the painted frame against the one the client is
//! already showing. Only the cells that actually changed are sent, so a frame
//! that repeats its predecessor costs no bytes at all.

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
    fn new(text: &str) -> Self {
        let mut length = 0;
        for (index, character) in text.char_indices() {
            let end = index + character.len_utf8();
            if end > CELL_TEXT_BYTES {
                break;
            }
            length = end;
        }
        let mut bytes = [0; CELL_TEXT_BYTES];
        bytes[..length].copy_from_slice(&text.as_bytes()[..length]);
        Self {
            bytes,
            length: length as u8,
        }
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
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: vt100::UnderlineStyle,
    pub inverse: bool,
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
            bold: cell.bold(),
            dim: cell.dim(),
            italic: cell.italic(),
            underline: cell.underline_style(),
            inverse: cell.inverse(),
        }
    }
}

pub type Rgb = (u8, u8, u8);

pub fn rgb((red, green, blue): Rgb) -> vt100::Color {
    vt100::Color::Rgb(red, green, blue)
}

/// What a client's terminal understands. Every frame is painted in 24-bit
/// colour, because that is what the theme and the programs in the panes both
/// speak; a terminal that has not said it renders 24-bit colour is sent the
/// nearest entry of the xterm 256-colour palette instead of an escape sequence
/// it would ignore or print.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColorDepth {
    #[default]
    TrueColor,
    Palette256,
}

impl ColorDepth {
    pub fn of(truecolor: bool) -> Self {
        if truecolor {
            Self::TrueColor
        } else {
            Self::Palette256
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
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
    }

    fn index(&self, row: u16, col: u16) -> Option<usize> {
        if row == 0 || col == 0 || row > self.rows || col > self.cols {
            return None;
        }
        Some((row as usize - 1) * self.cols as usize + (col as usize - 1))
    }

    fn cell(&self, row: u16, col: u16) -> Option<&FrameCell> {
        self.index(row, col).map(|index| &self.cells[index])
    }

    pub fn set_cell(&mut self, row: u16, col: u16, text: &str, attributes: CellAttributes) {
        let Some(index) = self.index(row, col) else {
            return;
        };
        self.cells[index] = FrameCell {
            text: CellText::new(text),
            attributes,
            continuation: false,
        };
    }

    /// Paints a double-width character, reserving the cell to its right.
    pub fn set_wide_cell(&mut self, row: u16, col: u16, text: &str, attributes: CellAttributes) {
        self.set_cell(row, col, text, attributes);
        if let Some(index) = self.index(row, col + 1) {
            self.cells[index] = FrameCell {
                text: CellText::empty(),
                attributes,
                continuation: true,
            };
        }
    }

    /// Paints `text` one character per cell and returns the column after it.
    pub fn set_text(&mut self, row: u16, col: u16, text: &str, attributes: CellAttributes) -> u16 {
        let mut col = col;
        let mut encoded = [0; 4];
        for character in text.chars() {
            self.set_cell(row, col, character.encode_utf8(&mut encoded), attributes);
            col = col.saturating_add(1);
        }
        col
    }

    pub fn fill(&mut self, row: u16, col: u16, width: u16, attributes: CellAttributes) {
        for offset in 0..width {
            self.set_cell(row, col.saturating_add(offset), " ", attributes);
        }
    }

    pub fn set_cursor(&mut self, cursor: FrameCursor) {
        self.cursor = cursor;
    }

    /// Appends the escape sequences that turn `previous` into this frame.
    ///
    /// Nothing is appended when the two frames are identical, so an idle
    /// terminal receives no output at all.
    pub fn diff(&mut self, previous: &Frame, colors: ColorDepth, output: &mut Vec<u8>) {
        let incremental = previous.rows == self.rows && previous.cols == self.cols;
        let start = output.len();
        output.extend_from_slice(b"\x1b[?2026h\x1b[?25l");
        if !incremental {
            output.extend_from_slice(b"\x1b[0m\x1b[2J");
        }
        let mut painted = !incremental;
        let mut attributes = None;
        for row in 1..=self.rows {
            self.mark_changed_cells(previous, row, incremental);
            painted |= self.paint_row(output, row, colors, &mut attributes);
        }
        // Repositioning matters only once something has painted over the old
        // spot, or once the cursor is on show. A pane that keeps moving a hidden
        // cursor without touching a cell still costs nothing.
        let settled =
            previous.cursor == self.cursor || (!previous.cursor.visible && !self.cursor.visible);
        if !painted && settled {
            output.truncate(start);
            return;
        }
        // Unconditional, even for a cursor nothing will draw: painting parks the
        // terminal's cursor against the last cell of the last run, and leaving
        // it there puts it inside whichever bar, panel, or popup happened to
        // paint last.
        move_to(output, self.cursor.row, self.cursor.col);
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
        colors: ColorDepth,
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
            for current in run_start..=end {
                let Some(cell) = self.cell(row, current) else {
                    continue;
                };
                if cell.continuation {
                    continue;
                }
                if *attributes != Some(cell.attributes) {
                    write_cell_attributes(output, cell.attributes, colors);
                    *attributes = Some(cell.attributes);
                }
                output.extend_from_slice(cell.text.as_bytes());
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

fn write_steady_cursor_shape(output: &mut Vec<u8>, shape: CursorShape) {
    output.extend_from_slice(b"\x1b[?12l");
    output.extend_from_slice(match shape {
        CursorShape::Block => b"\x1b[2 q",
        CursorShape::Underline => b"\x1b[4 q",
        CursorShape::Bar => b"\x1b[6 q",
    });
}

pub fn write_cell_attributes(output: &mut Vec<u8>, attributes: CellAttributes, colors: ColorDepth) {
    output.extend_from_slice(b"\x1b[0");
    if attributes.bold {
        output.extend_from_slice(b";1");
    }
    if attributes.dim {
        output.extend_from_slice(b";2");
    }
    if attributes.italic {
        output.extend_from_slice(b";3");
    }
    match attributes.underline {
        vt100::UnderlineStyle::None => {}
        vt100::UnderlineStyle::Straight => output.extend_from_slice(b";4"),
        style => {
            output.extend_from_slice(b";4:");
            output.push(b'0' + style as u8);
        }
    }
    if attributes.inverse {
        output.extend_from_slice(b";7");
    }
    write_color(output, attributes.foreground, true, colors);
    write_color(output, attributes.background, false, colors);
    output.push(b'm');
}

fn write_color(output: &mut Vec<u8>, color: vt100::Color, foreground: bool, colors: ColorDepth) {
    let parameter = if foreground { 38 } else { 48 };
    match color {
        vt100::Color::Default => output.extend_from_slice(if foreground { b";39" } else { b";49" }),
        vt100::Color::Idx(index) => {
            output.extend_from_slice(format!(";{parameter};5;{index}").as_bytes())
        }
        vt100::Color::Rgb(red, green, blue) => match colors {
            ColorDepth::TrueColor => {
                output.extend_from_slice(format!(";{parameter};2;{red};{green};{blue}").as_bytes())
            }
            ColorDepth::Palette256 => {
                let index = nearest_palette_index(red, green, blue);
                output.extend_from_slice(format!(";{parameter};5;{index}").as_bytes())
            }
        },
    }
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

    fn frame(rows: u16, cols: u16) -> Frame {
        let mut frame = Frame::default();
        frame.reset(rows, cols);
        frame
    }

    fn diff(current: &mut Frame, previous: &Frame) -> Vec<u8> {
        let mut output = Vec::new();
        current.diff(previous, ColorDepth::TrueColor, &mut output);
        output
    }

    /// Compares what two terminals show. A cell that was never written and one
    /// holding a blank look the same, so blanks are normalized before comparing.
    fn assert_screens_match(left: &vt100::Screen, right: &vt100::Screen, context: &str) {
        let (rows, cols) = left.size();
        assert_eq!(right.size(), (rows, cols), "{context}: size");
        for row in 0..rows {
            for col in 0..cols {
                let left_cell = left.cell(row, col).unwrap();
                let right_cell = right.cell(row, col).unwrap();
                fn text(cell: &vt100::Cell) -> String {
                    if cell.has_contents() {
                        cell.contents().to_string()
                    } else {
                        " ".into()
                    }
                }
                assert_eq!(
                    (
                        text(&left_cell),
                        CellAttributes::from(&left_cell),
                        left_cell.is_wide(),
                        left_cell.is_wide_continuation(),
                    ),
                    (
                        text(&right_cell),
                        CellAttributes::from(&right_cell),
                        right_cell.is_wide(),
                        right_cell.is_wide_continuation(),
                    ),
                    "{context}: cell at row {row} column {col}"
                );
            }
        }
    }

    /// Paints a screen's worth of varied content: colors, attributes, wide
    /// characters and an overlay, keyed on `step` so successive frames differ in
    /// every way a real render can.
    fn paint_sample(frame: &mut Frame, step: usize) {
        let plain = CellAttributes::default();
        let accent = CellAttributes::colors((203, 163, 210), (46, 39, 57)).bold();
        let dim = CellAttributes::foreground((150, 138, 166)).dim();
        for row in 1..=frame.rows() {
            frame.set_text(
                row,
                1,
                &format!("row {row} step {step} ~ some terminal output"),
                if row % 3 == 0 { dim } else { plain },
            );
        }
        frame.set_text(1, 1, &format!(" {} ", step % 10), accent);
        frame.set_wide_cell(2, 6 + (step % 4) as u16, "世", plain);
        frame.set_wide_cell(2, 8 + (step % 4) as u16, "界", accent);
        frame.set_text(4, 4, &format!("╭{}╮", "─".repeat(8)), accent);
        frame.set_text(5, 4, &format!("│ {:6} │", step), accent);
        frame.fill(6, 4, 10, accent);
        frame.set_cursor(FrameCursor {
            row: 2 + (step % 5) as u16,
            col: 3 + (step % 7) as u16,
            shape: if step.is_multiple_of(2) {
                CursorShape::Block
            } else {
                CursorShape::Bar
            },
            visible: true,
        });
    }

    #[test]
    fn incremental_diffs_land_exactly_where_a_full_repaint_would() {
        let (rows, cols) = (12, 48);
        // One terminal receives diffs; the other is repainted from scratch each
        // step. A real vt100 parser decides whether they agree.
        let mut incremental = vt100::Parser::new(rows, cols, 0);
        let mut previous = frame(rows, cols);
        for step in 0..12 {
            let mut current = frame(rows, cols);
            paint_sample(&mut current, step);

            let mut fresh = vt100::Parser::new(rows, cols, 0);
            let mut full = frame(rows, cols);
            paint_sample(&mut full, step);
            fresh.process(&diff(&mut full, &Frame::default()));

            incremental.process(&diff(&mut current, &previous));
            assert_screens_match(
                incremental.screen(),
                fresh.screen(),
                &format!("step {step}"),
            );
            assert_eq!(
                incremental.screen().cursor_position(),
                fresh.screen().cursor_position(),
                "step {step} cursor diverged"
            );
            previous = current;
        }
    }

    #[test]
    fn a_resize_diff_lands_where_a_full_repaint_would() {
        let mut previous = frame(6, 30);
        paint_sample(&mut previous, 1);
        let mut incremental = vt100::Parser::new(6, 30, 0);
        incremental.process(&diff(&mut previous, &Frame::default()));

        // The client's terminal grew; the daemon paints a bigger frame.
        incremental.screen_mut().set_size(6, 40);
        let mut current = frame(6, 40);
        paint_sample(&mut current, 2);
        incremental.process(&diff(&mut current, &previous));

        let mut fresh = vt100::Parser::new(6, 40, 0);
        let mut full = frame(6, 40);
        paint_sample(&mut full, 2);
        fresh.process(&diff(&mut full, &Frame::default()));

        assert_screens_match(incremental.screen(), fresh.screen(), "after resize");
    }

    #[test]
    fn a_first_frame_clears_and_paints_everything() {
        let mut current = frame(2, 3);
        current.set_text(1, 1, "abc", CellAttributes::default());
        let output = String::from_utf8(diff(&mut current, &Frame::default())).unwrap();
        assert!(output.contains("\x1b[2J"), "{output:?}");
        assert!(output.contains("abc"), "{output:?}");
        assert!(output.contains("\x1b[?2026h") && output.contains("\x1b[?2026l"));
    }

    #[test]
    fn a_terminal_without_truecolor_is_sent_palette_entries() {
        let mut current = frame(1, 3);
        current.set_text(
            1,
            1,
            "abc",
            CellAttributes::colors((203, 163, 210), (46, 39, 57)),
        );
        let mut output = Vec::new();
        current.diff(&Frame::default(), ColorDepth::Palette256, &mut output);
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains(";2;"), "no 24-bit colour: {output:?}");
        assert!(output.contains(";38;5;182"), "{output:?}");
        assert!(output.contains(";48;5;236"), "{output:?}");

        // Indexed and default colours are already what the terminal expects.
        let mut current = frame(1, 1);
        current.set_cell(
            1,
            1,
            "x",
            CellAttributes {
                foreground: vt100::Color::Idx(4),
                background: vt100::Color::Default,
                ..CellAttributes::default()
            },
        );
        let mut output = Vec::new();
        current.diff(&Frame::default(), ColorDepth::Palette256, &mut output);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains(";38;5;4;49"), "{output:?}");
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
        let mut previous = frame(4, 8);
        previous.set_text(2, 2, "hello", CellAttributes::default());
        previous.set_cursor(FrameCursor {
            row: 2,
            col: 7,
            shape: CursorShape::Block,
            visible: true,
        });
        let mut current = frame(4, 8);
        current.set_text(2, 2, "hello", CellAttributes::default());
        current.set_cursor(FrameCursor {
            row: 2,
            col: 7,
            shape: CursorShape::Block,
            visible: true,
        });
        assert!(diff(&mut current, &previous).is_empty());
    }

    #[test]
    fn only_changed_cells_are_repainted() {
        let mut previous = frame(3, 40);
        previous.set_text(
            2,
            1,
            "unchanged text on this row",
            CellAttributes::default(),
        );
        let mut current = frame(3, 40);
        current.set_text(
            2,
            1,
            "unchanged text on this row",
            CellAttributes::default(),
        );
        current.set_cell(2, 5, "X", CellAttributes::default());
        let output = String::from_utf8(diff(&mut current, &previous)).unwrap();
        assert!(output.contains("\x1b[2;5H"), "{output:?}");
        assert!(output.contains('X'), "{output:?}");
        assert!(!output.contains("unchanged"), "{output:?}");
        assert!(!output.contains("\x1b[2J"), "{output:?}");
    }

    #[test]
    fn nearby_runs_merge_instead_of_repositioning() {
        let previous = frame(2, 20);
        let mut current = frame(2, 20);
        current.set_cell(1, 1, "a", CellAttributes::default());
        current.set_cell(1, 4, "b", CellAttributes::default());
        // Parked off the painted row, so the only move onto it is the run's.
        current.set_cursor(FrameCursor {
            row: 2,
            col: 1,
            ..FrameCursor::default()
        });
        let output = String::from_utf8(diff(&mut current, &previous)).unwrap();
        assert_eq!(output.matches("\x1b[1;").count(), 1, "{output:?}");
        assert!(output.contains("a  b"), "{output:?}");
    }

    #[test]
    fn a_moved_cursor_alone_is_repositioned() {
        let mut previous = frame(3, 10);
        previous.set_cursor(FrameCursor {
            row: 1,
            col: 1,
            shape: CursorShape::Block,
            visible: true,
        });
        let mut current = frame(3, 10);
        current.set_cursor(FrameCursor {
            row: 3,
            col: 4,
            shape: CursorShape::Bar,
            visible: true,
        });
        let output = String::from_utf8(diff(&mut current, &previous)).unwrap();
        assert!(output.contains("\x1b[3;4H"), "{output:?}");
        assert!(output.contains("\x1b[6 q"), "{output:?}");
        assert!(output.contains("\x1b[?25h"), "{output:?}");
    }

    #[test]
    fn a_hidden_cursor_stays_hidden_but_still_takes_its_position() {
        let mut previous = frame(2, 4);
        previous.set_cursor(FrameCursor {
            row: 1,
            col: 1,
            shape: CursorShape::Block,
            visible: true,
        });
        let mut current = frame(2, 4);
        current.set_cursor(FrameCursor {
            row: 2,
            col: 3,
            shape: CursorShape::Block,
            visible: false,
        });
        let output = String::from_utf8(diff(&mut current, &previous)).unwrap();
        assert!(output.contains("\x1b[?25l"), "{output:?}");
        assert!(!output.contains("\x1b[?25h"), "{output:?}");
        // Invisible, but still moved: painting must not leave it on whatever
        // drew last.
        assert!(output.contains("\x1b[2;3H"), "{output:?}");
    }

    #[test]
    fn painting_never_leaves_the_cursor_on_the_last_painted_cell() {
        let previous = frame(3, 20);
        let mut current = frame(3, 20);
        // A bar across the bottom, the way an overlay paints last.
        current.set_text(3, 1, "status bar", CellAttributes::default());
        current.set_cursor(FrameCursor {
            row: 1,
            col: 5,
            shape: CursorShape::Block,
            visible: true,
        });
        let output = String::from_utf8(diff(&mut current, &previous)).unwrap();
        assert!(output.ends_with("\x1b[1;5H\x1b[?12l\x1b[2 q\x1b[?25h\x1b[?2026l"));
    }

    #[test]
    fn wide_characters_repaint_as_one_unit() {
        let previous = frame(1, 6);
        let mut current = frame(1, 6);
        current.set_wide_cell(1, 3, "世", CellAttributes::default());
        let output = String::from_utf8(diff(&mut current, &previous)).unwrap();
        assert!(output.contains("世"), "{output:?}");
        // The continuation cell is never emitted on its own.
        let mut later = frame(1, 6);
        later.set_wide_cell(1, 3, "界", CellAttributes::default());
        let output = String::from_utf8(diff(&mut later, &current)).unwrap();
        assert!(output.contains("\x1b[1;3H"), "{output:?}");
        assert!(output.contains("界"), "{output:?}");
    }

    #[test]
    fn a_resize_repaints_the_whole_screen() {
        let mut previous = frame(4, 4);
        previous.set_text(1, 1, "keep", CellAttributes::default());
        let mut current = frame(4, 8);
        current.set_text(1, 1, "keep", CellAttributes::default());
        let output = String::from_utf8(diff(&mut current, &previous)).unwrap();
        assert!(output.contains("\x1b[2J"), "{output:?}");
    }

    #[test]
    fn attributes_are_written_once_per_run() {
        let attributes = CellAttributes::colors((1, 2, 3), (4, 5, 6));
        let mut current = frame(1, 4);
        current.set_text(1, 1, "abcd", attributes);
        let output = String::from_utf8(diff(&mut current, &Frame::default())).unwrap();
        assert_eq!(output.matches("38;2;1;2;3").count(), 1, "{output:?}");
    }

    #[test]
    fn curly_underlines_survive_a_pane_repaint() {
        let mut parser = vt100::Parser::new(1, 5, 0);
        parser.process(b"\x1b[4:3mwave");
        let cell = parser.screen().cell(0, 0).unwrap();
        assert_eq!(cell.underline_style(), vt100::UnderlineStyle::Curly);

        let mut current = frame(1, 5);
        current.set_text(1, 1, "wave", CellAttributes::from(&cell));
        let output = String::from_utf8(diff(&mut current, &Frame::default())).unwrap();
        assert!(output.contains("4:3"), "{output:?}");
    }
}
