//! The daemon: it owns every pane, and paints one screen per attached client.
//!
//! This module holds the session tree (sessions, windows, panes), the clients,
//! and the event loop. The submodules each own one job:
//!
//! - [`input`]: what a keystroke, a click or a paste does
//! - [`command`]: the `mux ...` subcommands and queries
//! - [`render`]: painting one client's screen
//! - [`layout`]: where a window's panes sit, and the borders between them
//! - [`terminal`]: the PTY and `vt100` glue for one pane
//! - [`snapshot`]: the still copy of a pane that Vim mode moves around in
//! - [`bell`]: a pending bell and its animation
//! - [`ui`]: the bar, the tree, popups and previews
//! - [`themes`]: the theme picker
//! - [`process`]: what a pane is running, and its icon
//! - [`journal`]: a pane's replayable history on disk
//! - [`persist`]: the session tree on disk, and the daemon's private files

mod bell;
mod command;
mod input;
mod journal;
mod layout;
mod output_budget;
mod persist;
mod process;
mod pty_input;
mod render;
pub(crate) mod snapshot;
mod terminal;
mod themes;
mod ui;

use bell::*;
use journal::*;
use layout::*;
use output_budget::{OutputBudget, OutputPermit};
use persist::*;
use process::*;
use pty_input::PtyInput;
use snapshot::*;
use terminal::*;
use themes::*;
use ui::*;

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::{
    config::{BellStyle, Bindings, Theme},
    frame::{Frame, TerminalFeatures},
    protocol::{ClientMessage, Hello, ServerMessage, write_message},
    vim::{Position, VimMode, VimOutcome},
};

/// Shortest gap between two frames, so a burst of terminal output is coalesced
/// into one repaint instead of one per chunk.
const FRAME_INTERVAL: Duration = Duration::from_millis(8);
/// Repaint cadence while a bell animation is running.
const ANIMATION_INTERVAL: Duration = Duration::from_millis(16);
/// Shortest gap between two working-directory samples of one pane.
const CWD_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Shortest gap between two process-icon refreshes for one pane.
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Frames a client may fall behind before the daemon stops waiting for it.
const CLIENT_QUEUE_DEPTH: usize = 8;
/// Bound queued PTY chunks while readers apply backpressure to busy panes.
const EVENT_QUEUE_DEPTH: usize = 64;
const STORAGE_INTERVAL: Duration = Duration::from_millis(250);
const MAX_TERMINAL_CELLS: usize = 1_000_000;
/// How long a daemon that is shutting down waits for a client to take its
/// last messages.
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Cells one resize keystroke moves a divider.
const RESIZE_STEP: u16 = 2;
/// Lines one turn of the wheel scrolls.
const MOUSE_SCROLL_LINES: usize = 3;
const PANE_TERM: &str = "xterm-256color";
const PANE_COLORTERM: &str = "truecolor";

fn configure_pane_terminal(command: &mut CommandBuilder) {
    // A pane talks to mux's terminal emulator, not directly to the physical
    // terminal that launched the client. Advertising the physical terminal
    // makes remote panes depend on terminal-specific terminfo (for example,
    // xterm-kitty) and can even make zsh emit malformed colour sequences when
    // that entry is absent.
    command.env("TERM", PANE_TERM);
    command.env("COLORTERM", PANE_COLORTERM);
}

impl Event {
    /// Client events go first within a batch: two dozen freshly restored
    /// shells can queue thousands of output events in front of the attach
    /// waiting to draw the screen.
    fn is_client_event(&self) -> bool {
        matches!(
            self,
            Self::Connected(..) | Self::Client(..) | Self::Disconnected(..)
        )
    }
}

enum Event {
    Connected(usize, UnixStream),
    Client(usize, ClientMessage),
    Disconnected(usize),
    PtyOutput(usize, Vec<u8>, OutputPermit),
    PtyClosed(usize),
    ProcessIcon(usize, Option<i32>, &'static str),
    ClipboardCopied(usize, usize, Result<(), String>),
    ThemeSwitched(usize, String, Result<(), String>),
}

struct Pane {
    id: usize,
    master: Box<dyn MasterPty + Send>,
    writer: PtyInput,
    child: Box<dyn Child + Send + Sync>,
    child_pid: Option<u32>,
    parser: vt100::Parser<TerminalCallbacks>,
    cwd: PathBuf,
    cwd_sampled: Instant,
    process_icon: &'static str,
    process_sampled: Instant,
    process_pending: bool,
    history: PaneJournal,
}

struct ProcessSample {
    pane_id: usize,
    group: Option<i32>,
}

struct Window {
    panes: Vec<Pane>,
    layout: PaneLayout,
    active_pane: usize,
    previous_pane: usize,
    bell: Option<BellState>,
    /// While set, the active pane fills the client, the sidebar is hidden, and
    /// keys bypass mux. The layout underneath is untouched, so leaving the mode
    /// puts everything back exactly where it was.
    zoomed: bool,
    /// A name given with `rename-window`. Without one the window goes by
    /// whatever its active pane last set as the terminal title.
    name: Option<String>,
}

impl Window {
    fn new(pane: Pane) -> Self {
        let pane_id = pane.id;
        Self {
            panes: vec![pane],
            layout: PaneLayout::Pane(pane_id),
            active_pane: pane_id,
            previous_pane: pane_id,
            bell: None,
            zoomed: false,
            name: None,
        }
    }

    fn select_pane(&mut self, pane_id: usize) {
        if pane_id != self.active_pane {
            self.previous_pane = self.active_pane;
            self.active_pane = pane_id;
            // Looking at another pane is the end of looking at just one.
            self.zoomed = false;
        }
    }

    /// What this window is called: the name it was given, or failing that the
    /// title its active pane last set.
    fn label(&self) -> Option<&str> {
        if let Some(name) = &self.name {
            return Some(name);
        }
        self.panes
            .iter()
            .find(|pane| pane.id == self.active_pane)?
            .parser
            .callbacks()
            .title
            .as_deref()
    }

    fn active_process_icon(&self) -> &'static str {
        self.panes
            .iter()
            .find(|pane| pane.id == self.active_pane)
            .map_or(IDLE_ICON, |pane| pane.process_icon)
    }

    /// Moves focus off `pane_id` once it has left this window.
    fn forget_pane(&mut self, pane_id: usize) {
        let Some(first) = self.panes.first().map(|pane| pane.id) else {
            return;
        };
        if self.active_pane == pane_id {
            self.active_pane = first;
            self.previous_pane = first;
        } else if self.previous_pane == pane_id {
            self.previous_pane = self.active_pane;
        }
    }

    /// Where this window's panes sit inside `area`. Zoomed, only the active
    /// pane has a place; the rest keep their size and are not drawn.
    fn regions(&self, area: Rect) -> (Vec<(usize, Rect)>, Vec<Divider>) {
        window_regions(&self.layout, self.active_pane, self.zoomed, area)
    }
}

struct Session {
    id: usize,
    name: String,
    root: PathBuf,
    windows: Vec<Window>,
    current_window: usize,
}

impl Session {
    fn current_window_mut(&mut self) -> &mut Window {
        &mut self.windows[self.current_window]
    }

    /// Removes a window, keeping the current one in place where it survives.
    fn remove_window(&mut self, index: usize) {
        self.windows.remove(index);
        if !self.windows.is_empty() {
            self.current_window =
                window_index_after_removal(self.current_window, index, self.windows.len());
        }
    }
}

/// The daemon's end of one client connection.
///
/// Messages go through a writer thread, so a client that has stopped reading —
/// a suspended terminal, a dropped ssh link — fills its own queue instead of
/// blocking the event loop and stalling every pane the daemon owns.
struct ClientWriter {
    messages: mpsc::SyncSender<ServerMessage>,
    thread: thread::JoinHandle<()>,
    /// The connection itself, to cut a client off that never drains its
    /// last messages while the daemon shuts down.
    stream: Option<UnixStream>,
}

impl ClientWriter {
    fn spawn(mut stream: UnixStream) -> Self {
        let (messages, receiver) = mpsc::sync_channel(CLIENT_QUEUE_DEPTH);
        // No write timeout: a client whose terminal has stalled (a frozen ssh
        // link, output paused while text is selected) is slow, not gone. Its
        // writer blocks, its queue fills, and the event loop drops frames for
        // it and repaints in full once it drains. A client that really is gone
        // closes its socket, which ends the write with an error.
        let control = stream.try_clone().ok();
        let thread = thread::spawn(move || {
            while let Ok(message) = receiver.recv() {
                if write_message(&mut stream, &message).is_err() {
                    break;
                }
            }
            // Closing the connection is what tells a client the daemon is done
            // with it, even when the queue was too full for a last message.
            let _ = stream.shutdown(std::net::Shutdown::Both);
        });
        Self {
            messages,
            thread,
            stream: control,
        }
    }

