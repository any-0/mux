//! A still copy of a pane's screen and scrollback, which is what Vim mode
//! moves around in.

use std::cell::OnceCell;

use crate::{
    config::Theme,
    frame::{CellAttributes, rgb},
    vim::{Position, VimMode},
};

pub(super) struct VimState {
    pub(super) mode: VimMode,
}

#[derive(Clone, Debug)]
pub(super) struct VimLine {
    pub(super) text: String,
    pub(super) cells: Vec<VimCell>,
}

#[derive(Clone, Debug)]
pub(super) struct VimCell {
    pub(super) attributes: CellAttributes,
    pub(super) text_start: u32,
    pub(super) text_length: u32,
    pub(super) character_start: Option<u32>,
    pub(super) character_length: u32,
    pub(super) wide_continuation: bool,
}

impl VimCell {
    pub(super) fn contents<'a>(&self, line: &'a str) -> &'a str {
        let start = self.text_start as usize;
        &line[start..start + self.text_length as usize]
    }

    pub(super) fn character_start(&self) -> Option<usize> {
        self.character_start.map(|start| start as usize)
    }
}

/// A pane's screen and scrollback, copied but not yet read.
///
/// Copying a row is cheap: the scrollback keeps its rows packed, and this
/// takes them as they are. Reading one means unpacking every cell in it, which
/// over a long scrollback costs more than a keypress can hide — so a line is
/// unpacked the first time something asks for it, and kept.
#[derive(Clone, Debug)]
pub(crate) struct VimBuffer {
    rows: Vec<vt100::Row>,
    cols: u16,
    lines: Vec<OnceCell<VimLine>>,
}

impl VimBuffer {
    fn new(rows: Vec<vt100::Row>, cols: u16) -> Self {
        let lines = rows.iter().map(|_| OnceCell::new()).collect();
        Self { rows, cols, lines }
    }

    /// An empty buffer, which is what a pane with nothing in it looks like.
    pub(crate) fn blank() -> Self {
        Self::new(vec![vt100::Row::new(0)], 0)
    }

