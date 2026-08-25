//! Painting one client's screen.
//!
//! Everything here reads the daemon's state and writes cells into a [`Frame`];
//! what actually reaches the terminal is the diff against the frame that client
//! is already showing.

use anyhow::{Context, Result};

use crate::{
    config::{BellStyle, Theme},
    frame::{CellAttributes, CursorShape, Frame, FrameCursor},
    protocol::ServerMessage,
};

use super::*;

impl Server {
    /// Paints every attached client and sends each one only what changed.
    pub(super) fn render_all(&mut self) {
        let ids: Vec<_> = self
            .clients
            .iter()
            .filter_map(|(id, client)| client.initialized.then_some(*id))
            .collect();
        let mut output = Vec::new();
        for id in ids {
            let Some(client) = self.clients.get_mut(&id) else {
                continue;
            };
            let (rows, cols) = (client.rows.max(1), client.cols.max(1));
            let mut frame = std::mem::take(&mut client.scratch);
            frame.reset(rows, cols);
            let result = self.render(id, &mut frame);
            let Some(client) = self.clients.get_mut(&id) else {
                continue;
            };
            let message = match result {
                Ok(()) => {
                    output.clear();
                    frame.diff(&client.frame, client.colors, &mut output);
                    client.scratch = std::mem::replace(&mut client.frame, frame);
                    if output.is_empty() {
                        continue;
                    }
                    ServerMessage::Render(std::mem::take(&mut output))
                }
                Err(error) => {
                    client.scratch = frame;
                    ServerMessage::Error(format!("{error:#}"))
                }
            };
            if !client.writer.send(message) {
                // Too far behind to take this diff. Forget what it was showing
                // so the next frame repaints in full rather than patching a
                // screen that never received the frames in between.
                client.frame = Frame::default();
            }
        }
    }

    fn render(&mut self, id: usize, frame: &mut Frame) -> Result<()> {
        let vim_active = self.vim_active(id);
        let client = self.clients.get(&id).context("unknown client")?;
        let cols = frame.cols();
        let rows = frame.rows();
        let has_vim_panes = !client.vim.is_empty();
        let tree_active = client.tree.is_some();
        let bar_width = if tree_active {
            0
        } else {
            self.active_bar_width(id)
        };
        if tree_active {
            self.render_tree(id, frame, rows, cols, bar_width);
        } else {
            self.render_bar(id, frame, rows, vim_active, bar_width);
            self.render_terminal(id, frame, bar_width)?;
            if has_vim_panes {
                self.render_vim(id, frame, bar_width)?;
            }
        }
        // The picker is a dialog: it covers the screen it was opened from
        // rather than replacing it, so the panes stay in view around it.
        if let Some(picker) = self.clients[&id].themes.as_ref() {
            render_theme_picker(picker, frame, rows, cols);
            frame.set_cursor(FrameCursor::default());
        }
        self.render_popup(id, frame, rows, cols);
        Ok(())
    }

    /// Describes a pending question using the state as it stands now.
    fn confirmation_prompt(&self, confirmation: &Confirmation) -> String {
        match confirmation {
            Confirmation::KillPane { pane_id } => kill_pane_prompt(self.pane_number(*pane_id)),
            Confirmation::KillSession { session_id } => {
                let session = self
                    .sessions
                    .iter()
                    .find(|session| session.id == *session_id);
                kill_session_prompt(session.map(|session| {
                    (
                        session.name.as_str(),
                        session
                            .windows
                            .iter()
                            .map(|window| window.panes.len())
                            .sum::<usize>(),
                    )
                }))
            }
        }
    }

    /// Where a pane sits in its window, counting from one.
    fn pane_number(&self, pane_id: usize) -> Option<usize> {
        self.sessions
            .iter()
            .flat_map(|session| &session.windows)
            .find_map(|window| {
                window
                    .panes
                    .iter()
                    .position(|pane| pane.id == pane_id)
                    .map(|index| index + 1)
            })
    }