    /// Waits (bounded) for everything queued to reach the client, then closes
    /// it: the daemon exits right after shutdown, so it cannot leave the
    /// writer thread to drain on its own.
    fn finish(self) {
        drop(self.messages);
        let deadline = Instant::now() + CLIENT_WRITE_TIMEOUT;
        while !self.thread.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if !self.thread.is_finished()
            && let Some(stream) = &self.stream
        {
            // Unblocks a write to a client that stopped reading for good.
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        let _ = self.thread.join();
    }

    /// Queues `message`, reporting whether the client took it. A refusal means
    /// the client is behind, not that it is gone: its reader thread is what
    /// decides that.
    fn send(&self, message: ServerMessage) -> bool {
        self.messages.try_send(message).is_ok()
    }
}

struct Client {
    writer: ClientWriter,
    cols: u16,
    rows: u16,
    cwd: PathBuf,
    session_id: Option<usize>,
    previous_session_id: Option<usize>,
    bindings: Bindings,
    clipboard_command: Vec<String>,
    terminal_clipboard: bool,
    theme: Theme,
    /// What the theme picker runs to switch theme, and where it finds the
    /// themes to offer.
    theme_command: Vec<String>,
    theme_directory: Option<PathBuf>,
    /// Whether this client's terminal hands mux the mouse.
    mouse: bool,
    /// How this client shows a pending bell.
    bell_style: BellStyle,
    default_cursor_shape: crate::frame::CursorShape,
    /// What this client's terminal is painted for.
    terminal: TerminalFeatures,
    /// Which glyphs this client's font has for the sidebar.
    glyphs: crate::config::Glyphs,
    /// Whether this client's terminal has focus, as it last reported. A
    /// terminal that never reports focus is assumed to have it.
    focused: bool,
    vim: HashMap<usize, VimState>,
    tree: Option<TreeState>,
    themes: Option<ThemePicker>,
    /// Whether leader mode is held, waiting for the command that ends it.
    leader: bool,
    /// The key that entered leader mode, which a second press arms literal mode with.
    leader_key: Option<crate::protocol::Key>,
    /// Whether the next key goes straight to the pane, bindings bypassed.
    literal: bool,
    rename: Option<RenameState>,
    confirmation: Option<Confirmation>,
    message: Option<StatusMessage>,
    initialized: bool,
    /// What this client's terminal is currently showing.
    frame: Frame,
    /// Reused buffer for the frame being painted.
    scratch: Frame,
}

impl Client {
    fn new(writer: UnixStream) -> Self {
        Self {
            writer: ClientWriter::spawn(writer),
            cols: 80,
            rows: 24,
            cwd: PathBuf::new(),
            session_id: None,
            previous_session_id: None,
            bindings: Bindings::defaults(),
            clipboard_command: vec!["yank".into()],
            terminal_clipboard: false,
            theme_command: vec!["theme".into()],
            theme_directory: None,
            theme: Theme::default(),
            mouse: false,
            bell_style: BellStyle::default(),
            default_cursor_shape: crate::frame::CursorShape::default(),
            terminal: TerminalFeatures::FULL,
            glyphs: crate::config::Glyphs::default(),
            focused: true,
            vim: HashMap::new(),
            tree: None,
            themes: None,
            leader: false,
            leader_key: None,
            literal: false,
            rename: None,
            confirmation: None,
            message: None,
            initialized: false,
            frame: Frame::default(),
            scratch: Frame::default(),
        }
    }

    /// The selected theme is a temporary preview while the picker is open.
    fn rendered_theme(&self) -> Theme {
        self.themes
            .as_ref()
            .map(ThemePicker::theme)
            .unwrap_or(self.theme)
    }
}

struct Server {
    socket_path: PathBuf,
    zsh_startup: ZshStartup,
    persistence: Persistence,
    state_writer: StateWriter,
    process_sampler: Sender<Vec<ProcessSample>>,
    events: mpsc::SyncSender<Event>,
    sessions: Vec<Session>,
    clients: HashMap<usize, Client>,
    next_session_id: usize,
    next_pane_id: usize,
    last_active_pane: Option<usize>,
    /// The colours panes are told about when they query the terminal. Clients
    /// carry their own copy for painting; this is the one the daemon answers
    /// with, taken from the most recent client or `set-theme`.
    theme: Theme,
    dirty: bool,
    /// The panes some focused client is showing as its active pane. Programs
    /// that asked for focus reports (mode 1004) are told when they join or
    /// leave it.
    focused_panes: HashSet<usize>,
    /// Set when the session tree has changed and the copy on disk is stale.
    /// Writing it costs two fsyncs, so it is deferred until the daemon settles
    /// instead of running on every keystroke that moves the active pane.
    state_dirty: bool,
}

fn validate_terminal_size(cols: u16, rows: u16) -> Result<()> {
    if cols == 0 || rows == 0 || usize::from(cols) * usize::from(rows) > MAX_TERMINAL_CELLS {
        bail!("terminal dimensions must be nonzero and contain at most {MAX_TERMINAL_CELLS} cells");
    }
    Ok(())
}

pub fn run(socket_path: &Path) -> Result<()> {
    if nix::unistd::getsid(None)? != nix::unistd::getpid() {
        nix::unistd::setsid().context("start daemon session")?;
    }
    let persistence = Persistence::open()?;
    // Only one daemon may own this state: a second one would restore the same
    // sessions, spawn duplicate shells and interleave its writes into the same
    // pane journals. The loser exits and the client that started it connects to
    // the winner's socket instead.
    let Some(_lock) = persistence.lock()? else {
        return Ok(());
    };
    prepare_socket_directory(socket_path)?;
    if socket_path.exists() {
        fs::remove_file(socket_path)
            .with_context(|| format!("remove stale socket {}", socket_path.display()))?;
    }
    // A socket is created with the process umask, so tighten it for the bind
    // rather than widening the window between binding and the chmod below.
    let previous_umask = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("bind socket {}", socket_path.display()));
    nix::sys::stat::umask(previous_umask);
    let listener = listener?;
    set_private_permissions(socket_path)?;
    let zsh_startup = ZshStartup::create(&persistence.directory)?;
    let persisted_state = persistence.load()?;
    let state_writer = persistence.state_writer();

    let (sender, receiver) = mpsc::sync_channel(EVENT_QUEUE_DEPTH);
    accept_clients(listener, sender.clone());
    let process_sampler = process_icon_sampler(sender.clone());
    let mut server = Server {
        socket_path: socket_path.to_path_buf(),
        zsh_startup,
        persistence,
        state_writer,
        process_sampler,
        events: sender,
        sessions: Vec::new(),
        clients: HashMap::new(),
        next_session_id: 0,
        next_pane_id: 0,
        last_active_pane: None,
        theme: Theme::default(),
        focused_panes: HashSet::new(),
        dirty: false,
        state_dirty: false,
    };
    if let Some(state) = persisted_state {
        server.restore(state)?;
    }
    server.compact_journals()?;
    for pane in server.panes() {
        pane.parser.screen().flush_history_backing();
    }
    release_unused_memory();
    let result = server.event_loop(receiver);
    let _ = fs::remove_file(socket_path);
    result
}

fn accept_clients(listener: UnixListener, sender: mpsc::SyncSender<Event>) {
    thread::spawn(move || {
        let next_id = Arc::new(AtomicUsize::new(1));
        for connection in listener.incoming() {
            let Ok(stream) = connection else { break };
            let id = next_id.fetch_add(1, Ordering::Relaxed);
            let Ok(writer) = stream.try_clone() else {
                continue;
            };
            let connection_sender = sender.clone();
            if connection_sender
                .send(Event::Connected(id, writer))
                .is_err()
            {
                break;
            }
            thread::spawn(move || {
                let mut reader = stream;
                loop {
                    match crate::protocol::read_client_message(&mut reader) {
                        Ok(Some(message)) => {
                            if connection_sender.send(Event::Client(id, message)).is_err() {
                                return;
                            }
                        }
                        _ => {
                            let _ = connection_sender.send(Event::Disconnected(id));
                            return;
                        }
                    }
                }
            });
        }
    });
}

fn process_icon_sampler(events: mpsc::SyncSender<Event>) -> Sender<Vec<ProcessSample>> {
    let (sender, receiver) = mpsc::channel::<Vec<ProcessSample>>();
    thread::spawn(move || {
        while let Ok(samples) = receiver.recv() {
            let processes = processes();
            for sample in samples {
                let icon = sample
                    .group
                    .and_then(|group| foreground_program(&processes, group))
                    .map_or(IDLE_ICON, program_icon);
                if events
                    .send(Event::ProcessIcon(sample.pane_id, sample.group, icon))
                    .is_err()
                {
                    return;
                }
            }
        }
    });
    sender
}

impl Server {
    fn panes(&self) -> impl Iterator<Item = &Pane> {
        self.sessions
            .iter()
            .flat_map(|session| &session.windows)
            .flat_map(|window| &window.panes)
    }

    fn panes_mut(&mut self) -> impl Iterator<Item = &mut Pane> {
        self.sessions
            .iter_mut()
            .flat_map(|session| &mut session.windows)
            .flat_map(|window| &mut window.panes)
    }

    /// The session, window and pane indices of `pane_id`.
    fn locate_pane(&self, pane_id: usize) -> Option<(usize, usize, usize)> {
        self.sessions
            .iter()
            .enumerate()
            .find_map(|(session_index, session)| {
                session
                    .windows
                    .iter()
                    .enumerate()
                    .find_map(|(window_index, window)| {
                        let pane_index = window.panes.iter().position(|pane| pane.id == pane_id)?;
                        Some((session_index, window_index, pane_index))
                    })
            })
    }

    fn session_index(&self, session_id: usize) -> Option<usize> {
        self.sessions
            .iter()
            .position(|session| session.id == session_id)
    }

    fn session(&self, session_id: usize) -> Option<&Session> {
        self.sessions
            .iter()
            .find(|session| session.id == session_id)
    }

    fn restore(&mut self, state: PersistedState) -> Result<()> {
        // Replaying journals is the slow part of starting up, and panes have
        // nothing to say to each other while it happens, so every pane in the
        // saved state is replayed at once rather than one after another.
        let mut replayed = self.replay_saved_panes(&state)?;
        let mut session_ids = HashSet::new();
        let mut pane_ids = HashSet::new();
        let mut sessions = Vec::with_capacity(state.sessions.len());
        for saved_session in state.sessions {
            if !session_ids.insert(saved_session.id) {
                bail!(
                    "persisted state contains duplicate session {}",
                    saved_session.id
                );
            }
            if saved_session.windows.is_empty()
                || saved_session.current_window >= saved_session.windows.len()
            {
                bail!(
                    "persisted session {:?} has no active window",
                    saved_session.name
                );
            }
            let mut windows = Vec::with_capacity(saved_session.windows.len());
            for saved_window in saved_session.windows {
                if saved_window.panes.is_empty() {
                    bail!(
                        "persisted session {:?} contains an empty window",
                        saved_session.name
                    );
                }
                let mut layout_ids = Vec::new();
                saved_window.layout.pane_ids(&mut layout_ids);
                let mut saved_ids: Vec<_> = saved_window.panes.iter().map(|pane| pane.id).collect();
                layout_ids.sort_unstable();
                saved_ids.sort_unstable();
                if layout_ids != saved_ids
                    || !saved_ids.contains(&saved_window.active_pane)
                    || saved_ids.iter().any(|pane_id| !pane_ids.insert(*pane_id))
                {
                    bail!(
                        "persisted session {:?} has an invalid pane layout",
                        saved_session.name
                    );
                }
                let mut panes = Vec::with_capacity(saved_window.panes.len());
                for saved_pane in saved_window.panes {
                    let ReplayedPane {
                        parser,
                        valid_length,
                        history_length,
                        replayed: had_history,
                    } = replayed
                        .remove(&saved_pane.id)
                        .context("a saved pane was not replayed")?;
                    let history = match self
                        .persistence
                        .resume_pane_history(saved_pane.id, valid_length)
                    {
                        Ok(file) => PaneJournal::new(file, valid_length),
                        Err(_) => {
                            PaneJournal::new(self.persistence.new_pane_history(saved_pane.id)?, 0)
                        }
                    };
                    let mut pane = self.spawn_pane(
                        saved_pane.id,
                        &saved_pane.cwd,
                        saved_pane.cols,
                        saved_pane.rows,
                        history,
                        parser,
                        false,
                    )?;
                    pane.history.compact_soon();
                    if had_history && valid_length != history_length {
                        let _ = pane.history.truncate(valid_length);
                    }
                    if let Some(correction) = restored_prompt_correction(&pane.parser) {
                        let _ = pane.history.append_output(&correction, None);
                        process_terminal_bytes(&mut pane.parser, &correction);
                    }
                    panes.push(pane);
                }
                windows.push(Window {
                    panes,
                    layout: saved_window.layout,
                    active_pane: saved_window.active_pane,
                    previous_pane: saved_window.active_pane,
                    bell: None,
                    zoomed: saved_window.zoomed,
                    name: saved_window.name,
                });
            }
            sessions.push(Session {
                id: saved_session.id,
                name: saved_session.name,
                root: saved_session.root,
                windows,
                current_window: saved_session.current_window,
            });
        }
        if session_ids.iter().any(|id| *id >= state.next_session_id)
            || pane_ids.iter().any(|id| *id >= state.next_pane_id)
        {
            bail!("persisted state has invalid next identifiers");
        }
        if state
            .last_active_pane
            .is_some_and(|pane_id| !pane_ids.contains(&pane_id))
        {
            bail!("persisted state has an invalid last active pane");
        }
        self.sessions = sessions;
        self.next_session_id = state.next_session_id;
        self.next_pane_id = state.next_pane_id;
        self.last_active_pane = state.last_active_pane;
        Ok(())
    }

