//! One neutral picture of a terminal screen, built from either emulator, so the
//! two can be compared cell by cell.

use std::fmt::Write as _;

use alacritty_terminal::{
    grid::Dimensions,
    index::{Column, Line},
    term::{Term, TermMode, cell::Flags},
    vte::ansi::{Color as AColor, NamedColor},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    /// 0 none, 1 single, 2 double, 3 curly, 4 dotted, 5 dashed.
    pub underline: u8,
    pub inverse: bool,
    pub strike: bool,
    pub hidden: bool,
    pub wide: bool,
    pub spacer: bool,
    /// Where a wide character did not fit and wrapped: terminals differ on
    /// whether this last column is blanked, so any content matches it.
    pub any: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: " ".into(),
            fg: Color::Default,
            bg: Color::Default,
            bold: false,
            dim: false,
            italic: false,
            underline: 0,
            inverse: false,
            strike: false,
            hidden: false,
            wide: false,
            spacer: false,
            any: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub rows: usize,
    pub cols: usize,
    pub cells: Vec<Cell>,
    pub cursor: (usize, usize),
    pub cursor_visible: bool,
}

/// What a comparison looks at. Emulators legitimately disagree on some
/// things (a hidden cursor's column, attributes on erased blanks), so each
/// test states what it holds the two to.
#[derive(Clone, Copy, Debug)]
pub struct Compare {
    pub attributes: bool,
    pub cursor: bool,
    /// Compare colours of blank cells too (background colour erase).
    pub blank_backgrounds: bool,
    pub strike: bool,
}

impl Compare {
    pub const TEXT: Self = Self {
        attributes: false,
        cursor: false,
        blank_backgrounds: false,
        strike: false,
    };
    pub const FULL: Self = Self {
        attributes: true,
        cursor: true,
        blank_backgrounds: true,
        strike: true,
    };
}