    /// A buffer of plain text, for tests that care about motions rather than
    /// what the cells looked like.
    #[cfg(test)]
    pub(crate) fn from_text(texts: Vec<String>) -> Self {
        let lines = texts
            .into_iter()
            .map(|text| {
                let cells = text
                    .char_indices()
                    .map(|(index, character)| VimCell {
                        attributes: CellAttributes::default(),
                        text_start: index as u32,
                        text_length: character.len_utf8() as u32,
                        character_start: Some(text[..index].chars().count() as u32),
                        character_length: 1,
                        wide_continuation: false,
                    })
                    .collect();
                OnceCell::from(VimLine { text, cells })
            })
            .collect::<Vec<_>>();
        Self {
            rows: Vec::new(),
            cols: 0,
            lines,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.lines.len()
    }

    pub(crate) fn line(&self, row: usize) -> &VimLine {
        self.lines[row].get_or_init(|| snapshot_vim_row(&self.rows[row], self.cols))
    }

    pub(crate) fn text(&self, row: usize) -> &str {
        &self.line(row).text
    }

    pub(crate) fn get_text(&self, row: usize) -> Option<&str> {
        (row < self.len()).then(|| self.text(row))
    }

    /// Every line, unpacked as it is reached.
    pub(crate) fn lines(&self) -> impl Iterator<Item = &VimLine> {
        (0..self.len()).map(|row| self.line(row))
    }

    /// Every line's text, unpacked as it is reached. Whole-buffer work — a
    /// search, a yank of everything — pays for the lines it actually walks.
    pub(crate) fn texts(&self) -> impl Iterator<Item = &str> {
        (0..self.len()).map(|row| self.text(row))
    }
}

pub(super) fn snapshot_screen(screen: &mut vt100::Screen) -> (VimBuffer, Position) {
    let (_, cols) = screen.size();
    let history = screen.history_rows();
    let screen_cursor = screen.cursor_position();
    let buffer = VimBuffer::new(screen.all_rows().cloned().collect(), cols);
    // Taking the rows unpacks the blocks they were stored in. The copies are
    // the snapshot's now, so the pane drops what it decoded to hand them over.
    screen.set_scrollback(0);
    let cursor_row = history + screen_cursor.0 as usize;
    let cursor_col = vim_character_column(buffer.line(cursor_row), screen_cursor.1 as usize);
    let cursor = Position {
        row: cursor_row,
        col: cursor_col,
    };
    (buffer, cursor)
}

pub(super) fn snapshot_vim_line(screen: &vt100::Screen, row: u16, cols: u16) -> VimLine {
    let cells: Vec<_> = screen
        .row_cells(row)
        .take(screen.row_used_cells(row) as usize)
        .collect();
    vim_line_from_cells(&cells, cols)
}

/// Unpacks one stored row into the line Vim mode reads.
fn snapshot_vim_row(row: &vt100::Row, cols: u16) -> VimLine {
    // Everything past the row's used cells is blank, and unpacking it is the
    // bulk of the cost of reading a long scrollback.
    let cells: Vec<_> = row.cells().take(row.used_cells() as usize).collect();
    vim_line_from_cells(&cells, cols)
}

fn vim_line_from_cells(cells: &[vt100::Cell], cols: u16) -> VimLine {
    let last_content = cells
        .iter()
        .rposition(|cell| !cell.is_wide_continuation() && cell.has_contents());
    let last_cell = cells.iter().rposition(|cell| {
        cell.has_contents()
            || cell.is_wide_continuation()
            || CellAttributes::from(cell) != CellAttributes::default()
    });
    let cell_count = last_cell.map_or(0, |col| col + 1).min(usize::from(cols));
    let mut text = String::with_capacity(cell_count);
    let mut snapshot_cells = Vec::with_capacity(cell_count);
    let mut character_col = 0;
    for (col, cell) in cells.iter().take(cell_count).enumerate() {
        let wide_continuation = cell.is_wide_continuation();
        let in_text = last_content.is_some_and(|last| col <= last) && !wide_continuation;
        let text_start = text.len();
        let character_length = if in_text {
            if cell.has_contents() {
                text.push_str(cell.contents());
                cell.contents().chars().count()
            } else {
                text.push(' ');
                1
            }
        } else {
            0
        };
        let character_start = in_text.then_some(character_col as u32);
        character_col += character_length;
        snapshot_cells.push(VimCell {
            attributes: CellAttributes::from(cell),
            text_start: text_start as u32,
            text_length: (text.len() - text_start) as u32,
            character_start,
            character_length: character_length as u32,
            wide_continuation,
        });
    }
    VimLine {
        text,
        cells: snapshot_cells,
    }
}

fn vim_character_column(line: &VimLine, terminal_col: usize) -> usize {
    if let Some(start) = line
        .cells
        .get(terminal_col)
        .and_then(VimCell::character_start)
    {
        return start;
    }
    if terminal_col > 0
        && let Some(start) = line
            .cells
            .get(terminal_col - 1)
            .and_then(VimCell::character_start)
    {
        return start;
    }
    line.text.chars().count()
}

pub(super) fn vim_cursor_column(line: &VimLine, character_col: usize) -> usize {
    line.cells
        .iter()
        .position(|cell| {
            cell.character_start()
                .is_some_and(|start| character_col < start + cell.character_length as usize)
        })
        .unwrap_or(0)
}

pub(super) fn vim_selected_cell_attributes(
    attributes: CellAttributes,
    theme: &Theme,
) -> CellAttributes {
    attributes.with_background(theme.vim_selection)
}

pub(super) fn vim_jump_cell_attributes(
    mut attributes: CellAttributes,
    theme: &Theme,
) -> CellAttributes {
    attributes.foreground = rgb(theme.bar_label_foreground);
    attributes.background = rgb(theme.vim_jump);
    attributes.bold = true;
    attributes
}

/// Search matches keep their own colors so the terminal's formatting stays
/// readable underneath; the match the cursor sits on is brighter, like `hlsearch`
/// against `incsearch`.
pub(super) fn vim_search_cell_attributes(
    mut attributes: CellAttributes,
    current: bool,
    theme: &Theme,
) -> CellAttributes {
    if current {
        attributes.foreground = rgb(theme.bar_label_foreground);
        attributes.background = rgb(theme.vim_search_current);
        attributes.bold = true;
    } else {
        attributes.background = rgb(theme.vim_search);
    }
    attributes
}