    fn persisted_state(&self) -> PersistedState {
        let sessions = self
            .sessions
            .iter()
            .map(|session| PersistedSession {
                id: session.id,
                name: session.name.clone(),
                root: session.root.clone(),
                windows: session
                    .windows
                    .iter()
                    .map(|window| PersistedWindow {
                        panes: window
                            .panes
                            .iter()
                            .map(|pane| {
                                let (rows, cols) = pane.parser.screen().size();
                                PersistedPane {
                                    id: pane.id,
                                    cwd: pane.cwd.clone(),
                                    rows,
                                    cols,
                                }
                            })
                            .collect(),
                        layout: window.layout.clone(),
                        active_pane: window.active_pane,
                        zoomed: window.zoomed,
                        name: window.name.clone(),
                    })
                    .collect(),
                current_window: session.current_window,
            })
            .collect();
        PersistedState {
            version: STATE_VERSION,
            next_session_id: self.next_session_id,
            next_pane_id: self.next_pane_id,
            sessions,
            last_active_pane: self.last_active_pane,
        }
    }

    fn event_loop(&mut self, receiver: Receiver<Event>) -> Result<()> {
        let mut last_render = Instant::now() - FRAME_INTERVAL;
        let mut last_settle = Instant::now();
        loop {
            if last_settle.elapsed() >= STORAGE_INTERVAL {
                self.settle();
                last_settle = Instant::now();
            }
            self.sample_process_icons();
            let event = match receiver.try_recv() {
                Ok(event) => Some(event),
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
                Err(mpsc::TryRecvError::Empty) => {
                    // Storage can settle while a bell or popup still needs
                    // timed repaints. Recompute the deadline after settling,
                    // since a storage failure can itself require a repaint.
                    self.settle();
                    last_settle = Instant::now();
                    match self.next_wake(last_render) {
                        Some(deadline) => {
                            let timeout = deadline.saturating_duration_since(Instant::now());
                            match receiver.recv_timeout(timeout) {
                                Ok(event) => Some(event),
                                Err(mpsc::RecvTimeoutError::Timeout) => None,
                                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                            }
                        }
                        // Nothing is pending: sleep until a client or pane speaks up.
                        None => match receiver.recv() {
                            Ok(event) => Some(event),
                            Err(_) => return Ok(()),
                        },
                    }
                }
            };
            if let Some(event) = event {
                let mut batch = vec![event];
                for _ in 0..512 {
                    match receiver.try_recv() {
                        Ok(event) => batch.push(event),
                        Err(_) => break,
                    }
                }
                // Stable, so panes still see their own output in order.
                batch.sort_by_key(|event| !event.is_client_event());
                for event in batch {
                    if self.dispatch(event) {
                        return Ok(());
                    }
                }
                self.report_focus();
            }
            self.dirty |= self.expire_messages();
            self.dirty |= self.advance_bell_animations();
            let now = Instant::now();
            let expired = self
                .panes_mut()
                .map(|pane| pane.parser.callbacks_mut().expire_synchronized_output(now))
                .fold(false, |any, expired| any | expired);
            self.dirty |= expired;
            if self.dirty && last_render.elapsed() >= FRAME_INTERVAL {
                self.dirty = false;
                self.render_all();
                last_render = Instant::now();
            }
        }
    }

    /// Tells programs that asked (mode 1004) when they gain or lose focus:
    /// when the terminal showing them does, and when mux moves between panes
    /// or windows, as tmux does with `focus-events` on.
    fn report_focus(&mut self) {
        let focused: HashSet<usize> = self
            .clients
            .iter()
            .filter(|(_, client)| client.initialized && client.focused)
            .filter_map(|(id, _)| self.active_pane(*id).map(|pane| pane.id))
            .collect();
        if focused == self.focused_panes {
            return;
        }
        let gained: Vec<usize> = focused.difference(&self.focused_panes).copied().collect();
        let lost: Vec<usize> = self.focused_panes.difference(&focused).copied().collect();
        self.focused_panes = focused;
        for (panes, report) in [(lost, b"\x1b[O"), (gained, b"\x1b[I")] {
            for pane_id in panes {
                if let Some(pane) = self.pane_mut(pane_id)
                    && pane.parser.screen().focus_reporting()
                {
                    // A pane whose program just exited cannot take it.
                    let _ = pane.writer.send(report);
                }
            }
        }
    }

    /// When the next repaint is due, or `None` when the daemon can sleep until
    /// something happens.
    fn next_wake(&self, last_render: Instant) -> Option<Instant> {
        let attached = self.clients.values().any(|client| client.initialized);
        // A message expiring is a change no further event would announce.
        self.clients
            .values()
            .filter_map(|client| client.message.as_ref().map(|message| message.expires))
            .chain(self.dirty.then_some(last_render + FRAME_INTERVAL))
            .chain(
                self.bells_animating()
                    .then_some(Instant::now() + ANIMATION_INTERVAL),
            )
            .chain(
                self.panes()
                    .filter(|pane| attached && !pane.process_pending)
                    .map(|pane| pane.process_sampled + PROCESS_POLL_INTERVAL),
            )
            .chain(self.panes().filter_map(|pane| {
                pane.parser
                    .callbacks()
                    .synchronized_output
                    .as_ref()
                    .map(|update| update.expires)
            }))
            .min()
    }

    fn bells_animating(&self) -> bool {
        self.bells_shimmer()
            && self
                .sessions
                .iter()
                .flat_map(|session| &session.windows)
                .any(|window| window.bell.is_some())
    }

    /// Whether any attached client draws the moving bell highlight. Nobody
    /// watching one means nothing to animate, and no frames to send.
    fn bells_shimmer(&self) -> bool {
        self.clients
            .values()
            .any(|client| client.bell_style == BellStyle::Shimmer)
    }

    /// Finishes the durable work that was deferred while the daemon was busy.
    fn settle(&mut self) {
        let mut failure = None;
        for pane in self.panes_mut() {
            if let Err(error) = pane.history.poll_failure() {
                failure = Some((pane.id, error));
            }
        }
        if let Some((pane_id, error)) = failure {
            self.note_failure(&format!("pane {pane_id} history"), error);
        }
        // Nothing is happening, so this is the moment to rewrite a journal that
        // has outgrown what it holds. Left alone it is replayed in full at every
        // startup, which is the slowest thing the daemon ever does.
        if let Some(pane_id) = self.overgrown_journals().first().copied()
            && let Err(error) = self.compact_journal(pane_id)
        {
            self.note_failure(&format!("pane {pane_id} history"), error);
        }
        if self.state_dirty {
            match self.state_writer.save(self.persisted_state()) {
                Ok(()) => self.state_dirty = false,
                Err(error) => self.note_failure("save sessions", error),
            }
        }
        if let Some(error) = self.state_writer.poll_failure() {
            self.state_dirty = true;
            self.note_failure("save sessions", error);
        }
    }

    /// Rewrites journals that have outgrown [`MAX_JOURNAL_BYTES`] so restoring
    /// a long-lived pane stays fast and its history stays bounded on disk.
    fn compact_journals(&mut self) -> Result<()> {
        for pane_id in self.overgrown_journals() {
            self.compact_journal(pane_id)?;
        }
        Ok(())
    }

    fn overgrown_journals(&self) -> Vec<usize> {
        self.panes()
            .filter(|pane| pane.history.needs_compaction())
            .map(|pane| pane.id)
            .collect()
    }

    fn compact_journal(&mut self, pane_id: usize) -> Result<()> {
        let path = self.persistence.pane_history_path(pane_id);
        let pane = self
            .pane_mut(pane_id)
            .context("pane vanished during compaction")?;
        let records = compacted_journal_records(pane.parser.screen_mut())?;
        pane.history.replace_async(path, records)
    }

    fn expire_messages(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        for client in self.clients.values_mut() {
            if client
                .message
                .as_ref()
                .is_some_and(|message| message.expired(now))
            {
                client.message = None;
                changed = true;
            }
        }
        changed
    }

    fn set_message(&mut self, id: usize, text: String) {
        if let Some(client) = self.clients.get_mut(&id) {
            client.message = Some(StatusMessage::new(text));
        }
    }

    /// Records that the session tree changed. The write happens in
    /// [`Self::settle`], once the daemon has nothing better to do.
    fn save_state_soon(&mut self) {
        self.state_dirty = true;
    }

    /// Reports something that went wrong without taking the daemon (and every
    /// shell it owns) down with it.
    fn note_failure(&mut self, what: &str, error: anyhow::Error) {
        let text = format!("{what}: {error:#}");
        for client in self
            .clients
            .values_mut()
            .filter(|client| client.initialized)
        {
            client.message = Some(StatusMessage::new(text.clone()));
        }
        self.dirty = true;
    }

