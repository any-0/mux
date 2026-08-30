//! The pieces mux paints for itself: the bar, the session tree, popups, and
//! the previews that show a pane inside one of them.

use std::{
    collections::HashSet,
    env,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::{
    config::{BellStyle, Theme},
    frame::{CellAttributes, CursorShape, Frame, FrameCursor, Rgb},
};

use super::{
    Session, Window,
    bell::{BellLabel, bell_visual, render_bell_label},
    layout::Rect,
};

/// How long a transient status message stays on screen.
pub(super) const MESSAGE_DURATION: Duration = Duration::from_millis(1_600);

pub(super) struct TreeState {
    pub(super) selected: usize,
    pub(super) expanded: HashSet<usize>,
}

/// A pending `[y/N]` question.
///
/// Only identifiers are held: what the prompt says is worked out when it is
/// drawn, so a pane that moved or a session that was renamed while the question
/// was on screen is still described correctly.
pub(super) enum Confirmation {
    KillPane { pane_id: usize },
    KillSession { session_id: usize },
}

pub(super) struct RenameState {
    pub(super) target: RenameTarget,
    pub(super) text: String,
    pub(super) cursor: usize,
}

/// What a rename popup is editing.
#[derive(Clone, Copy)]
pub(super) enum RenameTarget {
    Session {
        session_id: usize,
    },
    /// Windows are identified by position, which is where they are shown from.
    Window {
        session_id: usize,
        window_index: usize,
    },
}

impl RenameState {
    pub(super) fn insert(&mut self, character: char) {
        let byte = character_byte_index(&self.text, self.cursor);
        self.text.insert(byte, character);
        self.cursor += 1;
    }

    pub(super) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = character_byte_index(&self.text, self.cursor - 1);
        let end = character_byte_index(&self.text, self.cursor);
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub(super) fn delete(&mut self) {
        let end = self.text.chars().count();
        if self.cursor == end {
            return;
        }
        let start = character_byte_index(&self.text, self.cursor);
        let end = character_byte_index(&self.text, self.cursor + 1);
        self.text.replace_range(start..end, "");
    }

    pub(super) fn delete_word_before_cursor(&mut self) {
        while self.cursor > 0
            && self
                .text
                .chars()
                .nth(self.cursor - 1)
                .is_some_and(char::is_whitespace)
        {
            self.backspace();
        }
        while self.cursor > 0
            && self
                .text
                .chars()
                .nth(self.cursor - 1)
                .is_some_and(|character| !character.is_whitespace())
        {
            self.backspace();
        }
    }

    pub(super) fn delete_before_cursor(&mut self) {
        let end = character_byte_index(&self.text, self.cursor);
        self.text.replace_range(..end, "");
        self.cursor = 0;
    }

    pub(super) fn delete_after_cursor(&mut self) {
        let start = character_byte_index(&self.text, self.cursor);
        self.text.truncate(start);
    }
}

impl TreeState {
    pub(super) fn folded(selected: usize) -> Self {
        Self {
            selected,
            expanded: HashSet::new(),
        }
    }
}

/// A transient status line that disappears on its own.
pub(super) struct StatusMessage {
    pub(super) text: String,
    pub(super) expires: Instant,
}

impl StatusMessage {
    pub(super) fn new(text: String) -> Self {
        Self {
            text,
            expires: Instant::now() + MESSAGE_DURATION,
        }
    }

    pub(super) fn expired(&self, now: Instant) -> bool {
        self.expires <= now
    }
}

#[derive(Clone)]
pub(super) struct TreeItem {
    pub(super) session_id: usize,
    pub(super) window: Option<usize>,
    pub(super) pane: Option<usize>,
    pub(super) label: String,
}

#[derive(Clone, Copy)]
pub(super) enum TreeRowKind {
    Primary,
    Root,
}

pub(super) struct TreeDisplayRow {
    pub(super) item_index: usize,
    pub(super) kind: TreeRowKind,
}

pub(super) enum Popup {
    Status(String),
    Warning(String),
    Rename {
        text: String,
        cursor: usize,
        shape: CursorShape,
    },
}

/// Where a popup sits. Anything that asks a question or takes input holds the
/// middle of the screen; anything that only reports keeps out of the way at the
/// bottom, where it reads as chrome rather than as an interruption.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum PopupAnchor {
    Center,
    Bottom,
}