    fn render_popup(&self, id: usize, frame: &mut Frame, rows: u16, cols: u16) {
        let active_vim_pane = self.active_vim_pane_id(id);
        let cursor_shape = self
            .active_pane(id)
            .map(|pane| pane.parser.callbacks().cursor_shape)
            .unwrap_or_default();
        let client = &self.clients[&id];
        let theme = client.rendered_theme();
        let popup = if let Some(confirmation) = &client.confirmation {
            Some((
                Popup::Warning(self.confirmation_prompt(confirmation)),
                PopupAnchor::Center,
            ))
        } else if let Some(rename) = &client.rename {
            let prefix = match rename.target {
                RenameTarget::Session { .. } => "rename session: ",
                RenameTarget::Window { .. } => "rename window: ",
            };
            Some((
                Popup::Rename {
                    text: format!("{prefix}{}", rename.text),
                    cursor: prefix.chars().count() + rename.cursor,
                    shape: cursor_shape,
                },
                PopupAnchor::Center,
            ))
        } else if client.leader {
            Some((
                Popup::Status(
                    "leader: $ session · , window · -/| split · z zoom · b bell · x kill · d detach · arrows focus · ctrl-arrows resize"
                        .into(),
                ),
                PopupAnchor::Bottom,
            ))
        } else if let Some(prompt) = active_vim_pane
            .and_then(|pane_id| client.vim.get(&pane_id))
            .and_then(|state| state.mode.prompt())
        {
            Some((Popup::Status(prompt), PopupAnchor::Center))
        } else {
            client.message.as_ref().map(|message| {
                (
                    Popup::Status(message.text.clone()),
                    // Nothing to answer: this only confirms something already
                    // done, so it stays out of the middle of the screen.
                    PopupAnchor::Bottom,
                )
            })
        };
        let Some((popup, anchor)) = popup else {
            return;
        };
        render_popup_box(frame, (rows, cols), anchor, &popup, &theme);
    }

    fn render_bar(
        &self,
        id: usize,
        frame: &mut Frame,
        rows: u16,
        vim_active: bool,
        bar_width: u16,
    ) {
        let theme = self.clients[&id].rendered_theme();
        let bell_style = self.clients[&id].bell_style;
        let normal_rgb = theme.bar_inactive;
        let current_rgb = theme.bar_active;
        let active = self.active_indices(id);
        let windows = active
            .map(|(session, _)| self.sessions[session].windows.len())
            .unwrap_or(0);
        let session_index = active.map(|(session, _)| session);
        let current_window = active.map(|(_, window)| window).unwrap_or(0);
        let (first_window, first_row, visible) =
            centered_bar_layout(windows, current_window, rows.saturating_sub(2) as usize);
        let current_row = (visible > 0)
            .then_some((first_row + current_window.saturating_sub(first_window) * 3 + 2) as u16);
        let number_width = bar_width.saturating_sub(4) as usize;
        let label_width = number_width + 2;
        let active_animation_width = label_width + 1;
        let separator_rgb = active
            .and_then(|(session, window)| self.sessions[session].windows[window].bell.as_ref())
            .and_then(|bell| bell_visual(bell, bell_style))
            .map(|visual| {
                bell_cell_colors(
                    visual,
                    label_width,
                    active_animation_width,
                    (current_rgb, theme.bar_label_foreground),
                    &theme,
                )
                .0
            })
            .unwrap_or(current_rgb);
        render_bar_separator(frame, rows, bar_width, current_row, separator_rgb);
        let client = &self.clients[&id];
        let (tile, dot) = state_colors(client.literal, client.leader, vim_active, &theme);
        frame.set_text(1, 1, " ● ", CellAttributes::colors(dot, tile));
        for offset in 0..visible {
            let window = first_window + offset;
            let row = (first_row + offset * 3) as u16 + 2;
            let zoomed =
                active.is_some_and(|(session, _)| self.sessions[session].windows[window].zoomed);
            let label = bar_window_label(window, current_window, number_width, zoomed);
            let bell = active
                .and_then(|(session, _)| self.sessions[session].windows[window].bell.as_ref())
                .and_then(|bell| bell_visual(bell, bell_style));
            let background = if window == current_window {
                current_rgb
            } else {
                normal_rgb
            };
            if let Some(visual) = bell {
                let animation_width = if window == current_window {
                    active_animation_width
                } else {
                    label_width
                };
                render_bell_label(
                    frame,
                    (row, 1),
                    &label,
                    BellLabel {
                        visual,
                        animation_width,
                        resting: (background, theme.bar_label_foreground),
                        bold: false,
                    },
                    &theme,
                );
            } else {
                frame.set_text(
                    row,
                    1,
                    &label,
                    CellAttributes::colors(theme.bar_label_foreground, background),
                );
            }
            let icon = session_index
                .map(|session| self.sessions[session].windows[window].active_process_icon())
                .unwrap_or(IDLE_ICON);
            let icon_label = if icon.chars().count() == 3 {
                icon.to_owned()
            } else {
                format!(" {icon} ")
            };
            let icon_row = row + 1;
            if let Some(visual) = bell {
                render_bell_label(
                    frame,
                    (icon_row, 1),
                    &icon_label,
                    BellLabel {
                        visual,
                        animation_width: if window == current_window {
                            active_animation_width
                        } else {
                            label_width
                        },
                        resting: (background, theme.bar_label_foreground),
                        bold: false,
                    },
                    &theme,
                );
            } else {
                let attributes = CellAttributes::colors(theme.bar_label_foreground, background);
                frame.set_text(icon_row, 1, &icon_label, attributes);
            }
        }
        if let Some((count, bell)) = self.other_session_bells(id)
            && let Some(visual) = bell_visual(bell, bell_style)
        {
            render_bell_label(
                frame,
                (rows, 1),
                &other_session_bell_label(count),
                BellLabel {
                    visual,
                    animation_width: 3,
                    resting: (normal_rgb, theme.bar_label_foreground),
                    bold: false,
                },
                &theme,
            );
        }
    }