    /// Runs one event and reports whether the daemon should stop.
    ///
    /// An error, or even a panic, becomes a message on screen: ending the
    /// daemon would hang up every PTY and kill every program running in them.
    fn dispatch(&mut self, event: Event) -> bool {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.handle_event(event)));
        match result {
            Ok(Ok(stop)) => stop,
            Ok(Err(error)) => {
                self.note_failure("mux", error);
                false
            }
            Err(panic) => {
                self.note_failure(
                    "mux internal error",
                    anyhow::anyhow!("{}", panic_message(&*panic)),
                );
                false
            }
        }
    }

    fn handle_event(&mut self, event: Event) -> Result<bool> {
        match event {
            Event::Connected(id, writer) => {
                self.clients.insert(id, Client::new(writer));
            }
            Event::Disconnected(id) => {
                self.track_active_pane(id);
                self.clients.remove(&id);
            }
            Event::PtyOutput(pane_id, bytes, permit) => {
                self.handle_pty_output(pane_id, &bytes, permit);
            }
            Event::PtyClosed(pane_id) => {
                self.close_pane(pane_id)?;
                self.dirty = true;
            }
            Event::ProcessIcon(pane_id, group, icon) => {
                if let Some(pane) = self.pane_mut(pane_id) {
                    pane.process_pending = false;
                    if pane.master.process_group_leader() != group {
                        pane.process_sampled = Instant::now() - PROCESS_POLL_INTERVAL;
                    } else if pane.process_icon != icon {
                        pane.process_icon = icon;
                        self.dirty = true;
                    }
                }
            }
            Event::ClipboardCopied(id, bytes, result) => {
                self.set_message(
                    id,
                    match result {
                        Ok(()) => format!("yanked {bytes} bytes"),
                        Err(error) => format!("clipboard: {error}"),
                    },
                );
                self.dirty = true;
            }
            Event::ThemeSwitched(id, name, result) => {
                self.set_message(
                    id,
                    match result {
                        Ok(()) => format!("theme: {name}"),
                        Err(error) => format!("theme: {error}"),
                    },
                );
                self.dirty = true;
            }
            Event::Client(_, ClientMessage::Shutdown) => {
                // The last chance to reach the disk, so this one is not deferred.
                self.state_writer.flush(self.persisted_state())?;
                for pane in self.panes_mut() {
                    let _ = pane.history.flush();
                }
                for client in self.clients.values() {
                    client.writer.send(ServerMessage::Detached);
                }
                // The daemon is about to exit, so wait for those to land.
                for (_, client) in self.clients.drain() {
                    client.writer.finish();
                }
                return Ok(true);
            }
            Event::Client(
                id,
                ClientMessage::Query {
                    pane_id,
                    query,
                    json,
                },
            ) => {
                let response = match self.listing(pane_id, query, json) {
                    Ok(lines) => ServerMessage::Listing(lines),
                    Err(error) => ServerMessage::Error(format!("{error:#}")),
                };
                if let Some(client) = self.clients.get(&id) {
                    client.writer.send(response);
                }
            }
            Event::Client(id, ClientMessage::Command { pane_id, command }) => {
                let result = self.run_command(pane_id, command);
                if let Some(client) = self.clients.get_mut(&id) {
                    let response = match result {
                        Ok(()) => ServerMessage::Done,
                        Err(error) => ServerMessage::Error(format!("{error:#}")),
                    };
                    client.writer.send(response);
                }
                self.dirty = true;
            }
            Event::Client(id, ClientMessage::Hello(hello)) => {
                if let Err(error) = self.initialize_client(id, *hello) {
                    if let Some(client) = self.clients.remove(&id) {
                        client
                            .writer
                            .send(ServerMessage::Error(format!("{error:#}")));
                    }
                    return Err(error);
                }
            }
            Event::Client(id, ClientMessage::Resize { cols, rows }) => {
                validate_terminal_size(cols, rows)?;
                if let Some(client) = self.clients.get_mut(&id) {
                    client.cols = cols;
                    client.rows = rows;
                    // A terminal may reflow, crop or clear its screen when it
                    // is resized, and a quick resize back to the same size
                    // leaves the frame size unchanged. What it shows now is
                    // unknown, so the next frame is painted in full.
                    client.frame = Frame::default();
                }
                self.resize_active(id)?;
                self.rebuild_vim(id);
                self.save_state_soon();
                self.dirty = true;
            }
            Event::Client(id, ClientMessage::Key(key)) => {
                self.track_active_pane(id);
                self.handle_key(id, key)?;
            }
            Event::Client(id, ClientMessage::Mouse(mouse)) => {
                self.handle_mouse(id, mouse)?;
                self.dirty = true;
            }
            Event::Client(id, ClientMessage::Paste(text)) => {
                self.track_active_pane(id);
                self.handle_paste(id, text)?;
            }
            Event::Client(id, ClientMessage::Detach) => self.detach(id)?,
            Event::Client(id, ClientMessage::Focus(focused)) => {
                if let Some(client) = self.clients.get_mut(&id) {
                    client.focused = focused;
                }
            }
        }
        Ok(false)
    }

    fn handle_pty_output(&mut self, pane_id: usize, bytes: &[u8], permit: OutputPermit) {
        let colors = TerminalColors::from(&self.theme);
        let Some(pane) = self.pane_mut(pane_id) else {
            return;
        };
        let history_failure = pane.history.append_output(bytes, Some(permit)).err();
        let previous_bells = pane.parser.callbacks().bell_count;
        let had_prompt = pane.parser.callbacks().prompt_ready.is_some();
        pane.parser.callbacks_mut().set_colors(colors);
        process_terminal_bytes(&mut pane.parser, bytes);
        let callbacks = pane.parser.callbacks_mut();
        let clipboard_writes = std::mem::take(&mut callbacks.clipboard_writes);
        let bell_events = callbacks.bell_count.saturating_sub(previous_bells) as usize;
        let reached_prompt = !had_prompt && callbacks.prompt_ready.is_some();
        let responses = std::mem::take(&mut callbacks.responses);
        // A pane whose shell has just died cannot take a reply.
        let input_failure = if responses.is_empty() {
            None
        } else {
            pane.writer.send(&responses).err()
        };
        // Sampling the shell's directory costs a system call, so do it when a
        // prompt appears and otherwise only occasionally.
        let mut cwd_changed = false;
        if reached_prompt || pane.cwd_sampled.elapsed() >= CWD_POLL_INTERVAL {
            pane.cwd_sampled = Instant::now();
            if let Some(pid) = pane.child_pid
                && let Some(cwd) = process_cwd(pid)
                && cwd != pane.cwd
            {
                pane.cwd = cwd;
                cwd_changed = true;
            }
        }
        self.dirty = true;
        if let Some(error) = history_failure {
            self.note_failure(&format!("pane {pane_id} history"), error);
        }
        if let Some(error) = input_failure {
            self.note_failure(&format!("pane {pane_id} input"), error);
        }
        if !clipboard_writes.is_empty() {
            let session_id = self
                .locate_pane(pane_id)
                .map(|(session_index, _, _)| self.sessions[session_index].id);
            for client in self
                .clients
                .values()
                .filter(|client| client.session_id == session_id)
            {
                for clipboard in &clipboard_writes {
                    client.writer.send(ServerMessage::Clipboard {
                        selection: clipboard.selection.clone(),
                        data: clipboard.data.clone(),
                    });
                }
            }
        }
        if bell_events > 0 {
            self.ring_bell(pane_id, bell_events);
        }
        if cwd_changed {
            self.save_state_soon();
        }
    }

    fn initialize_client(&mut self, id: usize, hello: Hello) -> Result<()> {
        validate_terminal_size(hello.cols, hello.rows)?;
        let Hello {
            cols,
            rows,
            cwd,
            session,
            bindings,
            clipboard_command,
            terminal_clipboard,
            theme,
            theme_command,
            theme_directory,
            mouse,
            bell_style,
            terminal,
            glyphs,
            default_cursor_shape,
        } = hello;
        let session_id = if let Some(name) = session {
            match self.sessions.iter().find(|session| session.name == name) {
                Some(session) => session.id,
                None => self.create_session(name, cwd.clone(), cols, rows)?,
            }
        } else if let Some(session_id) = self.last_active_session_id() {
            session_id
        } else if let Some(session) = self.sessions.first() {
            session.id
        } else {
            self.create_session(automatic_session_name(1), cwd.clone(), cols, rows)?
        };
        let client = self
            .clients
            .get_mut(&id)
            .context("client disconnected during setup")?;
        client.cols = cols;
        client.rows = rows;
        client.cwd = cwd;
        client.bindings = bindings;
        client.clipboard_command = clipboard_command;
        client.terminal_clipboard = terminal_clipboard;
        client.theme = theme;
        client.theme_command = theme_command;
        client.theme_directory = theme_directory;
        client.mouse = mouse;
        client.bell_style = bell_style;
        client.default_cursor_shape = default_cursor_shape;
        client.terminal = terminal;
        client.glyphs = glyphs;
        client.initialized = true;
        self.theme = theme;
        self.set_client_session(id, session_id);
        if let Some(session_index) = self.session_index(session_id) {
            self.answer_bell(session_index);
        }
        self.show_active(id)?;
        self.dirty = true;
        Ok(())
    }

    fn create_session(
        &mut self,
        name: String,
        root: PathBuf,
        cols: u16,
        rows: u16,
    ) -> Result<usize> {
        if self.sessions.iter().any(|session| session.name == name) {
            bail!("session {name:?} already exists");
        }
        let id = self.next_session_id;
        self.next_session_id += 1;
        let window = self.create_window(&root, cols, rows, bar_width(1))?;
        self.sessions.push(Session {
            id,
            name,
            root,
            windows: vec![window],
            current_window: 0,
        });
        Ok(id)
    }

    fn create_window(
        &mut self,
        cwd: &Path,
        cols: u16,
        rows: u16,
        bar_width: u16,
    ) -> Result<Window> {
        let pane = self.create_pane(cwd, cols.saturating_sub(bar_width).max(1), rows.max(1))?;
        Ok(Window::new(pane))
    }

    fn create_pane(&mut self, cwd: &Path, cols: u16, rows: u16) -> Result<Pane> {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        let mut history = PaneJournal::new(self.persistence.new_pane_history(id)?, 0);
        history.append_resize(rows.max(1), cols.max(1))?;
        self.spawn_pane(id, cwd, cols, rows, history, new_parser(rows, cols), true)
    }

    /// Every saved pane's scrollback, read back off disk side by side.
    fn replay_saved_panes(&self, state: &PersistedState) -> Result<HashMap<usize, ReplayedPane>> {
        let prepared: Vec<_> = state
            .sessions
            .iter()
            .flat_map(|session| &session.windows)
            .flat_map(|window| &window.panes)
            .map(|pane| {
                // The files are opened here: a worker only reads and parses.
                let restored = self.persistence.restored_pane_history(pane.id).ok();
                let backing = self.persistence.new_scrollback_backing().ok();
                (pane.id, pane.rows, pane.cols, restored, backing)
            })
            .collect();
        let mut replayed = HashMap::with_capacity(prepared.len());
        thread::scope(|scope| {
            let workers: Vec<_> = prepared
                .into_iter()
                .map(|(id, rows, cols, restored, backing)| {
                    scope.spawn(move || {
                        let mut parser = new_parser(rows, cols);
                        if let Some(backing) = backing {
                            parser.screen_mut().set_history_backing(backing);
                        }
                        let pane = match restored {
                            None => ReplayedPane {
                                parser,
                                valid_length: 0,
                                history_length: 0,
                                replayed: false,
                            },
                            Some((reader, history_length)) => {
                                match replay_pane_journal(&mut parser, reader) {
                                    Ok(valid_length) => ReplayedPane {
                                        parser,
                                        valid_length,
                                        history_length,
                                        replayed: true,
                                    },
                                    // A corrupt journal costs this pane its
                                    // scrollback rather than its session.
                                    Err(_) => ReplayedPane {
                                        parser: new_parser(rows, cols),
                                        valid_length: 0,
                                        history_length,
                                        replayed: true,
                                    },
                                }
                            }
                        };
                        (id, pane)
                    })
                })
                .collect();
            for worker in workers {
                let Ok((id, pane)) = worker.join() else {
                    bail!("a pane's history could not be read");
                };
                replayed.insert(id, pane);
            }
            Ok(())
        })?;
        Ok(replayed)
    }

    /// Spawns a shell around `parser`, which a restored pane has already
    /// filled in with its scrollback.
    #[allow(clippy::too_many_arguments)]
    fn spawn_pane(
        &self,
        id: usize,
        cwd: &Path,
        cols: u16,
        rows: u16,
        history: PaneJournal,
        mut parser: vt100::Parser<TerminalCallbacks>,
        needs_backing: bool,
    ) -> Result<Pane> {
        // Clipboard writes are live terminal actions, not restorable screen state.
        parser.callbacks_mut().clipboard_writes.clear();
        // A restored journal belongs to the old process, including any pending
        // redraw and terminal queries. The new shell starts with the live screen.
        parser.callbacks_mut().synchronized_output = None;
        parser.callbacks_mut().responses.clear();
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("open PTY")?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut command = CommandBuilder::new(shell);
        command.cwd(cwd);
        configure_pane_terminal(&mut command);
        command.env("MUX", self.socket_path.as_os_str());
        command.env("MUX_PANE", id.to_string());
        // Started from inside tmux, the inherited variables would point programs
        // at the tmux pane mux itself is running in, not at this one.
        command.env_remove("TMUX");
        command.env_remove("TMUX_PANE");
        if command
            .get_argv()
            .first()
            .and_then(|program| Path::new(program).file_name())
            .is_some_and(|name| name == "zsh")
        {
            self.zsh_startup.configure(&mut command);
        }
        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("start shell in {}", cwd.display()))?;
        let child_pid = child.process_id();
        let mut reader = pair.master.try_clone_reader().context("clone PTY reader")?;
        let writer = PtyInput::spawn(pair.master.take_writer().context("take PTY writer")?);
        let sender = self.events.clone();
        let output_budget = OutputBudget::default();
        thread::spawn(move || {
            let mut buffer = vec![0; 32 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => {
                        let _ = sender.send(Event::PtyClosed(id));
                        return;
                    }
                    Ok(length) => {
                        let permit = output_budget.acquire(length);
                        if sender
                            .send(Event::PtyOutput(id, buffer[..length].to_vec(), permit))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }
        });
        if needs_backing {
            parser
                .screen_mut()
                .set_history_backing(self.persistence.new_scrollback_backing()?);
        }
        Ok(Pane {
            id,
            master: pair.master,
            writer,
            child,
            child_pid,
            parser,
            cwd: cwd.to_path_buf(),
            cwd_sampled: Instant::now(),
            process_icon: IDLE_ICON,
            process_sampled: Instant::now() - PROCESS_POLL_INTERVAL,
            process_pending: false,
            history,
        })
    }

    fn close_pane(&mut self, pane_id: usize) -> Result<()> {
        let Some((session_index, window_index, pane_index)) = self.locate_pane(pane_id) else {
            return Ok(());
        };
        // A background exit can remove rows before an open tree's selection.
        // Remember the selected object before changing the hierarchy, rather
        // than letting the same list index silently select its next neighbor.
        let tree_targets: Vec<_> = self
            .clients
            .iter()
            .filter_map(|(id, client)| {
                let tree = client.tree.as_ref()?;
                self.tree_items(&tree.expanded)
                    .get(tree.selected)
                    .cloned()
                    .map(|item| (*id, item))
            })
            .collect();
        let closed_session_id = self.sessions[session_index].id;
        for client in self.clients.values_mut() {
            client.vim.remove(&pane_id);
            if client.confirmation.as_ref().is_some_and(|confirmation| {
                matches!(confirmation, Confirmation::KillPane { pane_id: target, .. } if *target == pane_id)
            }) {
                client.confirmation = None;
            }
        }
        let window = &mut self.sessions[session_index].windows[window_index];
        window.panes.remove(pane_index);
        // The window this pane was zoomed out of has changed shape; show it.
        window.zoomed = false;
        let removed_window = window.panes.is_empty();
        if removed_window {
            self.sessions[session_index].remove_window(window_index);
        } else {
            window.layout = window
                .layout
                .clone()
                .without(pane_id)
                .context("pane layout lost its remaining pane")?;
            window.forget_pane(pane_id);
        }
        if self.sessions[session_index].windows.is_empty() {
            self.sessions.remove(session_index);
            let replacement = self.sessions.first().map(|session| session.id);
            let mut detached = Vec::new();
            for (client_id, client) in &mut self.clients {
                if let Some(tree) = &mut client.tree {
                    tree.expanded.remove(&closed_session_id);
                }
                if client.confirmation.as_ref().is_some_and(|confirmation| {
                    matches!(confirmation, Confirmation::KillSession { session_id: target, .. } if *target == closed_session_id)
                }) {
                    client.confirmation = None;
                }
                if client.session_id == Some(closed_session_id) {
                    client.session_id = replacement;
                    client.leader = false;
                    client.leader_key = None;
                    client.literal = false;
                    client.rename = None;
                    if replacement.is_none() {
                        client.tree = None;
                        client.writer.send(ServerMessage::Detached);
                        detached.push(*client_id);
                    }
                }
                client.previous_session_id = client
                    .previous_session_id
                    .filter(|previous| *previous != closed_session_id);
            }
            for client_id in detached {
                self.clients.remove(&client_id);
            }
            if replacement.is_some() {
                self.answer_bell(0);
            }
        } else if removed_window {
            self.answer_bell(session_index);
        }
        for (id, mut target) in tree_targets {
            if target.session_id == closed_session_id {
                if removed_window {
                    if let Some(window) = target.window {
                        target.window = shifted_index(window, window_index);
                        if target.window.is_none() {
                            target.pane = None;
                        }
                    }
                } else if target.window == Some(window_index) {
                    target.pane = target.pane.and_then(|pane| shifted_index(pane, pane_index));
                }
            }
            let Some(tree) = self
                .clients
                .get(&id)
                .and_then(|client| client.tree.as_ref())
            else {
                continue;
            };
            let items = self.tree_items(&tree.expanded);
            let selected = items
                .iter()
                .position(|item| {
                    item.session_id == target.session_id
                        && item.window == target.window
                        && item.pane == target.pane
                })
                .unwrap_or_else(|| tree.selected.min(items.len().saturating_sub(1)));
            let client = self.clients.get_mut(&id).unwrap();
            client.tree.as_mut().unwrap().selected = selected;
        }
        let client_ids: Vec<_> = self.clients.keys().copied().collect();
        for client_id in client_ids {
            self.resize_active(client_id)?;
        }
        if self.last_active_pane == Some(pane_id) {
            self.last_active_pane = self
                .session(closed_session_id)
                .or_else(|| self.sessions.first())
                .map(|session| session.windows[session.current_window].active_pane);
        }
        self.save_state_soon();
        if let Err(error) = self.persistence.remove_pane_history(pane_id) {
            self.note_failure(&format!("pane {pane_id} history"), error);
        }
        Ok(())
    }

    fn open_session_tree(&mut self, id: usize) {
        let session_id = self.clients[&id].session_id;
        let selected = self
            .sessions
            .iter()
            .position(|session| Some(session.id) == session_id)
            .unwrap_or(0);
        self.clients.get_mut(&id).unwrap().tree = Some(TreeState::folded(selected));
    }

    /// Opens the picker, or says why it cannot.
    ///
    /// The themes are read here rather than at attach time, so a theme added
    /// while the daemon has been running is offered without a restart.
    fn open_theme_picker(&mut self, id: usize) {
        let client = &self.clients[&id];
        let Some(directory) = client.theme_directory.clone() else {
            self.set_message(id, "no theme directory: set theme_directory".into());
            return;
        };
        let entries = match scan_themes(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                self.set_message(id, format!("themes: {error:#}"));
                return;
            }
        };
        if entries.is_empty() {
            self.set_message(id, format!("no themes in {}", compact_path(&directory)));
            return;
        }
        let in_use = current_theme_name(&directory)
            .and_then(|name| entries.iter().position(|entry| entry.name == name));
        let client = self.clients.get_mut(&id).unwrap();
        // The picker takes the whole screen, so it replaces the session tree
        // rather than stacking on top of it.
        client.tree = None;
        client.themes = Some(ThemePicker {
            entries,
            selected: in_use.unwrap_or(0),
            in_use,
        });
    }

    /// Hands the highlighted theme to the theme command and closes the picker.
    ///
    /// mux does not colour itself here: the command switches every program on
    /// the machine and sends mux a `set-theme` of its own, so the whole desktop
    /// changes together or not at all.
    fn apply_selected_theme(&mut self, id: usize) {
        let (name, already_in_use, command) = {
            let client = self.clients.get_mut(&id).unwrap();
            let Some(picker) = client.themes.take() else {
                return;
            };
            (
                picker.name().to_string(),
                picker.in_use == Some(picker.selected),
                client.theme_command.clone(),
            )
        };
        if already_in_use {
            self.set_message(id, format!("theme is already {name}"));
            return;
        }
        switch_theme(self.events.clone(), id, command, name);
    }

    fn ring_bell(&mut self, pane_id: usize, bell_events: usize) {
        let Some((session_index, window_index, _)) = self.locate_pane(pane_id) else {
            return;
        };
        let session_id = self.sessions[session_index].id;
        let visible = self.sessions[session_index].current_window == window_index
            && self
                .clients
                .values()
                .any(|client| client.initialized && client.session_id == Some(session_id));
        let bell = &mut self.sessions[session_index].windows[window_index].bell;
        let count = bell
            .as_ref()
            .map_or(bell_events, |bell| bell.count.saturating_add(bell_events));
        let appeared = bell
            .as_ref()
            .map_or_else(Instant::now, |bell| bell.appeared);
        *bell = Some(BellState {
            appeared,
            started: Instant::now(),
            render_token: 0,
            count,
            repeat: !visible,
            pane_id,
        });
    }

    fn advance_bell_animations(&mut self) -> bool {
        let mut changed = false;
        for window in self
            .sessions
            .iter_mut()
            .flat_map(|session| &mut session.windows)
        {
            let Some(bell) = &mut window.bell else {
                continue;
            };
            let elapsed = bell.started.elapsed().as_micros();
            if !bell.repeat && elapsed >= BELL_SHIMMER_MICROS {
                window.bell = None;
                changed = true;
                continue;
            }
            let render_token = bell_render_token(elapsed, bell.repeat);
            if bell.render_token != render_token {
                bell.render_token = render_token;
                changed = true;
            }
        }
        changed
    }

    fn jump_to_bell(&mut self, id: usize) -> Result<()> {
        let target = self
            .sessions
            .iter()
            .enumerate()
            .find_map(|(session_index, session)| {
                session
                    .windows
                    .iter()
                    .position(|window| window.bell.is_some())
                    .map(|window_index| (session_index, window_index))
            });
        let Some((session_index, window_index)) = target else {
            self.set_message(id, "no pending bells".into());
            return Ok(());
        };
        let session = &mut self.sessions[session_index];
        let pane_id = session.windows[window_index].bell.as_ref().unwrap().pane_id;
        visit_window(session, window_index);
        let window = &mut session.windows[window_index];
        if window.panes.iter().any(|pane| pane.id == pane_id) {
            window.select_pane(pane_id);
        }
        let session_id = session.id;
        self.answer_bell(session_index);
        self.set_client_session(id, session_id);
        self.clients.get_mut(&id).unwrap().tree = None;
        self.show_active(id)
    }

    fn new_window(&mut self, id: usize) -> Result<()> {
        let (session_index, _) = self.active_indices(id).context("no active session")?;
        let (cols, rows) = self.client_size(id);
        let root = self.sessions[session_index].root.clone();
        let future_window_count = self.sessions[session_index].windows.len() + 1;
        let window = self.create_window(&root, cols, rows, bar_width(future_window_count))?;
        let session = &mut self.sessions[session_index];
        session.windows.push(window);
        visit_window(session, session.windows.len() - 1);
        self.remember_active_pane(id);
        self.save_state_soon();
        Ok(())
    }

    /// Creates a session, named `name` or else the next free automatic name,
    /// and moves the client to it.
    fn new_session(&mut self, id: usize, name: Option<String>) -> Result<()> {
        if name.as_ref().is_some_and(|name| name.trim().is_empty()) {
            bail!("session name cannot be empty");
        }
        let root = self
            .active_cwd(id)
            .unwrap_or_else(|| self.clients[&id].cwd.clone());
        let name = name.unwrap_or_else(|| self.next_session_name());
        let (cols, rows) = self.client_size(id);
        let session_id = self.create_session(name, root, cols, rows)?;
        self.set_client_session(id, session_id);
        self.remember_active_pane(id);
        self.save_state_soon();
        Ok(())
    }

    fn set_client_session(&mut self, id: usize, session_id: usize) {
        let client = self.clients.get_mut(&id).unwrap();
        if client.session_id != Some(session_id) {
            client.previous_session_id = client.session_id;
            client.session_id = Some(session_id);
        }
    }

    fn switch_to_previous_session(&mut self, id: usize) -> Result<()> {
        let previous = self.clients[&id].previous_session_id;
        let Some(session_index) = previous.and_then(|session_id| self.session_index(session_id))
        else {
            self.clients.get_mut(&id).unwrap().previous_session_id = None;
            self.set_message(id, "no previous session".into());
            return Ok(());
        };
        self.set_client_session(id, self.sessions[session_index].id);
        self.answer_bell(session_index);
        self.clients.get_mut(&id).unwrap().tree = None;
        self.show_active(id)
    }

    /// The tree row a client has selected, clamped to the rows that exist.
    fn selected_tree_item(&self, tree: &TreeState) -> Option<TreeItem> {
        let items = self.tree_items(&tree.expanded);
        let index = tree.selected.min(items.len().saturating_sub(1));
        items.into_iter().nth(index)
    }

    /// Renames the session the tree is pointing at, or the current one.
    fn start_rename(&mut self, id: usize) {
        let session_id = match &self.clients[&id].tree {
            Some(tree) => self.selected_tree_item(tree).map(|item| item.session_id),
            None => self.clients[&id].session_id,
        };
        let Some(session) = session_id.and_then(|session_id| self.session(session_id)) else {
            return;
        };
        self.start_editing(
            id,
            RenameTarget::Session {
                session_id: session.id,
            },
            session.name.clone(),
        );
    }

    /// Renames the window the tree is pointing at, or the current one.
    fn start_rename_window(&mut self, id: usize) {
        let target = match &self.clients[&id].tree {
            Some(tree) => self
                .selected_tree_item(tree)
                .and_then(|item| Some((item.session_id, item.window?))),
            None => self
                .active_indices(id)
                .map(|(session_index, window_index)| {
                    (self.sessions[session_index].id, window_index)
                }),
        };
        let Some((session_id, window_index)) = target else {
            self.set_message(id, "select a window to rename".into());
            return;
        };
        let Some(name) = self
            .session(session_id)
            .and_then(|session| session.windows.get(window_index))
            .map(|window| window.name.clone().unwrap_or_default())
        else {
            return;
        };
        self.start_editing(
            id,
            RenameTarget::Window {
                session_id,
                window_index,
            },
            name,
        );
    }

    fn start_editing(&mut self, id: usize, target: RenameTarget, text: String) {
        let cursor = text.chars().count();
        self.clients.get_mut(&id).unwrap().rename = Some(RenameState {
            target,
            text,
            cursor,
        });
    }

    fn start_kill_pane(&mut self, id: usize) {
        let Some(pane_id) = self.active_pane(id).map(|pane| pane.id) else {
            return;
        };
        self.clients.get_mut(&id).unwrap().confirmation = Some(Confirmation::KillPane { pane_id });
    }

    fn start_kill_session(&mut self, id: usize, session_id: usize) {
        if self.session(session_id).is_none() {
            return;
        }
        self.clients.get_mut(&id).unwrap().confirmation =
            Some(Confirmation::KillSession { session_id });
    }

    fn kill_pane(&mut self, pane_id: usize) -> Result<()> {
        let Some(pane) = self.pane_mut(pane_id) else {
            return Ok(());
        };
        pane.child
            .kill()
            .with_context(|| format!("kill pane {pane_id}"))?;
        self.close_pane(pane_id)
    }

    fn kill_session(&mut self, session_id: usize) -> Result<()> {
        let Some(session) = self.session(session_id) else {
            return Ok(());
        };
        let pane_ids: Vec<_> = session
            .windows
            .iter()
            .flat_map(|window| &window.panes)
            .map(|pane| pane.id)
            .collect();
        for pane_id in &pane_ids {
            self.pane_mut(*pane_id)
                .unwrap()
                .child
                .kill()
                .with_context(|| format!("kill pane {pane_id}"))?;
        }
        for pane_id in pane_ids {
            self.close_pane(pane_id)?;
        }
        Ok(())
    }

    /// Why `name` cannot be given to the session at `session_index`, if it
    /// cannot.
    fn check_session_name(&self, session_index: usize, name: &str) -> Result<()> {
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
        Ok(())
    }

    fn finish_rename(&mut self, id: usize) -> Result<()> {
        let Some(rename) = self
            .clients
            .get_mut(&id)
            .and_then(|client| client.rename.take())
        else {
            return Ok(());
        };
        let name = rename.text;
        match rename.target {
            RenameTarget::Window {
                session_id,
                window_index,
            } => {
                let Some(window) = self
                    .session_index(session_id)
                    .and_then(|index| self.sessions[index].windows.get_mut(window_index))
                else {
                    return Ok(());
                };
                // An empty name hands the window back to whatever its program
                // calls itself, which is how a name is cleared.
                window.name = (!name.trim().is_empty()).then_some(name);
            }
            RenameTarget::Session { session_id } => {
                let Some(session_index) = self.session_index(session_id) else {
                    return Ok(());
                };
                if let Err(error) = self.check_session_name(session_index, &name) {
                    self.set_message(id, error.to_string());
                    return Ok(());
                }
                self.sessions[session_index].name = name;
            }
        }
        self.save_state_soon();
        Ok(())
    }

    fn split_active_pane(&mut self, id: usize, axis: SplitAxis) -> Result<()> {
        let (session_index, window_index, _) =
            self.active_pane_indices(id).context("no active pane")?;
        let area = self.content_area(id);
        let window = &self.sessions[session_index].windows[window_index];
        let active_pane = window.active_pane;
        let (regions, _) = window.regions(area);
        let active_rect = regions
            .iter()
            .find_map(|(pane_id, rect)| (*pane_id == active_pane).then_some(*rect))
            .context("active pane missing from layout")?;
        let enough_space = match axis {
            SplitAxis::Horizontal => active_rect.rows >= 3,
            SplitAxis::Vertical => active_rect.cols >= 3,
        };
        if !enough_space {
            self.set_message(id, "active pane is too small to split".into());
            return Ok(());
        }

        let cwd = self
            .active_cwd(id)
            .unwrap_or_else(|| self.sessions[session_index].root.clone());
        let (pane_cols, pane_rows) = match axis {
            SplitAxis::Horizontal => (active_rect.cols, (active_rect.rows - 1) / 2),
            SplitAxis::Vertical => ((active_rect.cols - 1) / 2, active_rect.rows),
        };
        let pane = self.create_pane(&cwd, pane_cols, pane_rows)?;
        let pane_id = pane.id;
        let window = &mut self.sessions[session_index].windows[window_index];
        if !window.layout.split(active_pane, pane_id, axis) {
            bail!("active pane missing from layout");
        }
        window.panes.push(pane);
        window.select_pane(pane_id);
        self.show_active(id)
    }

    fn focus_pane(&mut self, id: usize, direction: PaneDirection) -> Result<()> {
        let Some((session_index, window_index, _)) = self.active_pane_indices(id) else {
            return Ok(());
        };
        let area = self.content_area(id);
        let window = &self.sessions[session_index].windows[window_index];
        let (regions, _) = window.regions(area);
        if let Some(pane_id) = neighboring_pane(
            &regions,
            window.active_pane,
            Some(window.previous_pane),
            direction,
        ) {
            self.sessions[session_index].windows[window_index].select_pane(pane_id);
            self.remember_active_pane(id);
            self.save_state_soon();
        }
        Ok(())
    }

    /// Exchanges the current window with the one at `target`, counting from one.
    ///
    /// Swapping rather than shifting keeps every other window where it is, so
    /// the number you reach for a window only changes for the two involved.
    fn swap_window(&mut self, id: usize, target: usize) -> Result<()> {
        let (session_index, window_index) = self.active_indices(id).context("no active session")?;
        let session = &mut self.sessions[session_index];
        let target = target.checked_sub(1).context("windows count from 1")?;
        if target >= session.windows.len() {
            bail!("window {} does not exist", target + 1);
        }
        if target == window_index {
            return Ok(());
        }
        session.windows.swap(window_index, target);
        // Follow the window that moved, so swapping keeps you where you are.
        if session.current_window == window_index {
            session.current_window = target;
        } else if session.current_window == target {
            session.current_window = window_index;
        }
        self.save_state_soon();
        Ok(())
    }

    /// Moves the current window one place along, wrapping at the ends so a
    /// window can be walked all the way round.
    fn swap_window_by(&mut self, id: usize, offset: isize) -> Result<()> {
        let Some((session_index, window_index)) = self.active_indices(id) else {
            return Ok(());
        };
        let count = self.sessions[session_index].windows.len() as isize;
        if count < 2 {
            return Ok(());
        }
        let target = (window_index as isize + offset).rem_euclid(count) as usize;
        self.swap_window(id, target + 1)
    }

    /// Takes the active pane out of its window and gives it one of its own.
    fn break_pane(&mut self, id: usize) -> Result<()> {
        let (session_index, window_index, _) =
            self.active_pane_indices(id).context("no active pane")?;
        let session = &self.sessions[session_index];
        if session.windows[window_index].panes.len() < 2 {
            self.set_message(id, "the window has only this pane".into());
            return Ok(());
        }
        let pane_id = session.windows[window_index].active_pane;
        let pane = self.detach_pane(pane_id).context("active pane vanished")?;
        let session = &mut self.sessions[session_index];
        session.windows.push(Window::new(pane));
        visit_window(session, session.windows.len() - 1);
        self.show_active(id)
    }

    /// Moves the active pane into `target`, splitting the pane it finds there.
    fn join_pane(&mut self, id: usize, target: usize, axis: SplitAxis) -> Result<()> {
        let (session_index, window_index, _) =
            self.active_pane_indices(id).context("no active pane")?;
        let target = target.checked_sub(1).context("windows count from 1")?;
        let session = &self.sessions[session_index];
        if target >= session.windows.len() {
            bail!("window {} does not exist", target + 1);
        }
        if target == window_index && session.windows[window_index].panes.len() < 2 {
            self.set_message(id, "the pane is already alone in that window".into());
            return Ok(());
        }
        if session.windows[window_index].panes.len() < 2 && session.windows.len() < 2 {
            self.set_message(id, "nothing would be left behind".into());
            return Ok(());
        }
        let pane_id = session.windows[window_index].active_pane;
        // Taking the last pane out of a window closes it, which shifts every
        // window after it down one place — including, possibly, the target.
        let source_closes = session.windows[window_index].panes.len() == 1;
        let target = if source_closes && target > window_index {
            target - 1
        } else {
            target
        };
        let pane = self.detach_pane(pane_id).context("active pane vanished")?;
        let session = &mut self.sessions[session_index];
        let window = session
            .windows
            .get_mut(target)
            .context("the target window is gone")?;
        let anchor = window.active_pane;
        if !window.layout.split(anchor, pane_id, axis) {
            bail!("target pane missing from layout");
        }
        window.panes.push(pane);
        window.zoomed = false;
        window.select_pane(pane_id);
        visit_window(session, target);
        self.show_active(id)
    }

    /// Removes a pane from its window without touching the process inside it,
    /// closing the window if that was the last pane in it.
    fn detach_pane(&mut self, pane_id: usize) -> Option<Pane> {
        let (session_index, window_index, pane_index) = self.locate_pane(pane_id)?;
        let session = &mut self.sessions[session_index];
        let window = &mut session.windows[window_index];
        let pane = window.panes.remove(pane_index);
        window.zoomed = false;
        if window.panes.is_empty() {
            session.remove_window(window_index);
        } else {
            window.layout = window.layout.clone().without(pane_id)?;
            window.forget_pane(pane_id);
        }
        Some(pane)
    }

    /// Enters focus mode for the active pane and hands its keys through, or
    /// restores the normal mux interface.
    fn zoom_pane(&mut self, id: usize) -> Result<()> {
        let Some((session_index, window_index, _)) = self.active_pane_indices(id) else {
            return Ok(());
        };
        let window = &mut self.sessions[session_index].windows[window_index];
        window.zoomed = !window.zoomed;
        self.resize_active(id)?;
        self.rebuild_vim(id);
        self.save_state_soon();
        Ok(())
    }

    /// Moves the divider next to the active pane.
    ///
    /// `direction` is where the border goes, not what happens to the pane: a
    /// pane on the right of a split grows when its left border moves left.
    fn resize_pane(&mut self, id: usize, direction: PaneDirection, cells: u16) -> Result<()> {
        let Some((session_index, window_index, _)) = self.active_pane_indices(id) else {
            return Ok(());
        };
        let area = self.content_area(id);
        let (axis, cells) = match direction {
            PaneDirection::Left => (SplitAxis::Vertical, -i32::from(cells)),
            PaneDirection::Right => (SplitAxis::Vertical, i32::from(cells)),
            PaneDirection::Up => (SplitAxis::Horizontal, -i32::from(cells)),
            PaneDirection::Down => (SplitAxis::Horizontal, i32::from(cells)),
        };
        let window = &mut self.sessions[session_index].windows[window_index];
        let active_pane = window.active_pane;
        if !window.layout.resize(area, active_pane, axis, cells) {
            return Ok(());
        }
        self.resize_active(id)?;
        self.save_state_soon();
        Ok(())
    }

    fn set_session_root(&mut self, id: usize) -> Result<()> {
        let Some((session_index, _)) = self.active_indices(id) else {
            return Ok(());
        };
        match self.active_cwd(id) {
            Some(path) => {
                self.sessions[session_index].root = path.clone();
                self.set_message(id, format!("session root: {}", path.display()));
                self.save_state_soon();
            }
            None => self.set_message(id, "could not determine shell working directory".into()),
        }
        Ok(())
    }

    fn select_window(&mut self, id: usize, number: usize) -> Result<()> {
        let Some((session_index, _)) = self.active_indices(id) else {
            return Ok(());
        };
        if number > 0 && number <= self.sessions[session_index].windows.len() {
            visit_window(&mut self.sessions[session_index], number - 1);
            self.answer_bell(session_index);
            self.show_active(id)?;
        }
        Ok(())
    }

    /// Forgets what the client's terminal shows, so the next frame repaints
    /// all of it: the way back from anything else having drawn over mux.
    fn refresh_client(&mut self, id: usize) {
        if let Some(client) = self.clients.get_mut(&id) {
            client.frame = Frame::default();
        }
        self.dirty = true;
    }

    fn enter_vim(&mut self, id: usize) {
        let Some((session_index, window_index, pane_index)) = self.active_pane_indices(id) else {
            return;
        };
        let pane = &mut self.sessions[session_index].windows[window_index].panes[pane_index];
        let client = self.clients.get_mut(&id).unwrap();
        if client.vim.contains_key(&pane.id) {
            return;
        }
        let rows = client.rows.max(1) as usize;
        let (buffer, cursor) = snapshot_screen(pane.parser.screen_mut());
        client.vim.insert(
            pane.id,
            VimState {
                mode: VimMode::new(buffer, cursor, rows),
            },
        );
    }

    /// Enters vim mode already asking where to jump to, so one key does what
    /// entering the mode and pressing the jump key does.
    fn enter_vim_jump(&mut self, id: usize) {
        self.enter_vim(id);
        let Some(pane_id) = self.active_vim_pane_id(id) else {
            return;
        };
        if let Some(vim) = self.clients.get_mut(&id).unwrap().vim.get_mut(&pane_id) {
            vim.mode.start_jump();
        }
    }

    fn rebuild_vim(&mut self, id: usize) {
        let Some(pane_id) = self.active_vim_pane_id(id) else {
            return;
        };
        self.clients.get_mut(&id).unwrap().vim.remove(&pane_id);
        self.enter_vim(id);
    }
}

