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

    /// Answers a query as display lines or one JSON array.
    ///
    /// Unlike a command this needs no attached client: the pane a script is
    /// running in tells the daemon which session it means, and without even
    /// that it falls back to the session that was last active. A pane supplied
    /// by the caller must still exist.
    pub(super) fn listing(
        &self,
        pane_id: Option<usize>,
        query: MuxQuery,
        json: bool,
    ) -> Result<Vec<String>> {
        let attached: HashSet<usize> = self
            .clients
            .values()
            .filter(|client| client.initialized)
            .filter_map(|client| client.session_id)
            .collect();
        let target = match pane_id {
            Some(pane_id) => Some(
                self.pane_location(pane_id)
                    .with_context(|| format!("pane {pane_id} does not exist"))?,
            ),
            None => self
                .last_active_pane
                .and_then(|pane_id| self.pane_location(pane_id)),
        };
        let current = target.map(|(session_index, _)| self.sessions[session_index].id);
        if json {
            return self.json_listing(current, target, query, &attached);
        }
        match query {
            MuxQuery::Sessions => Ok(self
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
                .collect()),
            MuxQuery::Windows => {
                let Some(session) = self.session_or_current(current) else {
                    return Ok(Vec::new());
                };
                Ok(session
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
                    .collect())
            }
            MuxQuery::Panes => {
                let Some(session) = self.session_or_current(current) else {
                    return Ok(Vec::new());
                };
                let window_index = target
                    .filter(|(session_index, _)| self.sessions[*session_index].id == session.id)
                    .map_or(session.current_window, |(_, window_index)| window_index);
                let window = &session.windows[window_index];
                Ok(window
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
                    .collect())
            }
        }
    }

    fn json_listing(
        &self,
        current: Option<usize>,
        target: Option<(usize, usize)>,
        query: MuxQuery,
        attached: &HashSet<usize>,
    ) -> Result<Vec<String>> {
        let values: Vec<serde_json::Value> = match query {
            MuxQuery::Sessions => self
                .sessions
                .iter()
                .map(|session| {
                    let panes: usize = session
                        .windows
                        .iter()
                        .map(|window| window.panes.len())
                        .sum();
                    serde_json::json!({
                        "id": session.id,
                        "name": session.name,
                        "root": session.root,
                        "windows": session.windows.len(),
                        "panes": panes,
                        "attached": attached.contains(&session.id),
                        "current": current == Some(session.id),
                    })
                })
                .collect(),
            MuxQuery::Windows => {
                let Some(session) = self.session_or_current(current) else {
                    return Ok(vec!["[]".into()]);
                };
                session
                    .windows
                    .iter()
                    .enumerate()
                    .map(|(index, window)| {
                        serde_json::json!({
                            "session_id": session.id,
                            "session": session.name,
                            "id": index + 1,
                            "name": window.label(),
                            "panes": window.panes.len(),
                            "active": index == session.current_window,
                            "focus_mode": window.zoomed,
                        })
                    })
                    .collect()
            }
            MuxQuery::Panes => {
                let Some(session) = self.session_or_current(current) else {
                    return Ok(vec!["[]".into()]);
                };
                let window_index = target
                    .filter(|(session_index, _)| self.sessions[*session_index].id == session.id)
                    .map_or(session.current_window, |(_, window_index)| window_index);
                let window = &session.windows[window_index];
                window
                    .panes
                    .iter()
                    .enumerate()
                    .map(|(index, pane)| {
                        serde_json::json!({
                            "session_id": session.id,
                            "session": session.name,
                            "window_id": window_index + 1,
                            "id": pane.id,
                            "index": index + 1,
                            "cwd": pane.cwd,
                            "cols": pane.parser.screen().size().1,
                            "rows": pane.parser.screen().size().0,
                            "active": pane.id == window.active_pane,
                        })
                    })
                    .collect()
            }
        };
        Ok(vec![
            serde_json::to_string(&values).context("encode query result")?,
        ])
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

    fn pane_location(&self, pane_id: usize) -> Option<(usize, usize)> {
        self.sessions
            .iter()
            .enumerate()
            .find_map(|(session_index, session)| {
                session
                    .windows
                    .iter()
                    .position(|window| window.panes.iter().any(|pane| pane.id == pane_id))
                    .map(|window_index| (session_index, window_index))
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
        let (session_index, window_index) =
            origin.with_context(|| format!("pane {pane_id} does not exist"))?;
        let session_id = self.sessions[session_index].id;
        let id = attached
            .iter()
            .rev()
            .copied()
            .find(|id| self.clients[id].session_id == Some(session_id))
            .context("no attached mux client for target session")?;
        visit_window(&mut self.sessions[session_index], window_index);
        self.sessions[session_index].windows[window_index].select_pane(pane_id);
        self.set_client_session(id, session_id);
        self.last_active_pane = Some(pane_id);
        self.save_state_soon();
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn json_queries_use_ids_and_explicit_panes_are_never_redirected() {
        let directory =
            std::env::temp_dir().join(format!("mux-command-query-{}", std::process::id()));
        let (mut server, _events, _client) =
            crate::server::tests::server_with_pending_bell(&directory);
        let first_pane = server.sessions[0].windows[0].panes[0].id;
        let first_session = server.sessions[0].id;
        server.last_active_pane = Some(first_pane);
        server.clients.get_mut(&1).unwrap().initialized = true;
        server.clients.get_mut(&1).unwrap().session_id = Some(first_session);
        let background_session = server
            .create_session("background".into(), directory.clone(), 90, 30)
            .unwrap();
        let background_pane = server.sessions[1].windows[0].panes[0].id;

        let sessions = server
            .listing(Some(first_pane), MuxQuery::Sessions, true)
            .unwrap();
        let sessions: serde_json::Value = serde_json::from_str(&sessions[0]).unwrap();
        assert_eq!(sessions[0]["id"], server.sessions[0].id);
        assert_eq!(sessions[1]["id"], background_session);

        let panes = server
            .listing(Some(background_pane), MuxQuery::Panes, true)
            .unwrap();
        let panes: serde_json::Value = serde_json::from_str(&panes[0]).unwrap();
        assert_eq!(panes[0]["session_id"], background_session);
        assert_eq!(panes[0]["window_id"], 1);
        assert_eq!(panes[0]["id"], background_pane);

        let error = server
            .run_command(Some(background_pane), MuxCommand::KillPane)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("no attached mux client for target session")
        );
        assert_eq!(server.clients[&1].session_id, Some(first_session));
        assert!(
            server.sessions[1].windows[0]
                .panes
                .iter()
                .any(|pane| pane.id == background_pane)
        );

        let session_count = server.sessions.len();
        let pane_count: usize = server
            .sessions
            .iter()
            .flat_map(|session| &session.windows)
            .map(|window| window.panes.len())
            .sum();
        let error = server
            .run_command(Some(usize::MAX), MuxCommand::KillPane)
            .unwrap_err();
        assert!(error.to_string().contains("does not exist"));
        assert_eq!(server.sessions.len(), session_count);
        assert_eq!(
            server
                .sessions
                .iter()
                .flat_map(|session| &session.windows)
                .map(|window| window.panes.len())
                .sum::<usize>(),
            pane_count
        );

        for pane in server
            .sessions
            .iter_mut()
            .flat_map(|session| &mut session.windows)
            .flat_map(|window| &mut window.panes)
        {
            pane.child.kill().unwrap();
        }
        drop(server);
        fs::remove_dir_all(directory).unwrap();
    }
}