/// A pane that has been closed under the question leaves it unnumbered rather
/// than naming whichever pane took its place.
pub(super) fn kill_pane_prompt(number: Option<usize>) -> String {
    match number {
        Some(number) => format!("kill pane {number}? [y/N]"),
        None => "kill pane? [y/N]".into(),
    }
}

pub(super) fn kill_session_prompt(session: Option<(&str, usize)>) -> String {
    match session {
        Some((name, panes)) => format!(
            "kill session {name:?} and its {panes} pane{}? [y/N]",
            if panes == 1 { "" } else { "s" }
        ),
        None => "kill session? [y/N]".into(),
    }
}

/// Paints the slice of `screen` starting at `source_top` into `destination`,
/// clipped to whatever the screen actually has.
pub(super) fn render_screen_region(
    frame: &mut Frame,
    screen: &vt100::Screen,
    source_top: u16,
    destination: Rect,
) {
    let (screen_rows, screen_cols) = screen.size();
    let width = destination.cols.min(screen_cols);
    let height = destination.rows.min(screen_rows.saturating_sub(source_top));
    for offset in 0..height {
        let row = destination.row + offset;
        for col in 0..width {
            let Some(cell) = screen.cell(source_top + offset, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let attributes = CellAttributes::from(&cell);
            let contents = if cell.has_contents() {
                cell.contents()
            } else {
                " "
            };
            let at = destination.col + col;
            if cell.is_wide() {
                frame.set_wide_cell(row, at, contents, attributes);
            } else {
                frame.set_cell(row, at, contents, attributes);
            }
        }
    }
}

/// Where the cursor belongs inside a preview of `screen`.
///
/// The tree has no cursor of its own, so it borrows the previewed pane's:
/// clamped into the slice actually on screen, it lands where that pane's cursor
/// would be if it were focused instead of on whichever panel row painted last.
pub(super) fn preview_cursor(
    screen: &vt100::Screen,
    shape: CursorShape,
    source_top: u16,
    destination: Rect,
) -> Option<FrameCursor> {
    let (screen_rows, screen_cols) = screen.size();
    let width = destination.cols.min(screen_cols);
    let height = destination.rows.min(screen_rows.saturating_sub(source_top));
    if width == 0 || height == 0 {
        return None;
    }
    let (row, col) = screen.cursor_position();
    let row = row.clamp(source_top, source_top + height - 1);
    Some(FrameCursor {
        row: destination.row + (row - source_top),
        col: destination.col + col.min(width - 1),
        shape,
        visible: !screen.hide_cursor(),
    })
}

pub(super) fn preview_source_region(screen: &vt100::Screen, maximum_height: u16) -> (u16, u16) {
    let (rows, cols) = screen.size();
    let last_content = (0..rows).rev().find(|row| {
        (0..cols).any(|col| {
            screen
                .cell(*row, col)
                .is_some_and(|cell| !cell.is_wide_continuation() && cell.has_contents())
        })
    });
    let anchor = last_content
        .unwrap_or(0)
        .max(screen.cursor_position().0.min(rows.saturating_sub(1)));
    let height = maximum_height.min(anchor + 1).min(rows);
    (anchor + 1 - height, height)
}

pub(super) fn tree_shortcut(index: usize) -> Option<String> {
    if index < 9 {
        Some((index + 1).to_string())
    } else if index == 9 {
        Some("0".into())
    } else if index < 35 {
        let letter = char::from(b'a' + (index - 9) as u8);
        (letter != 's').then(|| format!("M-{letter}"))
    } else {
        None
    }
}

fn character_byte_index(text: &str, character: usize) -> usize {
    text.char_indices()
        .nth(character)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

pub(super) fn tree_display_rows(items: &[TreeItem]) -> Vec<TreeDisplayRow> {
    let mut rows = Vec::new();
    for (item_index, item) in items.iter().enumerate() {
        rows.push(TreeDisplayRow {
            item_index,
            kind: TreeRowKind::Primary,
        });
        if item.window.is_none() {
            rows.push(TreeDisplayRow {
                item_index,
                kind: TreeRowKind::Root,
            });
        }
    }
    rows
}

pub(super) fn tree_first_display_row(
    display_rows: &[TreeDisplayRow],
    selected: usize,
    list_height: usize,
) -> usize {
    let selected_row = display_rows
        .iter()
        .position(|row| row.item_index == selected && matches!(row.kind, TreeRowKind::Primary))
        .unwrap_or(0);
    let selected_end = selected_row
        + usize::from(
            display_rows
                .get(selected_row + 1)
                .is_some_and(|row| row.item_index == selected),
        );
    let mut first_row = selected_row.saturating_sub(list_height / 2);
    if selected_end >= first_row.saturating_add(list_height) {
        first_row = selected_end + 1 - list_height;
    }
    first_row.min(display_rows.len().saturating_sub(list_height))
}

pub(super) fn render_preview_window_title(
    frame: &mut Frame,
    session: &Session,
    window_index: usize,
    window: &Window,
    rect: Rect,
    theme: &Theme,
    bell_style: BellStyle,
) {
    if rect.rows == 0 || rect.cols == 0 {
        return;
    }
    let title = match window.label() {
        Some(label) => format!(" window {}  ·  {label}", window_index + 1),
        None => format!(
            " window {}  ·  {} pane{}",
            window_index + 1,
            window.panes.len(),
            if window.panes.len() == 1 { "" } else { "s" }
        ),
    };
    let title = truncate(&title, rect.cols as usize);
    let line = format!(
        "{title}{}",
        " ".repeat(rect.cols as usize - title.chars().count())
    );
    let background = if window_index == session.current_window {
        theme.bar_active
    } else {
        theme.bar_inactive
    };
    let foreground = theme.bar_label_foreground;
    if let Some(visual) = window
        .bell
        .as_ref()
        .and_then(|bell| bell_visual(bell, bell_style))
    {
        render_bell_label(
            frame,
            (rect.row, rect.col),
            &line,
            BellLabel {
                visual,
                animation_width: rect.cols as usize,
                resting: (background, foreground),
                bold: true,
            },
            theme,
        );
    } else {
        frame.set_text(
            rect.row,
            rect.col,
            &line,
            CellAttributes::colors(foreground, background).bold(),
        );
    }
}

pub(super) fn tree_window_line(shortcut: &str, label: &str) -> String {
    format!(" {shortcut:>2}      ├─ {label}")
}

pub(super) fn compact_path(path: &Path) -> String {
    if let Some(home) = env::var_os("HOME").map(PathBuf::from)
        && let Ok(relative) = path.strip_prefix(&home)
    {
        if relative.as_os_str().is_empty() {
            return "~".into();
        }
        return format!("~/{}", relative.display());
    }
    path.display().to_string()
}

pub(super) fn two_sided_line(left: &str, right: &str, width: usize) -> String {
    let left_width = left.chars().count();
    let right_width = right.chars().count();
    if left_width + right_width >= width {
        return truncate(left, width);
    }
    format!(
        "{left}{}{right}",
        " ".repeat(width - left_width - right_width)
    )
}

/// Draws a bordered popup, horizontally centred and anchored vertically.
///
/// Only a popup with an input field claims the cursor. A message or a question
/// leaves it where the pane behind it put it, so a notice appearing does not
/// pull the cursor out of the text being typed.
pub(super) fn render_popup_box(
    frame: &mut Frame,
    (rows, cols): (u16, u16),
    anchor: PopupAnchor,
    popup: &Popup,
    theme: &Theme,
) {
    let (text, cursor, cursor_shape, warning) = match popup {
        Popup::Status(text) => (text.as_str(), None, CursorShape::Block, false),
        Popup::Warning(text) => (text.as_str(), None, CursorShape::Block, true),
        Popup::Rename {
            text,
            cursor,
            shape,
        } => (text.as_str(), Some(*cursor), *shape, false),
    };
    if rows < 3 || cols < 6 {
        let width = cols as usize;
        let (visible, cursor) = popup_text_window(text, cursor, width);
        let row = match anchor {
            PopupAnchor::Center => rows.saturating_add(1) / 2,
            PopupAnchor::Bottom => rows,
        };
        frame.set_text(
            row,
            1,
            &visible,
            CellAttributes::colors(theme.popup_text, theme.popup_background),
        );
        if let Some(cursor) = cursor {
            frame.set_cursor(FrameCursor {
                row,
                col: cursor as u16 + 1,
                shape: cursor_shape,
                visible: true,
            });
        }
        return;
    }

    let cursor_cell = usize::from(cursor.is_some());
    let width = (text.chars().count() + 4 + cursor_cell)
        .max(10)
        .min(cols as usize);
    let inner_width = width - 2;
    let text_width = inner_width - 2;
    let (visible, cursor) = popup_text_window(text, cursor, text_width);
    let padding = text_width.saturating_sub(visible.chars().count());
    let left = ((cols as usize - width) / 2 + 1) as u16;
    let top = match anchor {
        PopupAnchor::Center => ((rows as usize - 3) / 2 + 1) as u16,
        // Flush with the last row, so the box sits under the content rather
        // than across it.
        PopupAnchor::Bottom => rows - 2,
    };
    let accent = CellAttributes::colors(
        if warning {
            theme.popup_warning
        } else {
            theme.popup_accent
        },
        theme.popup_background,
    );
    let body = CellAttributes::colors(theme.popup_text, theme.popup_background);

    frame.set_text(top, left, &format!("╭{}╮", "─".repeat(inner_width)), accent);
    frame.set_text(
        top + 2,
        left,
        &format!("╰{}╯", "─".repeat(inner_width)),
        accent,
    );
    frame.set_text(top + 1, left, "│ ", accent);
    let after_text = frame.set_text(top + 1, left + 2, &visible, body);
    frame.fill(top + 1, after_text, padding as u16, body);
    frame.set_text(top + 1, left + 1 + inner_width as u16 - 1, " │", accent);

    if let Some(cursor) = cursor {
        frame.set_cursor(FrameCursor {
            row: top + 1,
            col: left + cursor as u16 + 2,
            shape: cursor_shape,
            visible: true,
        });
    }
}

pub(super) fn popup_text_window(
    text: &str,
    cursor: Option<usize>,
    width: usize,
) -> (String, Option<usize>) {
    let characters: Vec<_> = text.chars().collect();
    let Some(cursor) = cursor else {
        return (characters.into_iter().take(width).collect(), None);
    };
    let cursor = cursor.min(characters.len());
    let start = if cursor < width {
        0
    } else {
        cursor + 1 - width
    };
    let visible = characters.into_iter().skip(start).take(width).collect();
    (visible, Some(cursor - start))
}

pub(super) fn truncate(value: &str, width: usize) -> String {
    value.chars().take(width).collect()
}

pub(super) fn bar_width(window_count: usize) -> u16 {
    let digits = window_count.max(1).ilog10() as u16 + 1;
    digits + 4
}

pub(super) fn render_bar_separator(
    frame: &mut Frame,
    rows: u16,
    bar_width: u16,
    current_row: Option<u16>,
    color: Rgb,
) {
    let attributes = CellAttributes::foreground(color);
    for row in 1..=rows {
        let glyph = if Some(row) == current_row {
            "\u{e010}"
        } else if current_row.is_some_and(|active_row| row == active_row + 1) {
            "\u{e018}"
        } else {
            "\u{e011}"
        };
        frame.set_cell(row, bar_width - 1, glyph, attributes);
    }
}

pub(super) fn centered_bar_layout(
    window_count: usize,
    current_window: usize,
    available_rows: usize,
) -> (usize, usize, usize) {
    let visible = window_count.min(available_rows / 3);
    let first_window = if window_count <= visible {
        0
    } else {
        current_window
            .saturating_sub(visible / 2)
            .min(window_count - visible)
    };
    let first_row = (available_rows - visible * 3) / 2;
    (first_window, first_row, visible)
}

pub(super) fn bar_label(window_number: usize, number_width: usize) -> String {
    bar_text_label(&window_number.to_string(), number_width)
}

fn bar_text_label(text: &str, number_width: usize) -> String {
    format!(" {text:^number_width$} ")
}

pub(super) fn bar_window_label(
    window: usize,
    current_window: usize,
    number_width: usize,
) -> String {
    if window == current_window {
        bar_text_label("•", number_width)
    } else {
        bar_label(window + 1, number_width)
    }
}

/// The colour of the tile behind the state dot at the top of the bar.
///
/// One tile carries the whole mode: the bar's own colour while mux is watching
/// for its bindings, grey once a second leader has handed the next key over
/// wholesale, and a colour of its own for each mode that intercepts keys.
pub(super) fn state_colors(
    passthrough: bool,
    leader: bool,
    vim: bool,
    theme: &Theme,
) -> (Rgb, Rgb) {
    let tile = if passthrough {
        theme.state_passthrough
    } else if leader {
        theme.state_leader
    } else if vim {
        theme.state_vim
    } else {
        return (theme.state_normal, theme.state_normal_dot);
    };
    (tile, theme.bar_label_foreground)
}

pub(super) fn other_session_bell_label(count: usize) -> String {
    if count == 1 {
        " ! ".into()
    } else {
        format!("{:^3}", count.min(999))
    }
}

pub(super) fn tree_panel_width(available_width: u16) -> u16 {
    if available_width <= 48 {
        (available_width / 2).max(1)
    } else {
        (available_width / 3).clamp(24, 38)
    }
}