impl Snapshot {
    pub fn blank(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            cells: vec![Cell::default(); rows * cols],
            cursor: (0, 0),
            cursor_visible: true,
        }
    }

    pub fn cell(&self, row: usize, col: usize) -> &Cell {
        &self.cells[row * self.cols + col]
    }

    pub fn row_text(&self, row: usize) -> String {
        (0..self.cols)
            .filter(|col| !self.cell(row, *col).spacer)
            .map(|col| self.cell(row, col).text.as_str())
            .collect()
    }

    pub fn text(&self) -> String {
        (0..self.rows)
            .map(|row| self.row_text(row).trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn contains(&self, needle: &str) -> bool {
        (0..self.rows).any(|row| self.row_text(row).contains(needle))
    }

    /// The rectangle `rows` x `cols` whose top left corner is `(top, left)`.
    pub fn crop(&self, top: usize, left: usize, rows: usize, cols: usize) -> Snapshot {
        let mut cells = Vec::with_capacity(rows * cols);
        for row in top..top + rows {
            for col in left..left + cols {
                cells.push(self.cell(row, col).clone());
            }
        }
        Snapshot {
            rows,
            cols,
            cells,
            cursor: (
                self.cursor.0.saturating_sub(top),
                self.cursor.1.saturating_sub(left),
            ),
            cursor_visible: self.cursor_visible,
        }
    }

    pub fn from_alacritty<T>(term: &Term<T>) -> Self {
        let grid = term.grid();
        let rows = grid.screen_lines();
        let cols = grid.columns();
        let mut cells = Vec::with_capacity(rows * cols);
        for row in 0..rows {
            for col in 0..cols {
                let cell = &grid[Line(row as i32)][Column(col)];
                let flags = cell.flags;
                let mut text = String::new();
                text.push(cell.c);
                // A zero-width character on a blank draws nothing; vt100
                // discards it there, alacritty keeps it.
                if let Some(zerowidth) = cell.zerowidth()
                    && cell.c != ' '
                {
                    // U+200B draws nothing anywhere; where it attaches after
                    // a carriage return differs between emulators.
                    text.extend(zerowidth.iter().filter(|c| **c != '\u{200b}'));
                }
                let spacer = flags.intersects(Flags::WIDE_CHAR_SPACER);
                if spacer
                    || flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
                    || cell.c == '\0'
                    || cell.c == '\t'
                {
                    text = " ".into();
                }
                let underline = if flags.contains(Flags::DOUBLE_UNDERLINE) {
                    2
                } else if flags.contains(Flags::UNDERCURL) {
                    3
                } else if flags.contains(Flags::DOTTED_UNDERLINE) {
                    4
                } else if flags.contains(Flags::DASHED_UNDERLINE) {
                    5
                } else if flags.contains(Flags::UNDERLINE) {
                    1
                } else {
                    0
                };
                cells.push(Cell {
                    text,
                    fg: alacritty_color(cell.fg, true),
                    bg: alacritty_color(cell.bg, false),
                    bold: flags.contains(Flags::BOLD),
                    dim: flags.contains(Flags::DIM),
                    italic: flags.contains(Flags::ITALIC),
                    underline,
                    inverse: flags.contains(Flags::INVERSE),
                    strike: flags.contains(Flags::STRIKEOUT),
                    hidden: flags.contains(Flags::HIDDEN),
                    wide: flags.contains(Flags::WIDE_CHAR),
                    spacer,
                    any: flags.contains(Flags::LEADING_WIDE_CHAR_SPACER),
                });
            }
        }
        let point = grid.cursor.point;
        Snapshot {
            rows,
            cols,
            cells,
            cursor: (point.line.0.max(0) as usize, point.column.0),
            cursor_visible: term.mode().contains(TermMode::SHOW_CURSOR),
        }
    }

    pub fn from_vt100(screen: &vt100::Screen) -> Self {
        let (rows, cols) = screen.size();
        let (rows, cols) = (rows as usize, cols as usize);
        let mut cells = Vec::with_capacity(rows * cols);
        for row in 0..rows {
            for col in 0..cols {
                let cell = screen.cell(row as u16, col as u16).unwrap();
                let text = if cell.has_contents() && !cell.is_wide_continuation() {
                    cell.contents().replace('\u{200b}', "")
                } else {
                    " ".to_string()
                };
                let text = if text.is_empty() {
                    " ".to_string()
                } else {
                    text
                };
                cells.push(Cell {
                    text,
                    fg: vt100_color(cell.fgcolor()),
                    bg: vt100_color(cell.bgcolor()),
                    bold: cell.bold(),
                    dim: cell.dim(),
                    italic: cell.italic(),
                    underline: cell.underline_style() as u8,
                    inverse: cell.inverse(),
                    strike: cell.strikethrough(),
                    hidden: cell.hidden(),
                    wide: cell.is_wide(),
                    spacer: cell.is_wide_continuation(),
                    any: false,
                });
            }
        }
        let (row, col) = screen.cursor_position();
        Snapshot {
            rows,
            cols,
            cells,
            // vt100 reports a pending wrap as one column past the edge.
            cursor: (row as usize, (col as usize).min(cols - 1)),
            cursor_visible: !screen.hide_cursor(),
        }
    }

    /// `None` when the two agree under `compare`, otherwise a report of every
    /// disagreeing cell (capped) with both screens printed.
    pub fn diff(&self, other: &Snapshot, compare: Compare) -> Option<String> {
        if (self.rows, self.cols) != (other.rows, other.cols) {
            return Some(format!(
                "size differs: {}x{} vs {}x{}",
                self.rows, self.cols, other.rows, other.cols
            ));
        }
        let mut report = String::new();
        let mut count = 0;
        for row in 0..self.rows {
            for col in 0..self.cols {
                let (left, right) = (self.cell(row, col), other.cell(row, col));
                if !cells_match(left, right, compare) {
                    count += 1;
                    if count <= 12 {
                        let _ = writeln!(report, "  ({row},{col}): {left:?}\n        vs {right:?}");
                    }
                }
            }
        }
        if compare.cursor
            && (self.cursor_visible != other.cursor_visible
                || (self.cursor_visible && self.cursor != other.cursor))
        {
            let _ = writeln!(
                report,
                "  cursor: {:?} visible={} vs {:?} visible={}",
                self.cursor, self.cursor_visible, other.cursor, other.cursor_visible
            );
            count += 1;
        }
        if count == 0 {
            return None;
        }
        let _ = writeln!(
            report,
            "  {count} difference(s)\n--- left\n{}\n--- right\n{}",
            self.render(),
            other.render()
        );
        Some(report)
    }

    /// The screen with a border, so trailing blanks are visible.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for row in 0..self.rows {
            out.push('|');
            out.push_str(&self.row_text(row));
            out.push_str("|\n");
        }
        out
    }
}

fn cells_match(left: &Cell, right: &Cell, compare: Compare) -> bool {
    if left.any || right.any {
        return !left.spacer && !right.spacer;
    }
    if left.spacer != right.spacer {
        return false;
    }
    if left.spacer {
        return true;
    }
    if left.text != right.text || left.wide != right.wide {
        return false;
    }
    if !compare.attributes {
        return true;
    }
    let blank = left.text == " ";
    if blank && !compare.blank_backgrounds {
        return true;
    }
    // A blank shows only its background (and inverse); foreground attributes
    // of a space are invisible, and emulators differ on keeping them.
    if blank {
        let visible = |cell: &Cell| {
            if cell.inverse {
                (cell.fg, true, cell.underline, cell.strike && compare.strike)
            } else {
                (
                    cell.bg,
                    false,
                    cell.underline,
                    cell.strike && compare.strike,
                )
            }
        };
        return visible(left) == visible(right);
    }
    left.fg == right.fg
        && left.bg == right.bg
        && left.bold == right.bold
        && left.dim == right.dim
        && left.italic == right.italic
        && left.underline == right.underline
        && left.inverse == right.inverse
        && (!compare.strike || left.strike == right.strike)
}

fn vt100_color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Default,
        vt100::Color::Idx(index) => Color::Idx(index),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn alacritty_color(color: AColor, foreground: bool) -> Color {
    match color {
        AColor::Named(named) => match named {
            NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground
                if foreground =>
            {
                Color::Default
            }
            NamedColor::Background if !foreground => Color::Default,
            NamedColor::Foreground | NamedColor::Background => Color::Default,
            other => {
                let index = other as usize;
                if index < 16 {
                    Color::Idx(index as u8)
                } else {
                    Color::Default
                }
            }
        },
        AColor::Indexed(index) => Color::Idx(index),
        AColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}