    fn render_terminal(&mut self, id: usize, frame: &mut Frame, bar_width: u16) -> Result<()> {
        let (session_index, window_index) = self.active_indices(id).context("no active session")?;
        let area = self.content_area(id);
        let window = &self.sessions[session_index].windows[window_index];
        let (regions, dividers) = window.regions(area);
        for pane in &window.panes {
            // A zoomed window hides every pane but the active one.
            let Some(rect) = regions
                .iter()
                .find_map(|(pane_id, rect)| (*pane_id == pane.id).then_some(*rect))
            else {
                continue;
            };
            if rect.rows == 0 || rect.cols == 0 {
                continue;
            }
            render_screen_region(
                frame,
                pane.parser.screen(),
                0,
                Rect {
                    row: rect.row + 1,
                    col: bar_width + rect.col + 1,
                    ..rect
                },
            );
        }
        render_dividers(
            frame,
            &dividers,
            bar_width,
            CellAttributes::foreground(self.clients[&id].rendered_theme().divider),
        );

        self.render_terminal_cursor(id, frame, bar_width, &regions)
    }

    fn render_terminal_cursor(
        &self,
        id: usize,
        frame: &mut Frame,
        bar_width: u16,
        regions: &[(usize, Rect)],
    ) -> Result<()> {
        let (session_index, window_index) = self.active_indices(id).context("no active session")?;
        let window = &self.sessions[session_index].windows[window_index];
        let active_pane = window
            .panes
            .iter()
            .find(|pane| pane.id == window.active_pane)
            .context("active pane missing from window")?;
        let rect = regions
            .iter()
            .find_map(|(pane_id, rect)| (*pane_id == active_pane.id).then_some(*rect))
            .context("active pane missing from layout")?;
        if rect.rows > 0 && rect.cols > 0 {
            let screen = active_pane.parser.screen();
            let (row, col) = screen.cursor_position();
            // A pane that hides its cursor still says where it is. The position
            // is kept so the terminal's cursor rests in the pane rather than on
            // the bar that painted after it.
            frame.set_cursor(FrameCursor {
                row: rect.row + row.min(rect.rows - 1) + 1,
                col: bar_width + rect.col + col.min(rect.cols - 1) + 1,
                shape: active_pane.parser.callbacks().cursor_shape,
                visible: !screen.hide_cursor(),
            });
        }
        Ok(())
    }

