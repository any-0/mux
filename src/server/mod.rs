//! The daemon: it owns every pane, and paints one screen per attached client.
//!
//! This module holds the state itself — sessions, windows, panes and clients —
//! along with the event loop that drives them, and the operations that change
//! the session tree. The submodules beside it each own one job:
//!
//! What drives the daemon:
//!
//! - [`input`]: what a keystroke, a click or a paste does
//! - [`command`]: the `mux ...` subcommands and queries
//! - [`render`]: painting one client's screen
//!
//! What it is made of:
//!
//! - [`layout`]: where a window's panes sit, and the borders between them
//! - [`terminal`]: the PTY and `vt100` glue for one pane
//! - [`snapshot`]: the still copy of a pane that Vim mode moves around in
//! - [`bell`]: a pending bell and its animation
//! - [`ui`]: the bar, the tree, popups and previews
//! - [`themes`]: the theme picker
//! - [`process`]: what a pane is running, and its icon
//!
//! What it keeps:
//!
//! - [`journal`]: a pane's replayable history on disk
//! - [`persist`]: the session tree on disk, and the daemon's private files

mod bell;
mod command;
mod input;
mod journal;
mod layout;
mod persist;
mod process;
mod render;
pub(crate) mod snapshot;
mod terminal;
mod themes;
mod ui;

use bell::*;
use journal::*;
use layout::*;
use persist::*;
use process::*;
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
    frame::{ColorDepth, Frame},
    protocol::{ClientMessage, Hello, ServerMessage, read_message, write_message},
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
/// How long one write to a client may take before it is considered gone.
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Cells one resize keystroke moves a divider.
const RESIZE_STEP: u16 = 2;
/// Lines one turn of the wheel scrolls.
const MOUSE_SCROLL_LINES: usize = 3;

