//! What a keystroke, a click or a paste does.
//!
//! Every client is in exactly one mode — normal, leader, tree, theme or Vim —
//! and the mode decides which table a key is looked up in and which of these
//! handlers runs it.

use anyhow::Result;

use crate::{
    config::{Action, Mode},
    protocol::{ALT, CTRL, Key, KeyCode, Mouse, MouseKind},
};

use super::*;

impl Server {
    pub(super) fn handle_key(&mut self, id: usize, key: Key) -> Result<()> {
        let vim_active = self.vim_active(id);
        let Some(client) = self.clients.get_mut(&id) else {
            return Ok(());
        };
        if !client.initialized {
            return Ok(());
        }
        // A transient message has served its purpose once the next key arrives.
        client.message = None;
        let client = &self.clients[&id];
        if client.confirmation.is_some() {
            self.handle_confirmation_key(id, key)?;
            self.dirty = true;
            return Ok(());
        }
        if client.rename.is_some() {
            self.handle_rename_key(id, key)?;
            self.dirty = true;
            return Ok(());
        }
        let mode = if client.leader {
            Mode::Leader
        } else if client.themes.is_some() {
            Mode::Theme
        } else if client.tree.is_some() {
            Mode::Tree
        } else if vim_active {
            Mode::Vim
        } else {
            Mode::Normal
        };
        let action = client.bindings.get(mode, &key);
        match mode {
            Mode::Normal => self.handle_normal_key(id, action, key)?,
            Mode::Leader => self.handle_leader_key(id, action)?,
            Mode::Tree => self.handle_tree_key(id, action)?,
            Mode::Theme => self.handle_theme_key(id, action)?,
            Mode::Vim => self.handle_vim_key(id, action, key)?,
        }
        self.dirty = true;
        Ok(())
    }

    fn handle_normal_key(&mut self, id: usize, action: Option<Action>, key: Key) -> Result<()> {
        match action {
            Some(Action::EnterLeader) => self.enter_leader(id),
            Some(Action::SessionTree) => self.open_session_tree(id),
            Some(Action::NewWindow) => self.new_window(id)?,
            Some(Action::NewSession) => self.new_session(id)?,
            Some(Action::SetSessionRoot) => self.set_session_root(id)?,
            Some(Action::SelectWindow(number)) => self.select_window(id, number as usize)?,
            Some(Action::EnterVim) => self.enter_vim(id),
            Some(Action::EnterVimJump) => self.enter_vim_jump(id),
            Some(Action::Detach) => self.detach(id)?,
            Some(Action::ThemePicker) => self.open_theme_picker(id),
            None => self.send_key_to_pty(id, &key)?,
            _ => {}
        }
        Ok(())
    }

    fn enter_leader(&mut self, id: usize) {
        self.clients.get_mut(&id).unwrap().leader = true;
    }

    fn handle_leader_key(&mut self, id: usize, action: Option<Action>) -> Result<()> {
        // Resizing is worth repeating, so those keys keep the leader held for
        // the next press; everything else is a one-shot.
        let repeatable = matches!(
            action,
            Some(
                Action::ResizePaneLeft
                    | Action::ResizePaneDown
                    | Action::ResizePaneUp
                    | Action::ResizePaneRight
            )
        );
        // A repeat keeps the time leader was first entered, so holding it down
        // to resize still brings the help out instead of restarting its wait.
        if !repeatable {
            self.clients.get_mut(&id).unwrap().leader = false;
        }
        match action {
            Some(Action::ResizePaneLeft) => {
                self.resize_pane(id, PaneDirection::Left, RESIZE_STEP)?
            }
            Some(Action::ResizePaneDown) => {
                self.resize_pane(id, PaneDirection::Down, RESIZE_STEP)?
            }
            Some(Action::ResizePaneUp) => self.resize_pane(id, PaneDirection::Up, RESIZE_STEP)?,
            Some(Action::ResizePaneRight) => {
                self.resize_pane(id, PaneDirection::Right, RESIZE_STEP)?
            }
            Some(Action::RenameSession) => self.start_rename(id),
            Some(Action::RenameWindow) => self.start_rename_window(id),
            Some(Action::SplitHorizontal) => self.split_active_pane(id, SplitAxis::Horizontal)?,
            Some(Action::SplitVertical) => self.split_active_pane(id, SplitAxis::Vertical)?,
            Some(Action::FocusPaneLeft) => self.focus_pane(id, PaneDirection::Left)?,
            Some(Action::FocusPaneDown) => self.focus_pane(id, PaneDirection::Down)?,
            Some(Action::FocusPaneUp) => self.focus_pane(id, PaneDirection::Up)?,
            Some(Action::FocusPaneRight) => self.focus_pane(id, PaneDirection::Right)?,
            Some(Action::ZoomPane) => self.zoom_pane(id)?,
            Some(Action::BreakPane) => self.break_pane(id)?,
            Some(Action::SwapWindowLeft) => self.swap_window_by(id, -1)?,
            Some(Action::SwapWindowRight) => self.swap_window_by(id, 1)?,
            Some(Action::JumpToBell) => self.jump_to_bell(id)?,
            Some(Action::KillPane) => self.start_kill_pane(id),
            Some(Action::Detach) => self.detach(id)?,
            Some(Action::ThemePicker) => self.open_theme_picker(id),
            // Anything else, including LeaderCancel, just leaves leader mode.
            _ => {}
        }
        Ok(())
    }