    fn render_vim(&self, id: usize, frame: &mut Frame, bar_width: u16) -> Result<()> {
        let Some((session_index, window_index)) = self.active_indices(id) else {
            return Ok(());
        };
        let area = self.content_area(id);
        let window = &self.sessions[session_index].windows[window_index];
        let (regions, _) = window.regions(area);
        let client = &self.clients[&id];
        let theme = client.rendered_theme();
        for (pane_id, rect) in &regions {
            if *pane_id != window.active_pane
                && let Some(state) = client.vim.get(pane_id)
            {
                Self::render_vim_region(state, frame, *rect, bar_width, false, &theme);
            }
        }
        if let Some(state) = client.vim.get(&window.active_pane) {
            let rect = regions
                .iter()
                .find_map(|(pane_id, rect)| (*pane_id == window.active_pane).then_some(*rect))
                .context("active pane missing from layout")?;
            Self::render_vim_region(state, frame, rect, bar_width, true, &theme);
        } else {
            self.render_terminal_cursor(id, frame, bar_width, &regions)?;
        }
        Ok(())
    }

    pub(super) fn render_vim_region(
        state: &VimState,
        frame: &mut Frame,
        rect: Rect,
        bar_width: u16,
        active: bool,
        theme: &Theme,
    ) {
        if rect.rows == 0 || rect.cols == 0 {
            return;
        }
        let vim = &state.mode;
        let width = rect.cols as usize;
        for screen_row in 0..rect.rows as usize {
            let buffer_row = vim.viewport_top + screen_row;
            let row = rect.row + screen_row as u16 + 1;
            let left = bar_width + rect.col + 1;
            let mut skip_until = 0;
            let Some(line) = state.lines.get(buffer_row) else {
                frame.fill(row, left, rect.cols, CellAttributes::default());
                continue;
            };
            let rendered_cols = line.cells.len().min(width);
            for (offset, cell) in line.cells.iter().take(rendered_cols).enumerate() {
                if offset < skip_until {
                    continue;
                }
                if cell.wide_continuation {
                    continue;
                }
                let col = left + offset as u16;
                let columns = cell
                    .character_start()
                    .map(|start| start..start + cell.character_length as usize);
                let hint = columns.clone().and_then(|mut columns| {
                    columns.find_map(|col| {
                        vim.jump_hint(Position {
                            row: buffer_row,
                            col,
                        })
                    })
                });
                let matched = |predicate: &dyn Fn(Position) -> bool| {
                    columns.clone().is_some_and(|columns| {
                        columns.into_iter().any(|col| {
                            predicate(Position {
                                row: buffer_row,
                                col,
                            })
                        })
                    })
                };
                let attributes = if hint.is_some() {
                    vim_jump_cell_attributes(cell.attributes, theme)
                } else if matched(&|position| vim.selected(position)) {
                    vim_selected_cell_attributes(cell.attributes, theme)
                } else if matched(&|position| vim.search_match(position)) {
                    vim_search_cell_attributes(
                        cell.attributes,
                        matched(&|position| vim.current_search_match(position)),
                        theme,
                    )
                } else {
                    cell.attributes
                };
                let contents = cell.contents(&line.text);
                if let Some(hint) = hint {
                    frame.set_text(row, col, hint, attributes);
                    skip_until = offset + hint.chars().count();
                    continue;
                }
                let text = if contents.is_empty() { " " } else { contents };
                let wide = line
                    .cells
                    .get(offset + 1)
                    .is_some_and(|next| next.wide_continuation);
                if wide && hint.is_none() {
                    frame.set_wide_cell(row, col, text, attributes);
                } else {
                    frame.set_cell(row, col, text, attributes);
                }
            }
            if rendered_cols < width {
                frame.fill(
                    row,
                    left + rendered_cols as u16,
                    (width - rendered_cols) as u16,
                    CellAttributes::default(),
                );
            }
        }
        if !active {
            return;
        }
        let cursor_row = vim
            .cursor
            .row
            .saturating_sub(vim.viewport_top)
            .min(rect.rows as usize - 1) as u16;
        let cursor_col = vim_cursor_column(&state.lines[vim.cursor.row], vim.cursor.col)
            .min(width.saturating_sub(1)) as u16;
        frame.set_cursor(FrameCursor {
            row: rect.row + cursor_row + 1,
            col: bar_width + rect.col + cursor_col + 1,
            shape: CursorShape::Block,
            visible: true,
        });
    }

