//! A still copy of a pane's screen and scrollback, which is what Vim mode
//! moves around in.

use crate::{
    config::Theme,
    frame::{CellAttributes, rgb},
    vim::{Position, VimMode},
};

pub(super) struct VimState {
    pub(super) mode: VimMode,
    pub(super) lines: Vec<VimLine>,
}

pub(super) struct VimLine {
    pub(super) text: String,
    pub(super) cells: Vec<VimCell>,
}

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

pub(super) fn snapshot_screen(screen: &mut vt100::Screen) -> (Vec<VimLine>, Position) {
    screen.set_scrollback(usize::MAX);
    let history = screen.scrollback();
    let (rows, cols) = screen.size();
    let screen_cursor = screen.cursor_position();
    let total = history + rows as usize;
    let mut lines = Vec::with_capacity(total);
    let mut absolute = 0;
    while absolute < total {
        let offset = history.saturating_sub(absolute);
        screen.set_scrollback(offset);
        let top_absolute = history - offset;
        let skip = absolute - top_absolute;
        let available = (rows as usize - skip).min(total - absolute);
        for row in skip..skip + available {
            lines.push(snapshot_vim_line(screen, row as u16, cols));
        }
        absolute += available;
    }
    screen.set_scrollback(0);
    let cursor_row = history + screen_cursor.0 as usize;
    let cursor_col = vim_character_column(&lines[cursor_row], screen_cursor.1 as usize);
    let cursor = Position {
        row: cursor_row,
        col: cursor_col,
    };
    (lines, cursor)
}

pub(super) fn snapshot_vim_line(screen: &vt100::Screen, row: u16, cols: u16) -> VimLine {
    let cells: Vec<_> = screen.row_cells(row).collect();
    let last_content = cells
        .iter()
        .rposition(|cell| !cell.is_wide_continuation() && cell.has_contents());
    let last_cell = cells.iter().rposition(|cell| {
        cell.has_contents()
            || cell.is_wide_continuation()
            || CellAttributes::from(cell) != CellAttributes::default()
    });
    let cell_count = last_cell.map_or(0, |col| col + 1).min(usize::from(cols));
    let mut text = String::new();
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
