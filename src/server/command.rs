//! The `mux ...` subcommands, and the read-only queries beside them.
//!
//! Unlike a keystroke these arrive from a shell rather than an attached client,
//! so the first job is working out which client — and which session — the
//! caller meant.

use std::collections::HashSet;

use anyhow::{Context, Result, bail};

use crate::protocol::{MuxCommand, MuxQuery};

use super::*;

impl Server {
    pub(super) fn run_command(
        &mut self,
        pane_id: Option<usize>,
        command: MuxCommand,
    ) -> Result<()> {
        let id = self.command_target_client(pane_id)?;
        match command {
            MuxCommand::ChooseTree => {
                self.open_session_tree(id);
            }
            MuxCommand::Detach => self.detach(id)?,
            MuxCommand::NewWindow => self.new_window(id)?,
            MuxCommand::NewSession(name) => {
                if let Some(name) = name {
                    if name.trim().is_empty() {
                        bail!("session name cannot be empty");
                    }
                    let root = self
                        .active_cwd(id)
                        .unwrap_or_else(|| self.clients[&id].cwd.clone());
                    let (cols, rows) = self.client_size(id);
                    let session_id = self.create_session(name, root, cols, rows)?;
                    self.set_client_session(id, session_id);
                    self.remember_active_pane(id)?;
                    self.save_state_soon();
                } else {
                    self.new_session(id)?;
                }
            }
            MuxCommand::SetSessionRoot => self.set_session_root(id)?,
            MuxCommand::RenameSession(name) => {
                let (session_index, _) = self.active_indices(id).context("no active session")?;
                if name.trim().is_empty() {
                    bail!("session name cannot be empty");
                }
                if self
                    .sessions
                    .iter()
                    .enumerate()
                    .any(|(index, session)| index != session_index && session.name == name)
                {
                    bail!("session {name:?} already exists");
                }
                self.sessions[session_index].name = name;
                self.save_state_soon();
            }
            MuxCommand::SplitHorizontal => self.split_active_pane(id, SplitAxis::Horizontal)?,
            MuxCommand::SplitVertical => self.split_active_pane(id, SplitAxis::Vertical)?,
            MuxCommand::FocusLeft => self.focus_pane(id, PaneDirection::Left)?,
            MuxCommand::FocusDown => self.focus_pane(id, PaneDirection::Down)?,
            MuxCommand::FocusUp => self.focus_pane(id, PaneDirection::Up)?,
            MuxCommand::FocusRight => self.focus_pane(id, PaneDirection::Right)?,
            MuxCommand::ResizeLeft(cells) => self.resize_pane(id, PaneDirection::Left, cells)?,
            MuxCommand::ResizeDown(cells) => self.resize_pane(id, PaneDirection::Down, cells)?,
            MuxCommand::ResizeUp(cells) => self.resize_pane(id, PaneDirection::Up, cells)?,
            MuxCommand::ResizeRight(cells) => self.resize_pane(id, PaneDirection::Right, cells)?,
            MuxCommand::ZoomPane => self.zoom_pane(id)?,
            MuxCommand::BreakPane => self.break_pane(id)?,
            MuxCommand::JoinPane {
                window,
                axis_is_vertical,
            } => self.join_pane(
                id,
                window as usize,
                if axis_is_vertical {
                    SplitAxis::Vertical
                } else {
                    SplitAxis::Horizontal
                },
            )?,
            MuxCommand::SwapWindow(window) => self.swap_window(id, window as usize)?,
            MuxCommand::RenameWindow(name) => {
                let (session_index, window_index) =
                    self.active_indices(id).context("no active window")?;
                self.sessions[session_index].windows[window_index].name =
                    (!name.trim().is_empty()).then_some(name);
                self.save_state_soon();
            }
            MuxCommand::JumpToBell => self.jump_to_bell(id)?,
            MuxCommand::KillPane => {
                let pane_id = self.active_pane(id).context("no active pane")?.id;
                self.kill_pane(pane_id)?;
            }
            MuxCommand::KillSession => {
                let session_id = self.clients[&id].session_id.context("no active session")?;
                self.kill_session(session_id)?;
            }
            MuxCommand::SelectWindow(number) => {
                let (session_index, _) = self.active_indices(id).context("no active session")?;
                if number as usize > self.sessions[session_index].windows.len() {
                    bail!("window {number} does not exist");
                }
                self.select_window(id, number as usize)?;
            }
            MuxCommand::EnterVim => self.enter_vim(id),
            MuxCommand::SetTheme(theme) => {
                self.theme = theme;
                for client in self
                    .clients
                    .values_mut()
                    .filter(|client| client.initialized)
                {
                    client.theme = theme;
                    // A picker open on another client was listing the theme
                    // that has just been replaced as the one in use.
                    if let Some(picker) = &mut client.themes
                        && let Some(directory) = &client.theme_directory
                    {
                        picker.in_use = current_theme_name(directory).and_then(|name| {
                            picker.entries.iter().position(|entry| entry.name == name)
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Answers a query, one line per session, window or pane.
    ///
    /// Unlike a command this needs no attached client: the pane a script is
    /// running in tells the daemon which session it means, and without even
    /// that it falls back to the session that was last active.
    pub(super) fn listing(&self, pane_id: Option<usize>, query: MuxQuery) -> Vec<String> {
        let attached: HashSet<usize> = self
            .clients
            .values()
            .filter(|client| client.initialized)
            .filter_map(|client| client.session_id)
            .collect();
        let current = pane_id
            .and_then(|pane_id| self.session_of_pane(pane_id))
            .or_else(|| self.last_active_session_id());
        match query {
            MuxQuery::Sessions => self
                .sessions
                .iter()
                .map(|session| {
                    let panes: usize = session
                        .windows
                        .iter()
                        .map(|window| window.panes.len())
                        .sum();
                    format!(
                        "{}: {} window{}, {panes} pane{} [{}]{}{}",
                        session.name,
                        session.windows.len(),
                        if session.windows.len() == 1 { "" } else { "s" },
                        if panes == 1 { "" } else { "s" },
                        session.root.display(),
                        if attached.contains(&session.id) {
                            " (attached)"
                        } else {
                            ""
                        },
                        if current == Some(session.id) {
                            " (current)"
                        } else {
                            ""
                        },
                    )
                })
                .collect(),
            MuxQuery::Windows => {
                let Some(session) = self.session_or_current(current) else {
                    return Vec::new();
                };
                session
                    .windows
                    .iter()
                    .enumerate()
                    .map(|(index, window)| {
                        format!(
                            "{}:{}: {} ({} pane{}){}{}",
                            session.name,
                            index + 1,
                            window.label().unwrap_or("shell"),
                            window.panes.len(),
                            if window.panes.len() == 1 { "" } else { "s" },
                            if index == session.current_window {
                                " (active)"
                            } else {
                                ""
                            },
                            if window.zoomed { " (focus mode)" } else { "" },
                        )
                    })
                    .collect()
            }
            MuxQuery::Panes => {
                let Some(session) = self.session_or_current(current) else {
                    return Vec::new();
                };
                let window_index = session.current_window;
                let window = &session.windows[window_index];
                window
                    .panes
                    .iter()
                    .enumerate()
                    .map(|(index, pane)| {
                        format!(
                            "{}:{}.{}: {} [{}x{}]{}",
                            session.name,
                            window_index + 1,
                            index + 1,
                            pane.cwd.display(),
                            pane.parser.screen().size().1,
                            pane.parser.screen().size().0,
                            if pane.id == window.active_pane {
                                " (active)"
                            } else {
                                ""
                            },
                        )
                    })
                    .collect()
            }
        }
    }

    fn session_or_current(&self, current: Option<usize>) -> Option<&Session> {
        current
            .and_then(|session_id| {
                self.sessions
                    .iter()
                    .find(|session| session.id == session_id)
            })
            .or_else(|| self.sessions.first())
    }

    fn session_of_pane(&self, pane_id: usize) -> Option<usize> {
        self.sessions.iter().find_map(|session| {
            session
                .windows
                .iter()
                .any(|window| window.panes.iter().any(|pane| pane.id == pane_id))
                .then_some(session.id)
        })
    }

    fn command_target_client(&mut self, pane_id: Option<usize>) -> Result<usize> {
        let mut attached: Vec<_> = self
            .clients
            .iter()
            .filter_map(|(id, client)| client.initialized.then_some(*id))
            .collect();
        attached.sort_unstable();

        let Some(pane_id) = pane_id else {
            return attached.pop().context("no attached mux client");
        };
        if let Some(id) = attached
            .iter()
            .rev()
            .copied()
            .find(|id| self.active_pane(*id).is_some_and(|pane| pane.id == pane_id))
        {
            return Ok(id);
        }

        let origin = self
            .sessions
            .iter()
            .enumerate()
            .find_map(|(session_index, session)| {
                session
                    .windows
                    .iter()
                    .enumerate()
                    .find_map(|(window_index, window)| {
                        window
                            .panes
                            .iter()
                            .any(|pane| pane.id == pane_id)
                            .then_some((session_index, window_index))
                    })
            });
        // `MUX_PANE` outlives its pane in a shell that was started inside one
        // and kept running — after a daemon restart, say. That is no reason to
        // refuse the command, so it goes to whoever is attached.
        let Some((session_index, window_index)) = origin else {
            return attached.pop().context("no attached mux client");
        };
        let session_id = self.sessions[session_index].id;
        let id = attached
            .iter()
            .rev()
            .copied()
            .find(|id| self.clients[id].session_id == Some(session_id))
            .or_else(|| attached.last().copied())
            .context("no attached mux client")?;
        visit_window(&mut self.sessions[session_index], window_index);
        self.sessions[session_index].windows[window_index].select_pane(pane_id);
        self.set_client_session(id, session_id);
        self.last_active_pane = Some(pane_id);
        self.save_state_soon();
        Ok(id)
    }
}