    fn render_tree(&self, id: usize, frame: &mut Frame, rows: u16, cols: u16, bar_width: u16) {
        let client = &self.clients[&id];
        let theme = client.rendered_theme();
        let bell_style = client.bell_style;
        let tree = client.tree.as_ref().unwrap();
        let items = self.tree_items(&tree.expanded);
        let selected = tree.selected.min(items.len().saturating_sub(1));
        let available_width = cols.saturating_sub(bar_width);
        let panel_width = tree_panel_width(available_width);
        let panel_col = bar_width + 1;
        let divider_col = panel_col + panel_width;
        let preview_col = divider_col.saturating_add(2);
        let preview_width = cols.saturating_sub(preview_col).saturating_add(1);

        let panel = CellAttributes::colors(theme.panel_foreground, theme.panel_background);
        for row in 1..=rows {
            frame.fill(row, panel_col, panel_width, panel);
            if divider_col <= cols {
                frame.set_cell(
                    row,
                    divider_col,
                    "│",
                    CellAttributes::foreground(theme.divider),
                );
            }
        }

        let header = two_sided_line(
            " sessions",
            &format!(
                "{} session{} ",
                self.sessions.len(),
                if self.sessions.len() == 1 { "" } else { "s" }
            ),
            panel_width as usize,
        );
        frame.set_text(
            1,
            panel_col,
            &header,
            CellAttributes::colors(theme.panel_heading, theme.panel_background).bold(),
        );

        let display_rows = tree_display_rows(&items);
        let list_height = rows.saturating_sub(1) as usize;
        let first_row = tree_first_display_row(&display_rows, selected, list_height);
        for (screen_index, display_row) in display_rows
            .iter()
            .skip(first_row)
            .take(list_height)
            .enumerate()
        {
            let row = screen_index as u16 + 2;
            let item_index = display_row.item_index;
            let item = &items[item_index];
            let session = self
                .sessions
                .iter()
                .find(|session| session.id == item.session_id)
                .unwrap();
            let selected_row = item_index == selected;
            let background = if selected_row {
                theme.panel_selected
            } else {
                theme.panel_background
            };
            let mut attributes = CellAttributes::colors(
                if selected_row {
                    theme.panel_foreground
                } else {
                    theme.panel_row_foreground
                },
                background,
            );
            frame.fill(row, panel_col, panel_width, attributes);
            let shortcut = tree_shortcut(item_index).unwrap_or_default();
            let line = match display_row.kind {
                TreeRowKind::Root => {
                    attributes = CellAttributes::colors(theme.panel_heading, background).dim();
                    format!("       {}", compact_path(&session.root))
                }
                TreeRowKind::Primary if item.window.is_none() => {
                    attributes = attributes.bold();
                    let fold = if tree.expanded.contains(&item.session_id) {
                        '▾'
                    } else {
                        '▸'
                    };
                    two_sided_line(
                        &format!(" {shortcut:>2}  {fold}  {}", item.label),
                        &format!(
                            "{} window{} ",
                            session.windows.len(),
                            if session.windows.len() == 1 { "" } else { "s" }
                        ),
                        panel_width as usize,
                    )
                }
                TreeRowKind::Primary if item.pane.is_none() => {
                    tree_window_line(&shortcut, &item.label)
                }
                TreeRowKind::Primary => {
                    let window = &session.windows[item.window.unwrap()];
                    let pane = item.pane.unwrap();
                    let marker = if window.panes[pane].id == window.active_pane {
                        '●'
                    } else {
                        '○'
                    };
                    format!(" {shortcut:>2}         {marker} {}", item.label)
                }
            };
            frame.set_text(
                row,
                panel_col,
                &truncate(&line, panel_width as usize),
                attributes,
            );
        }

        let Some(item) = items.get(selected) else {
            return;
        };
        let Some(session) = self
            .sessions
            .iter()
            .find(|session| session.id == item.session_id)
        else {
            return;
        };
        if preview_width == 0 || rows < 3 {
            return;
        }
        let title = if let Some(window_index) = item.window {
            let window = &session.windows[window_index];
            let pane_index = item.pane.unwrap_or_else(|| {
                window
                    .panes
                    .iter()
                    .position(|pane| pane.id == window.active_pane)
                    .unwrap()
            });
            match window.label() {
                Some(label) => format!(
                    "{}  ·  window {} · {label}  ·  pane {}",
                    session.name,
                    window_index + 1,
                    pane_index + 1
                ),
                None => format!(
                    "{}  ·  window {}  ·  pane {}",
                    session.name,
                    window_index + 1,
                    pane_index + 1
                ),
            }
        } else {
            format!(
                "{}  ·  {} window{}",
                session.name,
                session.windows.len(),
                if session.windows.len() == 1 { "" } else { "s" }
            )
        };
        frame.set_text(
            1,
            preview_col,
            &truncate(&title, preview_width as usize),
            CellAttributes::foreground(theme.panel_heading).bold(),
        );
        frame.set_text(
            2,
            preview_col,
            &truncate(&compact_path(&session.root), preview_width as usize),
            CellAttributes::foreground(theme.panel_heading).dim(),
        );
        let preview_height = rows.saturating_sub(3);
        let Some(window_index) = item.window else {
            render_session_overview(
                frame,
                session,
                Rect {
                    row: 4,
                    col: preview_col,
                    rows: preview_height,
                    cols: preview_width,
                },
                &theme,
                bell_style,
            );
            return;
        };

        let window = &session.windows[window_index];
        let pane_index = item.pane.unwrap_or_else(|| {
            window
                .panes
                .iter()
                .position(|pane| pane.id == window.active_pane)
                .unwrap()
        });
        let pane = &window.panes[pane_index];
        let screen = pane.parser.screen();
        let (source_top, source_height) = preview_source_region(screen, preview_height);
        let destination = Rect {
            row: 4,
            col: preview_col,
            rows: source_height,
            cols: preview_width,
        };
        render_screen_region(frame, screen, source_top, destination);
        if let Some(cursor) = preview_cursor(
            screen,
            pane.parser.callbacks().cursor_shape,
            source_top,
            destination,
        ) {
            frame.set_cursor(cursor);
        }
    }
}