    fn handle_confirmation_key(&mut self, id: usize, key: Key) -> Result<()> {
        let unmodified = key.modifiers & (ALT | CTRL) == 0;
        let confirmed = unmodified && matches!(key.code, KeyCode::Char('y' | 'Y'));
        let cancelled = matches!(key.code, KeyCode::Escape | KeyCode::Enter)
            || (unmodified && matches!(key.code, KeyCode::Char('n' | 'N')));
        if !confirmed && !cancelled {
            return Ok(());
        }
        let confirmation = self.clients.get_mut(&id).unwrap().confirmation.take();
        if confirmed {
            match confirmation {
                Some(Confirmation::KillPane { pane_id, .. }) => self.kill_pane(pane_id)?,
                Some(Confirmation::KillSession { session_id, .. }) => {
                    self.kill_session(session_id)?
                }
                None => {}
            }
        }
        Ok(())
    }

    fn handle_rename_key(&mut self, id: usize, key: Key) -> Result<()> {
        let rename = self.clients.get_mut(&id).unwrap().rename.as_mut().unwrap();
        match key.code {
            KeyCode::Enter => self.finish_rename(id)?,
            KeyCode::Escape => self.clients.get_mut(&id).unwrap().rename = None,
            KeyCode::Left => rename.cursor = rename.cursor.saturating_sub(1),
            KeyCode::Right => rename.cursor = (rename.cursor + 1).min(rename.text.chars().count()),
            KeyCode::Home => rename.cursor = 0,
            KeyCode::End => rename.cursor = rename.text.chars().count(),
            KeyCode::Backspace => rename.backspace(),
            KeyCode::Delete => rename.delete(),
            KeyCode::Char('a') if key.modifiers & CTRL != 0 => rename.cursor = 0,
            KeyCode::Char('e') if key.modifiers & CTRL != 0 => {
                rename.cursor = rename.text.chars().count();
            }
            KeyCode::Char('w') if key.modifiers & CTRL != 0 => {
                rename.delete_word_before_cursor();
            }
            KeyCode::Char('u') if key.modifiers & CTRL != 0 => {
                rename.delete_before_cursor();
            }
            KeyCode::Char('k') if key.modifiers & CTRL != 0 => rename.delete_after_cursor(),
            KeyCode::Char('h') if key.modifiers & CTRL != 0 => rename.backspace(),
            KeyCode::Char('d') if key.modifiers & CTRL != 0 => rename.delete(),
            KeyCode::Char(character) if key.modifiers & (ALT | CTRL) == 0 => {
                rename.insert(character);
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_tree_key(&mut self, id: usize, action: Option<Action>) -> Result<()> {
        let items = {
            let tree = self.clients[&id].tree.as_ref().unwrap();
            self.tree_items(&tree.expanded)
        };
        if items.is_empty() {
            self.clients.get_mut(&id).unwrap().tree = None;
            return Ok(());
        }
        let tree = self.clients.get_mut(&id).unwrap().tree.as_mut().unwrap();
        match action {
            Some(Action::TreeDown) => tree.selected = (tree.selected + 1).min(items.len() - 1),
            Some(Action::TreeUp) => tree.selected = tree.selected.saturating_sub(1),
            Some(Action::TreeChoose) => {
                let item = items[tree.selected.min(items.len() - 1)].clone();
                self.choose_tree_item(id, item)?;
            }
            Some(Action::TreeSelect(number)) => {
                if let Some(item) = items.get(number as usize - 1) {
                    self.choose_tree_item(id, item.clone())?;
                }
            }
            Some(Action::TreeExpand) => {
                let item = items[tree.selected.min(items.len() - 1)].clone();
                if item.window.is_none() {
                    tree.expanded.insert(item.session_id);
                }
            }
            Some(Action::TreeCollapse) => {
                let item = items[tree.selected.min(items.len() - 1)].clone();
                tree.expanded.remove(&item.session_id);
                tree.selected = items
                    .iter()
                    .position(|candidate| {
                        candidate.session_id == item.session_id && candidate.window.is_none()
                    })
                    .unwrap_or(0);
            }
            Some(Action::TreeToggle) => {
                let item = items[tree.selected.min(items.len() - 1)].clone();
                if tree.expanded.remove(&item.session_id) {
                    if item.window.is_some() {
                        tree.selected = items
                            .iter()
                            .position(|candidate| {
                                candidate.session_id == item.session_id
                                    && candidate.window.is_none()
                            })
                            .unwrap_or(0);
                    }
                } else {
                    tree.expanded.insert(item.session_id);
                }
            }
            Some(Action::KillSession) => {
                let item = items[tree.selected.min(items.len() - 1)].clone();
                self.start_kill_session(id, item.session_id);
            }
            Some(Action::SessionTree) => self.switch_to_previous_session(id)?,
            Some(Action::EnterLeader) => self.enter_leader(id),
            Some(Action::ThemePicker) => self.open_theme_picker(id),
            Some(Action::TreeCancel) => self.clients.get_mut(&id).unwrap().tree = None,
            _ => {}
        }
        Ok(())
    }

    fn handle_theme_key(&mut self, id: usize, action: Option<Action>) -> Result<()> {
        let Some(picker) = self.clients.get_mut(&id).unwrap().themes.as_mut() else {
            return Ok(());
        };
        let last = picker.entries.len() - 1;
        match action {
            Some(Action::ThemeNext) => picker.selected = (picker.selected + 1).min(last),
            Some(Action::ThemePrevious) => picker.selected = picker.selected.saturating_sub(1),
            Some(Action::ThemeSelect(number)) => {
                let index = number as usize - 1;
                if index <= last {
                    picker.selected = index;
                    self.apply_selected_theme(id);
                }
            }
            Some(Action::ThemeChoose) => self.apply_selected_theme(id),
            Some(Action::ThemeCancel) => self.clients.get_mut(&id).unwrap().themes = None,
            // Rebinding the opening key inside the picker rereads the directory,
            // which is the only way to notice a theme installed since it opened.
            Some(Action::ThemePicker) => self.open_theme_picker(id),
            _ => {}
        }
        Ok(())
    }

    fn handle_vim_key(&mut self, id: usize, action: Option<Action>, key: Key) -> Result<()> {
        if let Some(Action::EnterLeader) = action {
            self.enter_leader(id);
            return Ok(());
        }
        if let Some(Action::SelectWindow(number)) = action {
            return self.select_window(id, number as usize);
        }
        let pane_id = self.active_vim_pane_id(id).unwrap();
        let outcome = self
            .clients
            .get_mut(&id)
            .unwrap()
            .vim
            .get_mut(&pane_id)
            .unwrap()
            .mode
            .handle(action, &key);
        match outcome {
            VimOutcome::None => {}
            VimOutcome::Exit => {
                self.clients.get_mut(&id).unwrap().vim.remove(&pane_id);
            }
            VimOutcome::Yank(text) => {
                let command = self.clients[&id].clipboard_command.clone();
                copy_to_clipboard(self.events.clone(), id, command, text);
                self.clients.get_mut(&id).unwrap().vim.remove(&pane_id);
            }
        }
        Ok(())
    }

    /// Routes a mouse event to whatever is under the pointer.
    ///
    /// A program that has asked for mouse reporting gets the event as the
    /// terminal would have sent it; otherwise a click picks a pane or a window
    /// and the wheel scrolls back through the pane's history.
    pub(super) fn handle_mouse(&mut self, id: usize, mouse: Mouse) -> Result<()> {
        let Some(client) = self.clients.get(&id) else {
            return Ok(());
        };
        if !client.mouse || !client.initialized {
            return Ok(());
        }
        // A popup owns the screen while it is up.
        if client.confirmation.is_some()
            || client.rename.is_some()
            || client.tree.is_some()
            || client.themes.is_some()
        {
            return Ok(());
        }
        let bar_width = self.active_bar_width(id);
        if mouse.col < bar_width {
            if matches!(mouse.kind, MouseKind::Down) {
                self.select_bar_row(id, mouse.row)?;
            }
            return Ok(());
        }
        let Some((pane_id, rect)) = self.pane_at(id, mouse.row, mouse.col) else {
            return Ok(());
        };
        let inside = Mouse {
            col: mouse.col - rect.col - bar_width,
            row: mouse.row - rect.row,
            ..mouse
        };
        if matches!(mouse.kind, MouseKind::Down) {
            let Some((session_index, window_index)) = self.active_indices(id) else {
                return Ok(());
            };
            self.sessions[session_index].windows[window_index].select_pane(pane_id);
            self.remember_active_pane(id)?;
            self.save_state_soon();
        }
        // Vim mode is mux's own; the program underneath does not see it.
        if self.vim_active(id) {
            return self.scroll_vim(id, mouse);
        }
        let forwarded = self
            .pane_mut(pane_id)
            .and_then(|pane| mouse_report(pane.parser.screen(), inside));
        if let Some(bytes) = forwarded {
            if let Some(pane) = self.pane_mut(pane_id) {
                let _ = pane
                    .writer
                    .write_all(&bytes)
                    .and_then(|()| pane.writer.flush());
            }
            return Ok(());
        }
        // Nothing is listening, so the wheel scrolls mux's own history instead.
        if matches!(mouse.kind, MouseKind::ScrollUp | MouseKind::ScrollDown) {
            self.enter_vim(id);
            return self.scroll_vim(id, mouse);
        }
        Ok(())
    }

    /// Scrolls the vim-mode viewport, leaving it when it reaches the bottom so
    /// the wheel hands the pane back the way it found it.
    fn scroll_vim(&mut self, id: usize, mouse: Mouse) -> Result<()> {
        let Some(pane_id) = self.active_vim_pane_id(id) else {
            return Ok(());
        };
        let up = matches!(mouse.kind, MouseKind::ScrollUp);
        if !up && !matches!(mouse.kind, MouseKind::ScrollDown) {
            return Ok(());
        }
        let client = self.clients.get_mut(&id).unwrap();
        let Some(state) = client.vim.get_mut(&pane_id) else {
            return Ok(());
        };
        if state.mode.scroll(up, MOUSE_SCROLL_LINES) {
            return Ok(());
        }
        if !up {
            // Scrolled past the end: the live pane is what comes next.
            client.vim.remove(&pane_id);
        }
        Ok(())
    }

    /// Which pane covers a point of the client's screen.
    fn pane_at(&self, id: usize, row: u16, col: u16) -> Option<(usize, Rect)> {
        let (session_index, window_index) = self.active_indices(id)?;
        let (regions, _) =
            self.sessions[session_index].windows[window_index].regions(self.content_area(id));
        let col = col.checked_sub(self.active_bar_width(id))?;
        regions.into_iter().find(|(_, rect)| {
            row >= rect.row
                && row < rect.row + rect.rows
                && col >= rect.col
                && col < rect.col + rect.cols
        })
    }

    /// Picks the window whose label is on `row` of the bar.
    fn select_bar_row(&mut self, id: usize, row: u16) -> Result<()> {
        let Some((session_index, current_window)) = self.active_indices(id) else {
            return Ok(());
        };
        let windows = self.sessions[session_index].windows.len();
        let (_, rows) = self.client_size(id);
        let (first_window, first_row, visible) =
            centered_bar_layout(windows, current_window, rows.max(1) as usize);
        let offset = (row as usize).checked_sub(first_row).map(|row| row / 3);
        let Some(offset) = offset.filter(|offset| *offset < visible) else {
            return Ok(());
        };
        self.select_window(id, first_window + offset + 1)
    }

    pub(super) fn handle_paste(&mut self, id: usize, text: String) -> Result<()> {
        let Some(client) = self.clients.get(&id) else {
            return Ok(());
        };
        if client.confirmation.is_some() {
            return Ok(());
        }
        if client.rename.is_some() {
            let rename = self.clients.get_mut(&id).unwrap().rename.as_mut().unwrap();
            for character in text.chars().filter(|character| !character.is_control()) {
                rename.insert(character);
            }
            self.dirty = true;
            return Ok(());
        }
        if self.vim_active(id) || client.tree.is_some() || client.themes.is_some() {
            return Ok(());
        }
        let bracketed = self
            .active_pane(id)
            .is_some_and(|pane| pane.parser.screen().bracketed_paste());
        let mut bytes = Vec::new();
        if bracketed {
            bytes.extend_from_slice(b"\x1b[200~");
        }
        bytes.extend_from_slice(text.as_bytes());
        if bracketed {
            bytes.extend_from_slice(b"\x1b[201~");
        }
        self.write_active(id, &bytes)
    }

    fn send_key_to_pty(&mut self, id: usize, key: &Key) -> Result<()> {
        let application_cursor = self
            .active_pane(id)
            .is_some_and(|pane| pane.parser.screen().application_cursor());
        let bytes = terminal_key_bytes(key, application_cursor);
        self.write_active(id, &bytes)
    }

    /// Sends `bytes` to the active pane's shell.
    ///
    /// A write that fails means that shell is on its way out; the `PtyClosed`
    /// event that follows tidies the pane up, so the keystroke is simply lost.
    fn write_active(&mut self, id: usize, bytes: &[u8]) -> Result<()> {
        let (session_index, window_index, pane_index) =
            self.active_pane_indices(id).context("no active pane")?;
        let writer =
            &mut self.sessions[session_index].windows[window_index].panes[pane_index].writer;
        let _ = writer.write_all(bytes).and_then(|()| writer.flush());
        Ok(())
    }
}