/// A pane's parser, filled in from its journal before the pane exists.
struct ReplayedPane {
    parser: vt100::Parser<TerminalCallbacks>,
    /// How much of the journal replayed cleanly, which is where the pane's
    /// own writes carry on from.
    valid_length: u64,
    history_length: u64,
    replayed: bool,
}

/// Runs the theme command off the event loop.
///
/// The command asks the daemon to recolour itself as part of its work, and that
/// request travels back over the socket the daemon is serving, so it must not be
/// waited for from inside the loop that would answer it.
fn switch_theme(sender: mpsc::SyncSender<Event>, id: usize, command: Vec<String>, name: String) {
    thread::spawn(move || {
        let result = run_theme_command(&command, &name).map_err(|error| format!("{error:#}"));
        let _ = sender.send(Event::ThemeSwitched(id, name, result));
    });
}

fn run_theme_command(command: &[String], name: &str) -> Result<()> {
    let status = Command::new(&command[0])
        .args(&command[1..])
        .arg(name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .with_context(|| format!("start {}", command[0]))?;
    if !status.success() {
        bail!("{} exited with {status}", command[0])
    }
    Ok(())
}

fn copy_to_clipboard(
    sender: mpsc::SyncSender<Event>,
    id: usize,
    command: Vec<String>,
    text: String,
) {
    thread::spawn(move || {
        let bytes = text.len();
        let result = run_clipboard_command(&command, &text).map_err(|error| format!("{error:#}"));
        let _ = sender.send(Event::ClipboardCopied(id, bytes, result));
    });
}

fn run_clipboard_command(command: &[String], text: &str) -> Result<()> {
    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .with_context(|| format!("start {}", command[0]))?;
    child.stdin.take().unwrap().write_all(text.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        bail!("{} exited with {status}", command[0])
    }
    Ok(())
}

impl Server {
    fn detach(&mut self, id: usize) -> Result<()> {
        self.track_active_pane(id);
        if let Some(client) = self.clients.remove(&id) {
            client.writer.send(ServerMessage::Detached);
        }
        Ok(())
    }

    fn choose_tree_item(&mut self, id: usize, item: TreeItem) -> Result<()> {
        let Some(session_index) = self.session_index(item.session_id) else {
            return Ok(());
        };
        let session = &mut self.sessions[session_index];
        if let Some(window) = item.window {
            visit_window(session, window);
            let window = session.current_window_mut();
            if let Some(pane_id) = item
                .pane
                .and_then(|pane| window.panes.get(pane))
                .map(|pane| pane.id)
            {
                window.select_pane(pane_id);
            }
        }
        self.answer_bell(session_index);
        self.set_client_session(id, item.session_id);
        self.clients.get_mut(&id).unwrap().tree = None;
        self.show_active(id)
    }

    fn next_session_name(&self) -> String {
        (1..)
            .map(automatic_session_name)
            .find(|name| self.sessions.iter().all(|session| &session.name != name))
            .unwrap()
    }

    fn last_active_session_id(&self) -> Option<usize> {
        let (session_index, _, _) = self.locate_pane(self.last_active_pane?)?;
        Some(self.sessions[session_index].id)
    }

    /// Records the client's active pane as the one to come back to, reporting
    /// whether that changed.
    fn remember_active_pane(&mut self, client_id: usize) -> bool {
        let Some(pane_id) = self.active_pane(client_id).map(|pane| pane.id) else {
            return false;
        };
        let changed = self.last_active_pane != Some(pane_id);
        self.last_active_pane = Some(pane_id);
        changed
    }

    fn track_active_pane(&mut self, client_id: usize) {
        if self.remember_active_pane(client_id) {
            self.save_state_soon();
        }
    }

    /// Settles a client on what it now shows: remembers its active pane,
    /// fits the window to it, and saves.
    fn show_active(&mut self, client_id: usize) -> Result<()> {
        self.remember_active_pane(client_id);
        self.resize_active(client_id)?;
        self.save_state_soon();
        Ok(())
    }

    /// Answers a pending bell on the session's current window, which has just
    /// come into view.
    fn answer_bell(&mut self, session_index: usize) {
        let shimmer = self.bells_shimmer();
        play_bell_once(
            &mut self.sessions[session_index].current_window_mut().bell,
            shimmer,
        );
    }

    fn active_cwd(&self, id: usize) -> Option<PathBuf> {
        let pane = self.active_pane(id)?;
        let pid = pane.child_pid?;
        process_cwd(pid)
    }

    fn client_size(&self, id: usize) -> (u16, u16) {
        self.clients
            .get(&id)
            .map(|client| (client.cols, client.rows))
            .unwrap_or((80, 24))
    }

    fn active_indices(&self, client_id: usize) -> Option<(usize, usize)> {
        let session_index = self.session_index(self.clients.get(&client_id)?.session_id?)?;
        Some((session_index, self.sessions[session_index].current_window))
    }

    fn other_session_bells(&self, client_id: usize) -> Option<(usize, &BellState)> {
        let active_session = self.clients.get(&client_id)?.session_id?;
        let mut count = 0usize;
        let mut newest: Option<&BellState> = None;
        for bell in self
            .sessions
            .iter()
            .filter(|session| session.id != active_session)
            .flat_map(|session| &session.windows)
            .filter_map(|window| window.bell.as_ref())
        {
            count = count.saturating_add(bell.count);
            if newest.is_none_or(|current| bell.started > current.started) {
                newest = Some(bell);
            }
        }
        newest.map(|bell| (count, bell))
    }

    fn active_pane_indices(&self, client_id: usize) -> Option<(usize, usize, usize)> {
        let (session, window) = self.active_indices(client_id)?;
        let pane_id = self.sessions[session].windows[window].active_pane;
        let pane = self.sessions[session].windows[window]
            .panes
            .iter()
            .position(|pane| pane.id == pane_id)?;
        Some((session, window, pane))
    }

    fn active_pane(&self, client_id: usize) -> Option<&Pane> {
        let (session, window, pane) = self.active_pane_indices(client_id)?;
        self.sessions[session].windows[window].panes.get(pane)
    }

    fn active_vim_pane_id(&self, client_id: usize) -> Option<usize> {
        let pane_id = self.active_pane(client_id)?.id;
        self.clients[&client_id]
            .vim
            .contains_key(&pane_id)
            .then_some(pane_id)
    }

    fn vim_active(&self, client_id: usize) -> bool {
        self.active_vim_pane_id(client_id).is_some()
    }

    fn active_bar_width(&self, client_id: usize) -> u16 {
        self.active_indices(client_id)
            .map(|(session, window)| {
                if self.sessions[session].windows[window].zoomed {
                    0
                } else {
                    bar_width(self.sessions[session].windows.len())
                }
            })
            .unwrap_or_else(|| bar_width(0))
    }

    /// The area a client's window has to lay its panes out in: its whole screen
    /// less the bar down the left, and never smaller than one cell.
    fn content_area(&self, client_id: usize) -> Rect {
        let (cols, rows) = self.client_size(client_id);
        Rect {
            row: 0,
            col: 0,
            rows: rows.max(1),
            cols: cols.saturating_sub(self.active_bar_width(client_id)).max(1),
        }
    }

    fn pane_mut(&mut self, pane_id: usize) -> Option<&mut Pane> {
        self.panes_mut().find(|pane| pane.id == pane_id)
    }

    fn sample_process_icons(&mut self) {
        if !self.clients.values().any(|client| client.initialized) {
            return;
        }
        let now = Instant::now();
        let samples: Vec<_> = self
            .panes_mut()
            .filter(|pane| {
                !pane.process_pending
                    && now.duration_since(pane.process_sampled) >= PROCESS_POLL_INTERVAL
            })
            .map(|pane| {
                pane.process_sampled = now;
                pane.process_pending = true;
                ProcessSample {
                    pane_id: pane.id,
                    group: pane.master.process_group_leader(),
                }
            })
            .collect();
        if !samples.is_empty() {
            let _ = self.process_sampler.send(samples);
        }
    }

    fn resize_active(&mut self, id: usize) -> Result<()> {
        let Some((session_index, window_index)) = self.active_indices(id) else {
            return Ok(());
        };
        let area = self.content_area(id);
        let mut failure = None;
        let window = &mut self.sessions[session_index].windows[window_index];
        let (regions, _) = window.regions(area);
        for pane in &mut window.panes {
            // A pane hidden behind a zoomed one keeps the size it had, ready
            // for the layout it goes back to.
            let Some(rect) = regions
                .iter()
                .find_map(|(pane_id, rect)| (*pane_id == pane.id).then_some(*rect))
            else {
                continue;
            };
            let size = PtySize {
                rows: rect.rows.max(1),
                cols: rect.cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            };
            if pane.parser.screen().size() != (size.rows, size.cols) {
                // The parser follows the new size either way: a pane whose PTY
                // or journal refused the change still has to be drawn.
                let _ = pane.master.resize(size);
                if let Err(error) = pane.history.append_resize(size.rows, size.cols) {
                    failure = Some((pane.id, error));
                }
                pane.parser.screen_mut().set_size(size.rows, size.cols);
            }
        }
        if let Some((pane_id, error)) = failure {
            self.note_failure(&format!("pane {pane_id} history"), error);
        }
        Ok(())
    }

    fn tree_items(&self, expanded: &HashSet<usize>) -> Vec<TreeItem> {
        let mut items = Vec::new();
        for session in &self.sessions {
            items.push(TreeItem {
                session_id: session.id,
                window: None,
                pane: None,
                label: session.name.clone(),
            });
            if expanded.contains(&session.id) {
                for (window_index, window) in session.windows.iter().enumerate() {
                    items.push(TreeItem {
                        session_id: session.id,
                        window: Some(window_index),
                        pane: None,
                        label: match window.label() {
                            Some(label) => format!("window {} · {label}", window_index + 1),
                            None => format!(
                                "window {} · {} pane{}",
                                window_index + 1,
                                window.panes.len(),
                                if window.panes.len() == 1 { "" } else { "s" }
                            ),
                        },
                    });
                    for pane in 0..window.panes.len() {
                        items.push(TreeItem {
                            session_id: session.id,
                            window: Some(window_index),
                            pane: Some(pane),
                            label: format!("pane {}", pane + 1),
                        });
                    }
                }
            }
        }
        items
    }
}

/// Returns pages from large, short-lived history replays to the operating
/// system instead of leaving them cached in macOS's allocator indefinitely.
#[cfg(target_os = "macos")]
fn release_unused_memory() {
    use std::ffi::c_void;

    unsafe extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut c_void, goal: usize) -> usize;
    }
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn release_unused_memory() {}

/// The text a panic was raised with.
pub(super) fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|message| (*message).to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

fn visit_window(session: &mut Session, window: usize) {
    session.current_window = window.min(session.windows.len() - 1);
}

fn window_index_after_removal(index: usize, removed: usize, remaining: usize) -> usize {
    if removed < index {
        index - 1
    } else {
        index.min(remaining - 1)
    }
}

/// Where `index` ends up once the item at `removed` is taken out of its list,
/// or `None` if it was that item.
fn shifted_index(index: usize, removed: usize) -> Option<usize> {
    (index != removed).then(|| index - usize::from(index > removed))
}

fn automatic_session_name(number: usize) -> String {
    format!("Session {number}")
}

#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod responsiveness_tests;
#[cfg(test)]
mod session_regression_tests;
#[cfg(test)]
mod terminal_query_tests;
#[cfg(test)]
mod tests;