/// Shows every window of a session at once, laid out in a grid.
fn render_session_overview(
    frame: &mut Frame,
    session: &Session,
    area: Rect,
    theme: &Theme,
    bell_style: BellStyle,
) {
    let rects = preview_grid_rects(session.windows.len(), area);
    for (window_index, (window, rect)) in session.windows.iter().zip(&rects).enumerate() {
        if rect.rows == 0 || rect.cols == 0 {
            continue;
        }
        render_preview_window_title(
            frame,
            session,
            window_index,
            window,
            *rect,
            theme,
            bell_style,
        );
        if rect.rows == 1 {
            continue;
        }
        let pane = window
            .panes
            .iter()
            .find(|pane| pane.id == window.active_pane)
            .unwrap();
        let screen = pane.parser.screen();
        let (source_top, source_height) = preview_source_region(screen, rect.rows - 1);
        let destination = Rect {
            row: rect.row + 1,
            col: rect.col,
            rows: source_height,
            cols: rect.cols,
        };
        render_screen_region(frame, screen, source_top, destination);
        // The grid shows every window at once, so the cursor goes to the one
        // the session would open on.
        if window_index == session.current_window
            && let Some(cursor) = preview_cursor(
                screen,
                pane.parser.callbacks().cursor_shape,
                source_top,
                destination,
            )
        {
            frame.set_cursor(cursor);
        }
    }
    render_preview_grid_separators(frame, area, &rects, theme);
}