impl Event {
    /// Whether this came from a client rather than from a pane.
    ///
    /// A pane restored from a long journal has its shell start up the moment it
    /// is spawned, and two dozen shells can put thousands of output events in
    /// front of the attach that is waiting to draw the screen. Client events go
    /// first within a batch so the daemon answers the person before the panes.
    fn from_client(&self) -> bool {
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
    PtyOutput(usize, Vec<u8>),
    PtyClosed(usize),
    ProcessIcon(usize, &'static str),
    ClipboardCopied(usize, usize, Result<(), String>),
    ThemeSwitched(usize, String, Result<(), String>),
}

struct Pane {
    id: usize,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    child_pid: Option<u32>,
    parser: vt100::Parser<TerminalCallbacks>,
    parser_prefix: Vec<u8>,
    query_prefix: Vec<u8>,
    cwd: PathBuf,
    cwd_sampled: Instant,
    process_icon: &'static str,
    process_sampled: Instant,
    history: PaneJournal,
}

struct ProcessSample {
    pane_id: usize,
    group: Option<i32>,
    child_pid: Option<u32>,
}

struct Window {
    panes: Vec<Pane>,
    layout: PaneLayout,
    active_pane: usize,
    previous_pane: usize,
    bell: Option<BellState>,
    /// While set, the active pane is drawn over the whole window and the others
    /// are left as they were. The layout underneath is untouched, so unzooming
    /// puts everything back exactly where it was.
    zoomed: bool,
    /// A name given with `rename-window`. Without one the window goes by
    /// whatever its active pane last set as the terminal title.
    name: Option<String>,
}

impl Window {
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

    /// Where this window's panes sit inside `area`.
    ///
    /// Zoomed, only the active pane has a place; the rest keep whatever size
    /// they last had and are not drawn.
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

/// The daemon's end of one client connection.
///
/// Messages go through a writer thread, so a client that has stopped reading —
/// a suspended terminal, a dropped ssh link — fills its own queue instead of
/// blocking the event loop and stalling every pane the daemon owns.
struct ClientWriter {
    messages: mpsc::SyncSender<ServerMessage>,
    thread: thread::JoinHandle<()>,
}

impl ClientWriter {
    fn spawn(mut stream: UnixStream) -> Self {
        let (messages, receiver) = mpsc::sync_channel(CLIENT_QUEUE_DEPTH);
        // Without a timeout, a client that never reads again would hold its
        // writer thread — and the daemon's exit — open forever.
        let _ = stream.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT));
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
        Self { messages, thread }
    }

    /// Waits for everything queued to reach the client, then closes it.
    ///
    /// Dropping the sender is enough on its own — the thread drains what is
    /// left before it sees the disconnect — but the daemon exits right after
    /// shutdown, so it has to wait for that to happen.
    fn finish(self) {
        drop(self.messages);
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
    theme: Theme,
    /// What the theme picker runs to switch theme, and where it finds the
    /// themes to offer.
    theme_command: Vec<String>,
    theme_directory: Option<PathBuf>,
    /// Whether this client's terminal hands mux the mouse.
    mouse: bool,
    /// How this client shows a pending bell.
    bell_style: BellStyle,
    /// The colour depth this client's terminal is painted for.
    colors: ColorDepth,
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
    process_sampler: Sender<ProcessSample>,
    events: Sender<Event>,
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
    /// Set when the session tree has changed and the copy on disk is stale.
    /// Writing it costs two fsyncs, so it is deferred until the daemon settles
    /// instead of running on every keystroke that moves the active pane.
    state_dirty: bool,
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

    let (sender, receiver) = mpsc::channel();
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
        dirty: false,
        state_dirty: false,
    };
    if let Some(state) = persisted_state {
        server.restore(state)?;
    }
    server.compact_journals()?;
    for pane in server
        .sessions
        .iter()
        .flat_map(|session| &session.windows)
        .flat_map(|window| &window.panes)
    {
        pane.parser.screen().flush_history_backing();
    }
    release_unused_memory();
    let result = server.event_loop(receiver);
    let _ = fs::remove_file(socket_path);
    result
}

fn accept_clients(listener: UnixListener, sender: Sender<Event>) {
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
                    match read_message(&mut reader) {
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

fn process_icon_sampler(events: Sender<Event>) -> Sender<ProcessSample> {
    let (sender, receiver) = mpsc::channel::<ProcessSample>();
    thread::spawn(move || {
        while let Ok(sample) = receiver.recv() {
            let commands = sample
                .group
                .map(process_group_info)
                .filter(|commands| !commands.is_empty())
                .or_else(|| {
                    sample
                        .child_pid
                        .and_then(|pid| process_info(pid as i32))
                        .map(|command| vec![command])
                })
                .unwrap_or_default();
            if events
                .send(Event::ProcessIcon(
                    sample.pane_id,
                    process_group_icon(&commands),
                ))
                .is_err()
            {
                return;
            }
        }
    });
    sender
}

impl Server {
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
                    let replayed = replayed
                        .remove(&saved_pane.id)
                        .context("a saved pane was not replayed")?;
                    let ReplayedPane {
                        parser,
                        parser_prefix,
                        valid_length,
                        history_length,
                        replayed: had_history,
                    } = replayed;
                    let history = match self
                        .persistence
                        .resume_pane_history(saved_pane.id, valid_length)
                    {
                        Ok(file) => PaneJournal::new(file, valid_length),
                        Err(_) => {
                            PaneJournal::new(self.persistence.new_pane_history(saved_pane.id)?, 0)
                        }
                    };
                    let mut pane = self.spawn_pane_with(
                        saved_pane.id,
                        &saved_pane.cwd,
                        saved_pane.cols,
                        saved_pane.rows,
                        history,
                        parser,
                        parser_prefix,
                        false,
                    )?;
                    pane.history.compact_soon();
                    if had_history && valid_length != history_length {
                        let _ = pane.history.truncate(valid_length);
                    }
                    if let Some(correction) = restored_prompt_correction(&pane.parser) {
                        let _ = pane.history.append_output(&correction);
                        process_terminal_bytes(
                            &mut pane.parser,
                            &mut pane.parser_prefix,
                            &correction,
                        );
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
        loop {
            let event = match self.next_wake(last_render) {
                Some(deadline) => {
                    let timeout = deadline.saturating_duration_since(Instant::now());
                    match receiver.recv_timeout(timeout) {
                        Ok(event) => Some(event),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                    }
                }
                // Nothing is pending: sleep until a client or a pane speaks up.
                None => {
                    self.settle();
                    match receiver.recv() {
                        Ok(event) => Some(event),
                        Err(_) => return Ok(()),
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
                batch.sort_by_key(|event| !event.from_client());
                for event in batch {
                    if self.dispatch(event) {
                        return Ok(());
                    }
                }
            }
            self.dirty |= self.expire_messages();
            self.dirty |= self.advance_bell_animations();
            if self.dirty && last_render.elapsed() >= FRAME_INTERVAL {
                self.render_all();
                self.dirty = false;
                last_render = Instant::now();
            }
        }
    }

    /// When the next repaint is due, or `None` when the daemon can sleep until
    /// something happens.
    fn next_wake(&self, last_render: Instant) -> Option<Instant> {
        if self.dirty {
            return Some(last_render + FRAME_INTERVAL);
        }
        if self.bells_animating() {
            return Some(last_render + ANIMATION_INTERVAL);
        }
        // A message expiring is a change no further event would announce.
        self.clients
            .values()
            .filter_map(|client| client.message.as_ref().map(|message| message.expires))
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
        for pane in self
            .sessions
            .iter_mut()
            .flat_map(|session| &mut session.windows)
            .flat_map(|window| &mut window.panes)
        {
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
        let overgrown = self.overgrown_journals();
        for pane_id in overgrown {
            self.compact_journal(pane_id)?;
        }
        Ok(())
    }

    fn overgrown_journals(&self) -> Vec<usize> {
        self.sessions
            .iter()
            .flat_map(|session| &session.windows)
            .flat_map(|window| &window.panes)
            .filter(|pane| pane.history.needs_compaction())
            .map(|pane| pane.id)
            .collect()
    }

    fn compact_journal(&mut self, pane_id: usize) -> Result<()> {
        let file = self.persistence.new_pane_history(pane_id)?;
        let pane = self
            .pane_mut(pane_id)
            .context("pane vanished during compaction")?;
        let records = compacted_journal_records(pane.parser.screen_mut())?;
        pane.history.replace(file, &records)
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

    /// Reports something that went wrong without taking the daemon with it.
    ///
    /// Losing a pane's history or a state write is worth telling the user
    /// about, but it is never worth killing every shell the daemon owns.
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
    /// Nothing an event can do is worth losing every running shell over, so a
    /// failure becomes a message on screen and the daemon carries on.
    fn dispatch(&mut self, event: Event) -> bool {
        match self.handle_event(event) {
            Ok(stop) => stop,
            Err(error) => {
                self.note_failure("mux", error);
                false
            }
        }
    }

    fn handle_event(&mut self, event: Event) -> Result<bool> {
        match event {
            Event::Connected(id, writer) => {
                self.clients.insert(
                    id,
                    Client {
                        writer: ClientWriter::spawn(writer),
                        cols: 80,
                        rows: 24,
                        cwd: PathBuf::new(),
                        session_id: None,
                        previous_session_id: None,
                        bindings: Bindings::defaults(),
                        clipboard_command: vec!["yank".into()],
                        theme_command: vec!["theme".into()],
                        theme_directory: None,
                        theme: Theme::default(),
                        mouse: false,
                        bell_style: BellStyle::default(),
                        colors: ColorDepth::TrueColor,
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
                    },
                );
            }
            Event::Disconnected(id) => {
                if self.remember_active_pane(id)? {
                    self.save_state_soon();
                }
                self.clients.remove(&id);
            }
            Event::PtyOutput(pane_id, bytes) => {
                let mut cwd_changed = false;
                let mut bell_events = 0;
                let mut failure = None;
                let mut sample_process = false;
                let colors = TerminalColors::from(&self.theme);
                if let Some(pane) = self.pane_mut(pane_id) {
                    failure = pane.history.append_output(&bytes).err();
                    let previous_bells = pane.parser.callbacks().bell_count;
                    let had_prompt = pane.parser.callbacks().prompt_ready.is_some();
                    process_terminal_bytes(&mut pane.parser, &mut pane.parser_prefix, &bytes);
                    bell_events = pane
                        .parser
                        .callbacks()
                        .bell_count
                        .saturating_sub(previous_bells) as usize;
                    let responses = terminal_query_responses(
                        &mut pane.query_prefix,
                        &bytes,
                        pane.parser.screen().cursor_position(),
                        colors,
                    );
                    if !responses.is_empty() {
                        // A pane whose shell has just died cannot take a reply.
                        let _ = pane
                            .writer
                            .write_all(&responses)
                            .and_then(|()| pane.writer.flush());
                    }
                    // Sampling the shell's directory costs a system call, so do
                    // it when a prompt appears and otherwise only occasionally.
                    let reached_prompt =
                        !had_prompt && pane.parser.callbacks().prompt_ready.is_some();
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
                    // A command can start and finish before the periodic poll
                    // interval elapses.  The prompt marker is the reliable
                    // transition back to the shell, so refresh immediately or
                    // a short-lived command's icon can remain stuck forever.
                    sample_process =
                        reached_prompt || pane.process_sampled.elapsed() >= PROCESS_POLL_INTERVAL;
                    self.dirty = true;
                }
                if let Some(error) = failure {
                    self.note_failure(&format!("pane {pane_id} history"), error);
                }
                if bell_events > 0 {
                    self.ring_bell(pane_id, bell_events);
                }
                if cwd_changed {
                    self.save_state_soon();
                }
                if sample_process {
                    self.sample_process_icon(pane_id);
                }
            }
            Event::PtyClosed(pane_id) => {
                self.close_pane(pane_id)?;
                self.dirty = true;
            }
            Event::ProcessIcon(pane_id, icon) => {
                if let Some(pane) = self.pane_mut(pane_id) {
                    pane.process_icon = icon;
                    self.dirty = true;
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
                for pane in self
                    .sessions
                    .iter_mut()
                    .flat_map(|session| &mut session.windows)
                    .flat_map(|window| &mut window.panes)
                {
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
            Event::Client(id, ClientMessage::Query { pane_id, query }) => {
                let lines = self.listing(pane_id, query);
                if let Some(client) = self.clients.get(&id) {
                    client.writer.send(ServerMessage::Listing(lines));
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
                self.initialize_client(id, *hello)?;
            }
            Event::Client(id, ClientMessage::Resize { cols, rows }) => {
                if let Some(client) = self.clients.get_mut(&id) {
                    client.cols = cols;
                    client.rows = rows;
                }
                self.resize_active(id)?;
                self.rebuild_vim(id);
                self.save_state_soon();
                self.dirty = true;
            }
            Event::Client(id, ClientMessage::Key(key)) => {
                if self.remember_active_pane(id)? {
                    self.save_state_soon();
                }
                self.handle_key(id, key)?;
            }
            Event::Client(id, ClientMessage::Mouse(mouse)) => {
                self.handle_mouse(id, mouse)?;
                self.dirty = true;
            }
            Event::Client(id, ClientMessage::Paste(text)) => {
                if self.remember_active_pane(id)? {
                    self.save_state_soon();
                }
                self.handle_paste(id, text)?;
            }
            Event::Client(id, ClientMessage::Detach) => self.detach(id)?,
        }
        Ok(false)
    }

    fn initialize_client(&mut self, id: usize, hello: Hello) -> Result<()> {
        let Hello {
            cols,
            rows,
            cwd,
            session,
            bindings,
            clipboard_command,
            theme,
            theme_command,
            theme_directory,
            mouse,
            bell_style,
            truecolor,
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
        {
            let client = self
                .clients
                .get_mut(&id)
                .context("client disconnected during setup")?;
            client.cols = cols;
            client.rows = rows;
            client.cwd = cwd;
            client.bindings = bindings;
            client.clipboard_command = clipboard_command;
            client.theme = theme;
            client.theme_command = theme_command;
            client.theme_directory = theme_directory;
            client.mouse = mouse;
            client.bell_style = bell_style;
            client.colors = ColorDepth::of(truecolor);
            client.initialized = true;
        }
        self.theme = theme;
        self.set_client_session(id, session_id);
        let shimmer = self.bells_shimmer();
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            play_bell_once(&mut session.windows[session.current_window].bell, shimmer);
        }
        self.remember_active_pane(id)?;
        self.resize_active(id)?;
        self.save_state_soon();
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
        let content_cols = cols.saturating_sub(bar_width).max(1);
        let content_rows = rows.max(1);
        let pane = self.create_pane(cwd, content_cols, content_rows)?;
        let pane_id = pane.id;
        Ok(Window {
            panes: vec![pane],
            layout: PaneLayout::Pane(pane_id),
            active_pane: pane_id,
            previous_pane: pane_id,
            bell: None,
            zoomed: false,
            name: None,
        })
    }

    fn create_pane(&mut self, cwd: &Path, cols: u16, rows: u16) -> Result<Pane> {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        let mut history = PaneJournal::new(self.persistence.new_pane_history(id)?, 0);
        history.append_resize(rows.max(1), cols.max(1))?;
        self.spawn_pane(id, cwd, cols, rows, history)
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
                        let mut parser_prefix = Vec::new();
                        let Some((reader, history_length)) = restored else {
                            return (id, ReplayedPane {
                                parser,
                                parser_prefix,
                                valid_length: 0,
                                history_length: 0,
                                replayed: false,
                            });
                        };
                        match replay_pane_journal(&mut parser, &mut parser_prefix, reader) {
                            Ok(valid_length) => (id, ReplayedPane {
                                parser,
                                parser_prefix,
                                valid_length,
                                history_length,
                                replayed: true,
                            }),
                            // A corrupt journal costs this pane its scrollback
                            // rather than the session it belongs to.
                            Err(_) => (id, ReplayedPane {
                                parser: new_parser(rows, cols),
                                parser_prefix: Vec::new(),
                                valid_length: 0,
                                history_length,
                                replayed: true,
                            }),
                        }
                    })
                })
                .collect();
            for worker in workers {
                match worker.join() {
                    Ok((id, pane)) => {
                        replayed.insert(id, pane);
                    }
                    Err(_) => bail!("a pane's history could not be read"),
                }
            }
            Ok(())
        })?;
        Ok(replayed)
    }

    fn spawn_pane(
        &self,
        id: usize,
        cwd: &Path,
        cols: u16,
        rows: u16,
        history: PaneJournal,
    ) -> Result<Pane> {
        let parser = new_parser(rows, cols);
        self.spawn_pane_with(id, cwd, cols, rows, history, parser, Vec::new(), true)
    }

    /// Spawns a pane around a parser someone else has already filled in, which
    /// is how a restored pane gets its scrollback back.
    #[allow(clippy::too_many_arguments)]
    fn spawn_pane_with(
        &self,
        id: usize,
        cwd: &Path,
        cols: u16,
        rows: u16,
        history: PaneJournal,
        mut parser: vt100::Parser<TerminalCallbacks>,
        parser_prefix: Vec<u8>,
        needs_backing: bool,
    ) -> Result<Pane> {
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
        let writer = pair.master.take_writer().context("take PTY writer")?;
        let sender = self.events.clone();
        thread::spawn(move || {
            let mut buffer = vec![0; 32 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => {
                        let _ = sender.send(Event::PtyClosed(id));
                        return;
                    }
                    Ok(length) => {
                        if sender
                            .send(Event::PtyOutput(id, buffer[..length].to_vec()))
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
            parser_prefix,
            query_prefix: Vec::new(),
            cwd: cwd.to_path_buf(),
            cwd_sampled: Instant::now(),
            process_icon: IDLE_ICON,
            process_sampled: Instant::now() - PROCESS_POLL_INTERVAL,
            history,
        })
    }

    fn close_pane(&mut self, pane_id: usize) -> Result<()> {
        let Some((session_index, window_index, pane_index)) = self
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
                            .position(|pane| pane.id == pane_id)
                            .map(|pane_index| (session_index, window_index, pane_index))
                    })
            })
        else {
            return Ok(());
        };
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
        if !removed_window {
            window.layout = window
                .layout
                .clone()
                .without(pane_id)
                .context("pane layout lost its remaining pane")?;
        }
        if let Some(first) = window.panes.first() {
            if window.active_pane == pane_id {
                window.active_pane = first.id;
                window.previous_pane = first.id;
            } else if window.previous_pane == pane_id {
                window.previous_pane = window.active_pane;
            }
        }

        if removed_window {
            self.sessions[session_index].windows.remove(window_index);
        }
        if self.sessions[session_index].windows.is_empty() {
            let session_id = self.sessions[session_index].id;
            self.sessions.remove(session_index);
            let replacement = self.sessions.first().map(|session| session.id);
            let mut detached = Vec::new();
            for (client_id, client) in &mut self.clients {
                if let Some(tree) = &mut client.tree {
                    tree.expanded.remove(&session_id);
                }
                if client.confirmation.as_ref().is_some_and(|confirmation| {
                    matches!(confirmation, Confirmation::KillSession { session_id: target, .. } if *target == session_id)
                }) {
                    client.confirmation = None;
                }
                if client.session_id == Some(session_id) {
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
                    .filter(|previous| *previous != session_id);
            }
            for client_id in detached {
                self.clients.remove(&client_id);
            }
            let shimmer = self.bells_shimmer();
            if let Some(replacement) = replacement
                && let Some(session) = self
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == replacement)
            {
                play_bell_once(&mut session.windows[session.current_window].bell, shimmer);
            }
        } else if removed_window {
            let shimmer = self.bells_shimmer();
            let session = &mut self.sessions[session_index];
            session.current_window = window_index_after_removal(
                session.current_window,
                window_index,
                session.windows.len(),
            );
            play_bell_once(&mut session.windows[session.current_window].bell, shimmer);
        }
        let client_ids: Vec<_> = self.clients.keys().copied().collect();
        for client_id in client_ids {
            self.resize_active(client_id)?;
        }
        if self.last_active_pane == Some(pane_id) {
            self.last_active_pane = self
                .sessions
                .iter()
                .find(|session| session.id == closed_session_id)
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
        let Some((session_index, window_index)) =
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
        else {
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
        let shimmer = self.bells_shimmer();
        let session_id = {
            let session = &mut self.sessions[session_index];
            let pane_id = session.windows[window_index].bell.as_ref().unwrap().pane_id;
            visit_window(session, window_index);
            if session.windows[window_index]
                .panes
                .iter()
                .any(|pane| pane.id == pane_id)
            {
                session.windows[window_index].select_pane(pane_id);
            }
            play_bell_once(&mut session.windows[window_index].bell, shimmer);
            session.id
        };
        self.set_client_session(id, session_id);
        self.clients.get_mut(&id).unwrap().tree = None;
        self.remember_active_pane(id)?;
        self.resize_active(id)?;
        self.save_state_soon();
        Ok(())
    }

    fn new_window(&mut self, id: usize) -> Result<()> {
        let (session_index, _) = self.active_indices(id).context("no active session")?;
        let (cols, rows) = self.client_size(id);
        let root = self.sessions[session_index].root.clone();
        let future_window_count = self.sessions[session_index].windows.len() + 1;
        let window = self.create_window(&root, cols, rows, bar_width(future_window_count))?;
        let session = &mut self.sessions[session_index];
        session.windows.push(window);
        let new_window = session.windows.len() - 1;
        visit_window(session, new_window);
        self.remember_active_pane(id)?;
        self.save_state_soon();
        Ok(())
    }

    fn new_session(&mut self, id: usize) -> Result<()> {
        let root = self
            .active_cwd(id)
            .unwrap_or_else(|| self.clients[&id].cwd.clone());
        let name = self.next_session_name();
        let (cols, rows) = self.client_size(id);
        let session_id = self.create_session(name, root, cols, rows)?;
        self.set_client_session(id, session_id);
        self.remember_active_pane(id)?;
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
        let Some(session_id) = self.clients[&id].previous_session_id else {
            self.set_message(id, "no previous session".into());
            return Ok(());
        };
        if !self.sessions.iter().any(|session| session.id == session_id) {
            self.clients.get_mut(&id).unwrap().previous_session_id = None;
            self.set_message(id, "no previous session".into());
            return Ok(());
        }
        self.set_client_session(id, session_id);
        let shimmer = self.bells_shimmer();
        if let Some((session_index, window_index)) = self.active_indices(id) {
            play_bell_once(
                &mut self.sessions[session_index].windows[window_index].bell,
                shimmer,
            );
        }
        self.clients.get_mut(&id).unwrap().tree = None;
        self.remember_active_pane(id)?;
        self.resize_active(id)?;
        self.save_state_soon();
        Ok(())
    }

    fn start_rename(&mut self, id: usize) {
        let session_id = if let Some(tree) = self.clients[&id].tree.as_ref() {
            let items = self.tree_items(&tree.expanded);
            items
                .get(tree.selected.min(items.len().saturating_sub(1)))
                .map(|item| item.session_id)
        } else {
            self.clients[&id].session_id
        };
        let Some(session) = session_id.and_then(|session_id| {
            self.sessions
                .iter()
                .find(|session| session.id == session_id)
        }) else {
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
        let target = if let Some(tree) = self.clients[&id].tree.as_ref() {
            let items = self.tree_items(&tree.expanded);
            items
                .get(tree.selected.min(items.len().saturating_sub(1)))
                .and_then(|item| Some((item.session_id, item.window?)))
        } else {
            self.active_indices(id)
                .map(|(session_index, window_index)| {
                    (self.sessions[session_index].id, window_index)
                })
        };
        let Some((session_id, window_index)) = target else {
            self.set_message(id, "select a window to rename".into());
            return;
        };
        let name = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.windows.get(window_index))
            .map(|window| window.name.clone().unwrap_or_default());
        let Some(name) = name else {
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
        let Some((session_index, window_index, pane_index)) = self.active_pane_indices(id) else {
            return;
        };
        let pane_id = self.sessions[session_index].windows[window_index].panes[pane_index].id;
        self.clients.get_mut(&id).unwrap().confirmation = Some(Confirmation::KillPane { pane_id });
    }

    fn start_kill_session(&mut self, id: usize, session_id: usize) {
        if !self.sessions.iter().any(|session| session.id == session_id) {
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
        let Some(session) = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        else {
            return Ok(());
        };
        let pane_ids: Vec<_> = session
            .windows
            .iter()
            .flat_map(|window| window.panes.iter().map(|pane| pane.id))
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

    fn finish_rename(&mut self, id: usize) -> Result<()> {
        let Some(rename) = self
            .clients
            .get_mut(&id)
            .and_then(|client| client.rename.take())
        else {
            return Ok(());
        };
        let (session_id, window_index) = match rename.target {
            RenameTarget::Session { session_id } => (session_id, None),
            RenameTarget::Window {
                session_id,
                window_index,
            } => (session_id, Some(window_index)),
        };
        let Some(session_index) = self
            .sessions
            .iter()
            .position(|session| session.id == session_id)
        else {
            return Ok(());
        };
        let name = rename.text;
        if let Some(window_index) = window_index {
            let Some(window) = self.sessions[session_index].windows.get_mut(window_index) else {
                return Ok(());
            };
            // An empty name hands the window back to whatever its program calls
            // itself, which is how a name is cleared.
            window.name = (!name.trim().is_empty()).then_some(name);
            self.save_state_soon();
            return Ok(());
        }
        if name.trim().is_empty() {
            self.set_message(id, "session name cannot be empty".into());
            return Ok(());
        }
        if self
            .sessions
            .iter()
            .enumerate()
            .any(|(index, session)| index != session_index && session.name == name)
        {
            self.set_message(id, format!("session {name:?} already exists"));
            return Ok(());
        }
        self.sessions[session_index].name = name;
        self.save_state_soon();
        Ok(())
    }

    fn split_active_pane(&mut self, id: usize, axis: SplitAxis) -> Result<()> {
        let (session_index, window_index, pane_index) =
            self.active_pane_indices(id).context("no active pane")?;
        let area = self.content_area(id);
        let window = &self.sessions[session_index].windows[window_index];
        let active_pane = window.panes[pane_index].id;
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
        self.remember_active_pane(id)?;
        self.resize_active(id)?;
        self.save_state_soon();
        Ok(())
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
            self.remember_active_pane(id)?;
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
        let pane_id = pane.id;
        let session = &mut self.sessions[session_index];
        session.windows.push(Window {
            panes: vec![pane],
            layout: PaneLayout::Pane(pane_id),
            active_pane: pane_id,
            previous_pane: pane_id,
            bell: None,
            zoomed: false,
            name: None,
        });
        let new_window = session.windows.len() - 1;
        visit_window(session, new_window);
        self.remember_active_pane(id)?;
        self.resize_active(id)?;
        self.save_state_soon();
        Ok(())
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
        let pane_id = pane.id;
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
        self.remember_active_pane(id)?;
        self.resize_active(id)?;
        self.save_state_soon();
        Ok(())
    }

    /// Removes a pane from its window without touching the process inside it,
    /// closing the window if that was the last pane in it.
    fn detach_pane(&mut self, pane_id: usize) -> Option<Pane> {
        let (session_index, window_index, pane_index) =
            self.sessions
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
                                .position(|pane| pane.id == pane_id)
                                .map(|pane_index| (session_index, window_index, pane_index))
                        })
                })?;
        let window = &mut self.sessions[session_index].windows[window_index];
        let pane = window.panes.remove(pane_index);
        window.zoomed = false;
        if window.panes.is_empty() {
            let session = &mut self.sessions[session_index];
            session.windows.remove(window_index);
            session.current_window = window_index_after_removal(
                session.current_window,
                window_index,
                session.windows.len(),
            );
            return Some(pane);
        }
        window.layout = window.layout.clone().without(pane_id)?;
        if window.active_pane == pane_id {
            let first = window.panes[0].id;
            window.active_pane = first;
            window.previous_pane = first;
        } else if window.previous_pane == pane_id {
            window.previous_pane = window.active_pane;
        }
        Some(pane)
    }

    /// Grows the active pane to the whole window, or puts it back.
    fn zoom_pane(&mut self, id: usize) -> Result<()> {
        let Some((session_index, window_index, _)) = self.active_pane_indices(id) else {
            return Ok(());
        };
        let window = &mut self.sessions[session_index].windows[window_index];
        if window.panes.len() < 2 {
            self.set_message(id, "only one pane in this window".into());
            return Ok(());
        }
        window.zoomed = !window.zoomed;
        let zoomed = window.zoomed;
        self.resize_active(id)?;
        self.rebuild_vim(id);
        self.set_message(id, if zoomed { "zoomed" } else { "unzoomed" }.into());
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
            let shimmer = self.bells_shimmer();
            let session = &mut self.sessions[session_index];
            visit_window(session, number - 1);
            play_bell_once(&mut session.windows[session.current_window].bell, shimmer);
            self.remember_active_pane(id)?;
            self.resize_active(id)?;
            self.save_state_soon();
        }
        Ok(())
    }

    fn enter_vim(&mut self, id: usize) {
        let Some((session_index, window_index, pane_index)) = self.active_pane_indices(id) else {
            return;
        };
        let rows = self.clients[&id].rows.max(1) as usize;
        let pane_id = self.sessions[session_index].windows[window_index].panes[pane_index].id;
        if self.clients[&id].vim.contains_key(&pane_id) {
            return;
        }
        let (buffer, cursor) = snapshot_screen(
            self.sessions[session_index].windows[window_index].panes[pane_index]
                .parser
                .screen_mut(),
        );
        self.clients.get_mut(&id).unwrap().vim.insert(
            pane_id,
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
    parser_prefix: Vec<u8>,
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
fn switch_theme(sender: Sender<Event>, id: usize, command: Vec<String>, name: String) {
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

fn copy_to_clipboard(sender: Sender<Event>, id: usize, command: Vec<String>, text: String) {
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
        if self.remember_active_pane(id)? {
            self.save_state_soon();
        }
        if let Some(client) = self.clients.get_mut(&id) {
            client.writer.send(ServerMessage::Detached);
        }
        self.clients.remove(&id);
        Ok(())
    }

    fn choose_tree_item(&mut self, id: usize, item: TreeItem) -> Result<()> {
        let shimmer = self.bells_shimmer();
        if let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == item.session_id)
        {
            if let Some(window) = item.window {
                visit_window(session, window.min(session.windows.len() - 1));
                if let Some(pane) = item.pane {
                    let window = &mut session.windows[session.current_window];
                    if let Some(selected) = window.panes.get(pane) {
                        window.select_pane(selected.id);
                    }
                }
            }
            play_bell_once(&mut session.windows[session.current_window].bell, shimmer);
            self.set_client_session(id, item.session_id);
            self.clients.get_mut(&id).unwrap().tree = None;
            self.remember_active_pane(id)?;
            self.resize_active(id)?;
            self.save_state_soon();
        }
        Ok(())
    }

    fn next_session_name(&self) -> String {
        (1..)
            .map(automatic_session_name)
            .find(|name| self.sessions.iter().all(|session| &session.name != name))
            .unwrap()
    }

    fn last_active_session_id(&self) -> Option<usize> {
        let pane_id = self.last_active_pane?;
        self.sessions.iter().find_map(|session| {
            session
                .windows
                .iter()
                .any(|window| window.panes.iter().any(|pane| pane.id == pane_id))
                .then_some(session.id)
        })
    }

    fn remember_active_pane(&mut self, client_id: usize) -> Result<bool> {
        let Some(pane_id) = self.active_pane(client_id).map(|pane| pane.id) else {
            return Ok(false);
        };
        let changed = self.last_active_pane != Some(pane_id);
        self.last_active_pane = Some(pane_id);
        Ok(changed)
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
        let session_id = self.clients.get(&client_id)?.session_id?;
        let session_index = self
            .sessions
            .iter()
            .position(|session| session.id == session_id)?;
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
            .map(|(session, _)| bar_width(self.sessions[session].windows.len()))
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
        self.sessions
            .iter_mut()
            .flat_map(|session| session.windows.iter_mut())
            .flat_map(|window| window.panes.iter_mut())
            .find(|pane| pane.id == pane_id)
    }

    fn sample_process_icon(&mut self, pane_id: usize) {
        let Some(pane) = self.pane_mut(pane_id) else {
            return;
        };
        pane.process_sampled = Instant::now();
        let sample = ProcessSample {
            pane_id,
            group: pane.master.process_group_leader(),
            child_pid: pane.child_pid,
        };
        // The whole foreground group, not just its leader: a shell keeps the
        // lead while the script or direnv hook it started is busy.
        let _ = self.process_sampler.send(sample);
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

fn automatic_session_name(number: usize) -> String {
    format!("Session {number}")
}

#[cfg(test)]
mod tests;
