use std::{env, fs::File};

use crate::{
    config::{Action, Mode},
    frame::{CursorShape, FrameCursor, Rgb, rgb},
    protocol::{Mouse, MouseButton, MouseKind, read_message},
};

use super::*;

fn rect(row: u16, col: u16, rows: u16, cols: u16) -> Rect {
    Rect {
        row,
        col,
        rows,
        cols,
    }
}

fn split(axis: SplitAxis, first: PaneLayout, second: PaneLayout) -> PaneLayout {
    PaneLayout::Split {
        axis,
        ratio: EVEN_SPLIT,
        first: Box::new(first),
        second: Box::new(second),
    }
}

/// A unique scratch directory for one test, emptied before use.
pub(super) fn scratch(name: &str) -> PathBuf {
    let directory = env::temp_dir().join(format!(
        "mux-{name}-{}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    directory
}

fn test_persistence(directory: &Path) -> Persistence {
    Persistence {
        state_file: directory.join("state.bin"),
        directory: directory.to_path_buf(),
    }
}

/// Paints over a blank screen of the same size and returns the bytes a
/// client receives; `blank` decides whether that previous screen was empty
/// (a fresh client) or already cleared (only touched cells are sent).
fn paint(rows: u16, cols: u16, blank: bool, draw: impl FnOnce(&mut Frame)) -> String {
    let mut previous = Frame::default();
    if blank {
        previous.reset(rows, cols);
    }
    let mut frame = Frame::default();
    frame.reset(rows, cols);
    draw(&mut frame);
    let mut output = Vec::new();
    frame.diff(&previous, TerminalFeatures::FULL, &mut output);
    String::from_utf8(output).unwrap()
}

fn painted(rows: u16, cols: u16, draw: impl FnOnce(&mut Frame)) -> String {
    paint(rows, cols, false, draw)
}

fn repainted(rows: u16, cols: u16, draw: impl FnOnce(&mut Frame)) -> String {
    paint(rows, cols, true, draw)
}

/// What a terminal shows after receiving `output`.
fn shown(rows: u16, cols: u16, output: &str) -> vt100::Parser {
    let mut terminal = vt100::Parser::new(rows, cols, 0);
    terminal.process(output.as_bytes());
    terminal
}

fn binding(mode: Mode, key: &str) -> Option<Action> {
    Bindings::defaults().get(mode, &crate::config::parse_key(key).unwrap())
}

const RENDER_BARRIER: &str = "mux-test-render-barrier";

/// Renders, then decodes every frame client `id` was sent into `terminal`.
/// A marker queued behind the frames ends the read, so a slow machine cannot
/// cut it short the way a read timeout could.
pub(super) fn render_frames(
    server: &mut Server,
    id: usize,
    client: &mut UnixStream,
    terminal: &mut vt100::Parser,
) {
    server.render_all();
    assert!(
        server.clients[&id]
            .writer
            .send(ServerMessage::Listing(vec![RENDER_BARRIER.into()]))
    );
    loop {
        match read_message::<ServerMessage>(client) {
            Ok(Some(ServerMessage::Render(bytes))) => terminal.process(&bytes),
            Ok(Some(ServerMessage::Error(error))) => panic!("render failed: {error}"),
            Ok(Some(ServerMessage::Listing(lines))) if lines == [RENDER_BARRIER] => break,
            Ok(Some(_)) => {}
            Ok(None) => panic!("client disconnected"),
            Err(error) => panic!("read client frame: {error:#}"),
        }
    }
}

pub(super) fn hello(rows: u16, cols: u16, cwd: PathBuf, session: &str) -> ClientMessage {
    let settings = crate::config::Settings::default();
    ClientMessage::Hello(Box::new(Hello {
        rows,
        cols,
        cwd,
        session: Some(session.into()),
        bindings: settings.bindings,
        clipboard_command: settings.clipboard_command,
        terminal_clipboard: false,
        theme: settings.theme,
        theme_command: settings.theme_command,
        theme_directory: settings.theme_directory,
        mouse: settings.mouse,
        bell_style: settings.bell_style,
        terminal: TerminalFeatures::FULL,
        glyphs: crate::config::Glyphs::Font,
        default_cursor_shape: settings.default_cursor_shape,
    }))
}

/// A daemon with one 80x24 session, "before", running a real shell, and
/// client 1 connected but not attached. Panes and files go on drop.
pub(super) struct TestServer {
    pub(super) server: Server,
    pub(super) events: Receiver<Event>,
    pub(super) client: UnixStream,
    pub(super) directory: PathBuf,
}

fn test_server(directory: &Path) -> (Server, Receiver<Event>, UnixStream) {
    let persistence = test_persistence(directory);
    let (events, receiver) = mpsc::sync_channel(EVENT_QUEUE_DEPTH);
    let mut server = Server {
        socket_path: directory.join("mux.sock"),
        zsh_startup: ZshStartup::create(directory).unwrap(),
        state_writer: persistence.state_writer(),
        persistence,
        process_sampler: mpsc::channel().0,
        events,
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
    server
        .create_session("before".into(), directory.to_path_buf(), 80, 24)
        .unwrap();
    // Keep process-icon sampling out of the way unless a test opts in.
    server.sessions[0].windows[0].panes[0].process_pending = true;
    let (writer, client) = UnixStream::pair().unwrap();
    server.handle_event(Event::Connected(1, writer)).unwrap();
    (server, receiver, client)
}

/// [`TestServer`] without cleanup, in a new directory, with a bell ringing.
pub(super) fn server_with_pending_bell(directory: &Path) -> (Server, Receiver<Event>, UnixStream) {
    fs::create_dir(directory).unwrap();
    let (mut server, events, client) = test_server(directory);
    server.ring_bell(0, 1);
    assert!(server.bells_animating());
    (server, events, client)
}

impl TestServer {
    pub(super) fn new(name: &str) -> Self {
        let directory = scratch(name);
        let (server, receiver, client) = test_server(&directory);
        client
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        Self {
            server,
            events: receiver,
            client,
            directory,
        }
    }

    /// Attaches client 1 to the first session.
    pub(super) fn attach(&mut self) {
        let client = self.server.clients.get_mut(&1).unwrap();
        client.initialized = true;
        client.session_id = Some(0);
    }

    pub(super) fn ring_bell(&mut self) {
        self.server.ring_bell(0, 1);
        assert!(self.server.bells_animating());
    }

    /// Client 1's active pane.
    pub(super) fn pane(&self) -> &Pane {
        let (s, w) = self.server.active_indices(1).unwrap();
        let window = &self.server.sessions[s].windows[w];
        window
            .panes
            .iter()
            .find(|pane| pane.id == window.active_pane)
            .unwrap()
    }

    /// Handles daemon events until the active pane shows `marker`.
    pub(super) fn wait_output(&mut self, marker: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.pane().parser.screen().contents().contains(marker) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "PTY did not produce {marker:?}");
            let event = self.events.recv_timeout(remaining).unwrap();
            self.server.handle_event(event).unwrap();
        }
    }

    pub(super) fn send(&mut self, id: usize, message: ClientMessage) {
        self.server
            .handle_event(Event::Client(id, message))
            .unwrap();
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        for session in &mut self.server.sessions {
            for window in &mut session.windows {
                for pane in &mut window.panes {
                    let _ = pane.child.kill();
                }
            }
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn older_persisted_state_versions_migrate() {
    let migrated = decode_persisted_state(&encode(&PersistedStateV1 {
        version: LEGACY_STATE_VERSION,
        next_session_id: 3,
        next_pane_id: 7,
        sessions: Vec::new(),
    }))
    .unwrap();
    assert_eq!(migrated.version, STATE_VERSION);
    assert_eq!((migrated.next_session_id, migrated.next_pane_id), (3, 7));
    assert_eq!(migrated.last_active_pane, None);

    // State saved before resizable splits comes back split evenly.
    let pane = |id| PersistedPane {
        id,
        cwd: PathBuf::from("/tmp"),
        rows: 10,
        cols: 40,
    };
    let migrated = decode_persisted_state(&encode(&PersistedStateV2 {
        version: EVEN_SPLIT_STATE_VERSION,
        next_session_id: 1,
        next_pane_id: 2,
        last_active_pane: Some(1),
        sessions: vec![PersistedSessionV2 {
            id: 0,
            name: "work".into(),
            root: PathBuf::from("/tmp"),
            current_window: 0,
            windows: vec![PersistedWindowV2 {
                active_pane: 0,
                layout: PaneLayoutV2::Split {
                    axis: SplitAxis::Vertical,
                    first: Box::new(PaneLayoutV2::Pane(0)),
                    second: Box::new(PaneLayoutV2::Pane(1)),
                },
                panes: vec![pane(0), pane(1)],
            }],
        }],
    }))
    .unwrap();
    assert_eq!(migrated.version, STATE_VERSION);
    assert_eq!(migrated.last_active_pane, Some(1));
    let PaneLayout::Split { ratio, .. } = &migrated.sessions[0].windows[0].layout else {
        panic!("the split survived the migration");
    };
    assert_eq!(*ratio, EVEN_SPLIT);
}

fn encode(state: &impl serde::Serialize) -> Vec<u8> {
    bincode::serde::encode_to_vec(state, bincode::config::standard()).unwrap()
}

#[test]
fn mouse_events_reach_only_the_programs_that_asked_for_them() {
    let click = Mouse {
        kind: MouseKind::Down,
        button: MouseButton::Left,
        col: 9,
        row: 4,
        modifiers: 0,
    };
    let release = Mouse {
        kind: MouseKind::Up,
        ..click
    };
    let wheel = Mouse {
        kind: MouseKind::ScrollUp,
        ..click
    };
    let report = |modes: &[u8], mouse| {
        let mut parser = new_parser(24, 80);
        parser.process(modes);
        mouse_report(parser.screen(), mouse)
    };
    // A plain shell has not asked for the mouse, so mux keeps the event.
    assert_eq!(report(b"", click), None);
    let sgr = b"\x1b[?1000h\x1b[?1006h";
    assert_eq!(report(sgr, click).unwrap(), b"\x1b[<0;10;5M");
    assert_eq!(report(sgr, release).unwrap(), b"\x1b[<0;10;5m");
    assert_eq!(report(sgr, wheel).unwrap(), b"\x1b[<64;10;5M");
    // Press-only mode reports the press but not the release.
    assert!(report(b"\x1b[?9h\x1b[?1006h", click).is_some());
    assert_eq!(report(b"\x1b[?9h\x1b[?1006h", release), None);
    // Without SGR the report is the original three-byte form.
    assert_eq!(
        report(b"\x1b[?1000h", click).unwrap(),
        [0x1b, b'[', b'M', 32, 32 + 10, 32 + 5]
    );
}

#[test]
fn a_pane_reports_the_title_its_program_sets() {
    let mut parser = new_parser(4, 20);
    assert_eq!(parser.callbacks().title, None);
    parser.process(b"\x1b]2;vim README.md\x07");
    assert_eq!(parser.callbacks().title.as_deref(), Some("vim README.md"));
    // OSC 0 sets the icon name as well, and mux treats it the same.
    parser.process(b"\x1b]0;zsh\x1b\\");
    assert_eq!(parser.callbacks().title.as_deref(), Some("zsh"));
    parser.process(b"\x1b]2;\x07");
    assert_eq!(parser.callbacks().title, None);
}

#[test]
fn a_zoomed_window_shows_only_the_active_pane() {
    let area = rect(0, 0, 24, 80);
    let layout = split(
        SplitAxis::Vertical,
        PaneLayout::Pane(1),
        PaneLayout::Pane(2),
    );
    let (regions, dividers) = window_regions(&layout, 2, true, area);
    assert_eq!(regions, vec![(2, area)]);
    assert!(dividers.is_empty());
    // The layout itself is untouched, so unzooming restores the split.
    let (regions, dividers) = window_regions(&layout, 2, false, area);
    assert_eq!((regions.len(), dividers.len()), (2, 1));
}

#[test]
fn dividers_that_meet_are_joined_instead_of_overwriting_each_other() {
    let area = rect(0, 0, 5, 7);
    let stacked = |a, b| {
        split(
            SplitAxis::Horizontal,
            PaneLayout::Pane(a),
            PaneLayout::Pane(b),
        )
    };
    // Panes 1 and 2 stacked on the left, pane 3 down the right-hand side.
    let layout = split(SplitAxis::Vertical, stacked(1, 2), PaneLayout::Pane(3));
    let (_, dividers) = window_regions(&layout, 1, false, area);
    assert_eq!(
        divider_cells(&dividers, 0),
        vec![
            ((1, 4), "│"),
            ((2, 4), "│"),
            ((3, 1), "─"),
            ((3, 2), "─"),
            ((3, 3), "─"),
            ((3, 4), "┤"),
            ((4, 4), "│"),
            ((5, 4), "│"),
        ]
    );

    // A pane on each side of the vertical divider splits it into a cross,
    // and the bar offset shifts it right.
    let layout = split(SplitAxis::Vertical, stacked(1, 2), stacked(3, 4));
    let (_, dividers) = window_regions(&layout, 1, false, area);
    let junctions: Vec<_> = divider_cells(&dividers, 5)
        .into_iter()
        .filter(|(_, glyph)| !matches!(*glyph, "│" | "─"))
        .collect();
    assert_eq!(junctions, vec![((3, 9), "┼")]);

    assert_eq!(divider_glyph(JOIN_LEFT | JOIN_RIGHT | JOIN_DOWN), "┬");
    assert_eq!(divider_glyph(JOIN_LEFT | JOIN_RIGHT | JOIN_UP), "┴");
    assert_eq!(divider_glyph(JOIN_UP | JOIN_DOWN | JOIN_RIGHT), "├");
}

#[test]
fn resizing_moves_the_nearest_divider_and_stops_at_the_edges() {
    let area = rect(0, 0, 24, 81);
    let mut layout = split(
        SplitAxis::Vertical,
        PaneLayout::Pane(1),
        split(
            SplitAxis::Vertical,
            PaneLayout::Pane(2),
            PaneLayout::Pane(3),
        ),
    );
    let width = |layout: &PaneLayout, pane_id: usize| {
        let (regions, _) = pane_layout_regions(layout, area);
        regions
            .iter()
            .find_map(|(id, rect)| (*id == pane_id).then_some(rect.cols))
            .unwrap()
    };
    let (before_1, before_2) = (width(&layout, 1), width(&layout, 2));

    // Pane 2 sits against two dividers; the inner one is the one that moves.
    assert!(layout.resize(area, 2, SplitAxis::Vertical, 4));
    assert_eq!(width(&layout, 1), before_1);
    assert_eq!(width(&layout, 2), before_2 + 4);
    // Pane 1 only has the outer divider, so that one moves instead.
    assert!(layout.resize(area, 1, SplitAxis::Vertical, 6));
    assert_eq!(width(&layout, 1), before_1 + 6);
    // A split cannot be pushed past its neighbour or the wrong way.
    assert!(!layout.resize(area, 1, SplitAxis::Horizontal, 3));
    while layout.resize(area, 1, SplitAxis::Vertical, 10) {}
    assert!(width(&layout, 1) < area.cols);
    assert!(width(&layout, 2) >= 1 && width(&layout, 3) >= 1);
}

#[test]
fn pane_layout_splits_collapses_and_finds_neighbors() {
    let area = rect(0, 0, 10, 20);
    let mut layout = PaneLayout::Pane(1);
    assert!(layout.split(1, 2, SplitAxis::Vertical));
    assert!(layout.split(2, 3, SplitAxis::Horizontal));
    let (regions, dividers) = pane_layout_regions(&layout, area);
    assert_eq!(
        regions,
        vec![
            (1, rect(0, 0, 10, 9)),
            (2, rect(0, 10, 4, 10)),
            (3, rect(5, 10, 5, 10)),
        ]
    );
    assert_eq!(dividers.len(), 2);
    let neighbor =
        |from, previous, direction| neighboring_pane(&regions, from, previous, direction);
    assert_eq!(neighbor(1, None, PaneDirection::Right), Some(3));
    assert_eq!(neighbor(2, None, PaneDirection::Down), Some(3));
    assert_eq!(neighbor(3, None, PaneDirection::Left), Some(1));
    // On a tie, navigation returns to the pane it came from.
    assert_eq!(neighbor(1, Some(2), PaneDirection::Right), Some(2));

    let (regions, _) = pane_layout_regions(&layout.without(2).unwrap(), area);
    assert_eq!(
        regions.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        [1, 3]
    );
}

#[test]
fn default_bindings_leave_tree_and_picker_commands_reachable() {
    for (mode, key, action) in [
        (Mode::Normal, "Alt-c", Some(Action::ThemePicker)),
        (Mode::Leader, "z", None),
        (Mode::Leader, "x", Some(Action::KillPane)),
        (Mode::Tree, "x", Some(Action::KillSession)),
        // Direct selection keys leave the leader and tree commands alone.
        (Mode::Tree, "Alt-a", Some(Action::EnterLeader)),
        (Mode::Tree, "Alt-s", Some(Action::SessionTree)),
        (Mode::Tree, "0", Some(Action::TreeSelect(10))),
        (Mode::Tree, "Alt-b", Some(Action::TreeSelect(11))),
        (Mode::Tree, "Alt-z", Some(Action::TreeSelect(35))),
        (Mode::Theme, "2", Some(Action::ThemeSelect(2))),
        // Nothing that acts on a session reaches the picker.
        (Mode::Theme, "Alt-t", None),
    ] {
        assert_eq!(binding(mode, key), action, "{mode:?} {key}");
    }
    assert_eq!(tree_shortcut(9).as_deref(), Some("0"));
    assert_eq!(tree_shortcut(10).as_deref(), Some("M-b"));
    assert_eq!(tree_shortcut(27), None);
    assert_eq!(tree_shortcut(34).as_deref(), Some("M-z"));
}

#[test]
fn panes_get_the_terminal_and_key_bytes_their_terminfo_expects() {
    let mut command = CommandBuilder::new("zsh");
    configure_pane_terminal(&mut command);
    assert_eq!(command.get_env("TERM"), Some("xterm-256color".as_ref()));
    assert_eq!(command.get_env("COLORTERM"), Some("truecolor".as_ref()));
    let bytes = |key| terminal_key_bytes(&crate::config::parse_key(key).unwrap(), false);
    assert_eq!(bytes("Backspace"), b"\x7f");
    assert_eq!(bytes("Alt-x"), b"\x1bx");
    assert_eq!(bytes("Ctrl-Left"), b"\x1b[1;5D");
    // Bindings name this key in lowercase, but the pane is owed the capital.
    assert_eq!(bytes("Alt-Shift-a"), b"\x1bA");
}

#[test]
fn zle_cursor_save_restore_keeps_the_entered_command() {
    let mut parser = vt100::Parser::new(4, 40, 0);
    process_terminal_bytes(&mut parser, b"header\r\n\xe2\x9d\xaf echo kept\x1b[");
    process_terminal_bytes(&mut parser, b"s\x1b[1A\x1b[30Gtime\x1b");
    process_terminal_bytes(&mut parser, b"[u\r\r\noutput");
    let rows: Vec<_> = parser.screen().rows(0, 40).collect();
    assert!(rows[1].contains("❯ echo kept"));
    assert_eq!(rows[2], "output");
}

#[test]
fn synchronized_redraw_never_renders_an_intermediate_cursor() {
    let mut parser = new_parser(4, 40);
    process_terminal_bytes(&mut parser, b"\x1b[3;5Hprompt\x1b[6 q");
    let before = parser.screen().contents();
    let cursor = parser.screen().cursor_position();
    // A PTY read can end at every byte, including inside the markers.
    for byte in b"\x1b[?2026h\x1b[1;1Hstatus\x1b[2 q\x1b[3;11H\x1b[6 q\x1b[?2026" {
        process_terminal_bytes(&mut parser, &[*byte]);
        let (screen, shape) = rendered_terminal(&parser, CursorShape::Bar);
        assert_eq!(screen.contents(), before);
        assert_eq!(screen.cursor_position(), cursor);
        assert!(!screen.hide_cursor());
        assert_eq!(shape, CursorShape::Bar);
    }
    process_terminal_bytes(&mut parser, b"l");
    let (screen, shape) = rendered_terminal(&parser, CursorShape::Bar);
    assert!(screen.contents().contains("status"));
    assert_eq!(screen.cursor_position(), cursor);
    assert_eq!(shape, CursorShape::Bar);
    assert!(parser.callbacks().synchronized_output.is_none());
}

#[test]
fn synchronized_output_queries_report_the_mode_at_the_query() {
    let mut parser = new_parser(2, 20);
    for byte in b"\x1b[?2026$p\x1b[?2026h\x1b[?2026$p\x1b[?2026l\x1b[?2026$p" {
        process_terminal_bytes(&mut parser, &[*byte]);
    }
    assert_eq!(
        parser.callbacks().responses,
        b"\x1b[?2026;2$y\x1b[?2026;1$y\x1b[?2026;2$y"
    );
}

#[test]
fn a_repeated_synchronized_begin_keeps_the_original_screen() {
    let mut parser = new_parser(2, 20);
    parser.process(b"ready\x1b[?25l\x1b[?2026h");
    parser.process(b"\x1b[1;1Hpartial\x1b[?25h\x1b[?2026h");
    let (screen, _) = rendered_terminal(&parser, CursorShape::Bar);
    assert_eq!(screen.contents(), "ready");
    assert!(screen.hide_cursor());
    parser.process(b"\x1b[?2026l");
    let (screen, _) = rendered_terminal(&parser, CursorShape::Bar);
    assert_eq!(screen.contents(), "partial");
    assert!(!screen.hide_cursor());
}

#[test]
fn an_unfinished_synchronized_redraw_expires() {
    let mut parser = new_parser(2, 20);
    parser.process(b"ready\x1b[?2026h\x1b[1;1Hupdated");
    let expires = parser
        .callbacks()
        .synchronized_output
        .as_ref()
        .unwrap()
        .expires;
    let contents =
        |parser: &vt100::Parser<_>| rendered_terminal(parser, CursorShape::Bar).0.contents();
    let callbacks = parser.callbacks_mut();
    assert!(!callbacks.expire_synchronized_output(expires - Duration::from_millis(1)));
    assert_eq!(contents(&parser), "ready");
    assert!(parser.callbacks_mut().expire_synchronized_output(expires));
    assert_eq!(contents(&parser), "updated");
    assert!(!parser.callbacks_mut().expire_synchronized_output(expires));
}

#[test]
fn rename_input_edits_at_a_unicode_character_cursor() {
    let mut rename = RenameState {
        target: RenameTarget::Session { session_id: 1 },
        text: "wörk".into(),
        cursor: 2,
    };
    rename.insert('X');
    assert_eq!(rename.text, "wöXrk");
    rename.backspace();
    rename.delete();
    assert_eq!(rename.text, "wök");

    // Terminal-style line editing.
    rename.text = "one  twö three".into();
    rename.cursor = 9;
    rename.delete_word_before_cursor();
    assert_eq!((rename.text.as_str(), rename.cursor), ("one  three", 5));
    rename.delete_before_cursor();
    assert_eq!((rename.text.as_str(), rename.cursor), ("three", 0));
    rename.cursor = 2;
    rename.delete_after_cursor();
    assert_eq!(rename.text, "th");
}

#[test]
fn the_state_dot_colours_the_mode_that_owns_the_keys() {
    let theme = Theme::default();
    let lit = |state: Rgb| (state, theme.bar_label_foreground);
    assert_eq!(
        state_colors(false, false, false, &theme),
        (theme.state_normal, theme.state_normal_dot)
    );
    assert_eq!(
        state_colors(false, false, true, &theme),
        lit(theme.state_vim)
    );
    assert_eq!(
        state_colors(false, true, true, &theme),
        lit(theme.state_leader)
    );
    // A second leader hands the next key over, so mux stops claiming it.
    assert_eq!(
        state_colors(true, true, true, &theme),
        lit(theme.state_passthrough)
    );
}

fn popup(
    rows: u16,
    cols: u16,
    anchor: PopupAnchor,
    popup: &Popup,
    behind: Option<FrameCursor>,
) -> String {
    repainted(rows, cols, |frame| {
        if let Some(cursor) = behind {
            frame.set_cursor(cursor);
        }
        render_popup_box(frame, (rows, cols), anchor, popup, &Theme::default());
    })
}

#[test]
fn popups_are_placed_by_their_anchor_and_fit_small_screens() {
    let status = Popup::Status("leader".into());
    let output = popup(9, 30, PopupAnchor::Center, &status, None);
    // Three rows tall, in the middle of nine.
    let rows: Vec<String> = shown(9, 30, &output).screen().rows(0, 30).collect();
    assert_eq!(rows[3].trim(), "╭────────╮", "{output:?}");
    assert_eq!(rows[4].trim(), "│ leader │", "{output:?}");
    assert_eq!(rows[5].trim(), "╰────────╯", "{output:?}");
    assert!(output.contains("\x1b[4;11H"), "{output:?}");

    // A reporting popup sits flush with the last row, centred across.
    let output = popup(9, 30, PopupAnchor::Bottom, &status, None);
    assert!(output.contains("\x1b[7;11H"), "{output:?}");
    assert!(output.contains("\x1b[9;11H╰"), "{output:?}");

    // Too short for a border: the text alone takes the last row.
    let output = popup(
        2,
        4,
        PopupAnchor::Bottom,
        &Popup::Status("yanked".into()),
        None,
    );
    assert!(output.contains("\x1b[2;1H"), "{output:?}");
    assert!(output.contains("yank"), "{output:?}");

    // Text scrolls to keep the input cursor visible.
    assert_eq!(
        popup_text_window("rename session: abcdef", Some(22), 8),
        (" abcdef".into(), Some(7))
    );
}

#[test]
fn only_a_popup_with_an_input_field_takes_the_pane_cursor() {
    let behind = FrameCursor {
        row: 3,
        col: 8,
        shape: CursorShape::Bar,
        visible: true,
    };
    for message in [
        Popup::Status("yanked".into()),
        Popup::Warning("kill pane 1?".into()),
    ] {
        let output = popup(9, 30, PopupAnchor::Center, &message, Some(behind));
        assert!(output.contains("\x1b[3;8H"), "{output:?}");
        assert!(output.contains("\x1b[6 q"), "{output:?}");
        assert!(output.contains("\x1b[?25h"), "{output:?}");
    }
    let rename = Popup::Rename {
        text: "rename session: smoke".into(),
        cursor: 21,
        shape: CursorShape::Block,
    };
    let output = popup(9, 40, PopupAnchor::Center, &rename, Some(behind));
    assert!(output.contains("rename session: smoke"), "{output:?}");
    assert!(!output.contains("\x1b[3;8H"), "{output:?}");
    assert!(output.contains("\x1b[2 q"), "{output:?}");
}

#[test]
fn a_preview_borrows_the_cursor_of_the_pane_it_shows() {
    let mut parser = vt100::Parser::new(4, 10, 0);
    parser.process(b"one\r\ntwo\r\nthree");
    let cursor = |parser: &vt100::Parser, top, area| {
        preview_cursor(parser.screen(), CursorShape::Block, top, area)
            .map(|cursor| (cursor.row, cursor.col, cursor.visible))
    };
    // Row 2 of a three-row slice starting at row 1, column 6 of the pane.
    assert_eq!(cursor(&parser, 1, rect(4, 20, 3, 10)), Some((5, 25, true)));
    // A cursor outside the slice is clamped into it.
    assert_eq!(cursor(&parser, 0, rect(4, 20, 2, 3)), Some((5, 22, true)));
    assert_eq!(cursor(&parser, 0, rect(4, 20, 3, 0)), None);
    // A pane that hides its cursor still gives the preview a position.
    parser.process(b"\x1b[?25l");
    assert_eq!(cursor(&parser, 1, rect(4, 20, 3, 10)), Some((5, 25, false)));
}

#[test]
fn pane_preview_renders_current_terminal_cells() {
    let mut parser = vt100::Parser::new(3, 12, 0);
    parser.process(b"live preview");
    let output = painted(3, 12, |frame| {
        render_screen_region(frame, parser.screen(), 0, rect(1, 1, 3, 12))
    });
    assert!(output.contains("live preview"), "{output:?}");

    let mut parser = vt100::Parser::new(10, 12, 0);
    parser.process(b"older\r\nlatest");
    assert_eq!(preview_source_region(parser.screen(), 1), (1, 1));
    assert_eq!(preview_source_region(parser.screen(), 4), (0, 2));
}

#[test]
fn session_preview_grid_shows_every_window() {
    let area = rect(4, 20, 20, 60);
    let three = preview_grid_rects(3, area);
    assert_eq!(three.len(), 3);
    assert!(three.iter().all(|rect| rect.rows == 20 && rect.cols > 0));
    assert!(three.windows(2).all(|pair| pair[0].col < pair[1].col));
    let (vertical, horizontal) = preview_grid_separator_positions(area, &three);
    assert_eq!((vertical.len(), horizontal.len()), (2, 0));

    let four = preview_grid_rects(4, area);
    assert_eq!(four.len(), 4);
    assert_eq!(four[0].row, four[1].row);
    assert!(four[2].row > four[0].row);
    let (vertical, horizontal) = preview_grid_separator_positions(area, &four);
    assert_eq!((vertical.len(), horizontal.len()), (1, 1));
}

#[test]
fn tree_display_adds_root_rows_without_consuming_shortcuts() {
    let item = |window, label: &str| TreeItem {
        session_id: 1,
        window,
        pane: None,
        label: label.into(),
    };
    let rows = tree_display_rows(&[item(None, "work"), item(Some(0), "window 1 · 1 pane")]);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].item_index, 0);
    assert!(matches!(rows[1].kind, TreeRowKind::Root));
    assert_eq!(rows[2].item_index, 1);
    assert_eq!(
        two_sided_line(" work", "1 window ", 20),
        " work      1 window "
    );
}

#[test]
fn bar_text_and_prompts_follow_their_contents() {
    assert_eq!(
        [bar_width(1), bar_width(9), bar_width(10), bar_width(100)],
        [5, 5, 6, 7]
    );
    assert_eq!(bar_label(10, 2), " 10 ");
    assert_eq!(bar_window_label(0, 0, 1), " • ");
    assert_eq!(bar_window_label(9, 0, 2), " 10 ");
    assert_eq!(centered_bar_layout(1, 0, 9), (0, 3, 1));
    assert_eq!(centered_bar_layout(3, 1, 9), (0, 0, 3));
    assert_eq!(centered_bar_layout(12, 10, 5), (10, 1, 1));
    assert_eq!(kill_pane_prompt(Some(2)), "kill pane 2? [y/N]");
    assert_eq!(
        kill_session_prompt(Some(("work", 2))),
        "kill session \"work\" and its 2 panes? [y/N]"
    );
    assert_eq!(
        kill_session_prompt(Some(("work", 1))),
        "kill session \"work\" and its 1 pane? [y/N]"
    );
    // Whatever the question was about may be gone by the time it is drawn.
    assert_eq!(kill_pane_prompt(None), "kill pane? [y/N]");
    assert_eq!(kill_session_prompt(None), "kill session? [y/N]");

    let separator = painted(3, 4, |frame| {
        render_bar_separator(
            frame,
            3,
            bar_width(1),
            Some(2),
            Theme::default().bar_active,
            crate::config::Glyphs::Font,
        )
    });
    assert!(separator.contains("38;2;203;163;210"), "{separator:?}");
    for glyph in ["", "", ""] {
        assert!(separator.contains(glyph), "{separator:?}");
    }
    assert!(!separator.contains("48;2"), "{separator:?}");
}

fn shimmering(elapsed: u128) -> BellVisual {
    BellVisual {
        shimmer: Some(elapsed),
        slide: BellSlide::Covered,
    }
}

fn bell_label(text: &str, visual: BellVisual, width: u16, theme: &Theme) -> String {
    painted(1, width, |frame| {
        render_bell_label(
            frame,
            (1, 1),
            text,
            BellLabel {
                visual,
                animation_width: width.into(),
                resting: ((40, 40, 40), theme.bar_label_foreground),
                bold: false,
            },
            theme,
        )
    })
}

#[test]
fn a_bell_shimmer_sweeps_smoothly_across_its_label() {
    let mut parser = new_parser(2, 8);
    parser.process(b"\x1b]2;window title\x07");
    assert_eq!(
        parser.callbacks().bell_count,
        0,
        "OSC terminators are not bells"
    );
    parser.process(b"\x07\x1bg");
    assert_eq!(parser.callbacks().bell_count, 2);

    let theme = Theme::default();
    let color =
        |elapsed, cell, width| bell_visual_colors(shimmering(elapsed), cell, width, &theme).0;
    let midway = BELL_SHIMMER_MICROS / 2;
    let output = bell_label(" 1 ", shimmering(midway), 4, &theme);
    let (red, green, blue) = color(midway, 1, 4);
    assert!(
        output.contains(&format!("48;2;{red};{green};{blue}")),
        "{output:?}"
    );

    let resting = BellVisual {
        shimmer: None,
        slide: BellSlide::Covered,
    };
    let output = bell_label(" ! ", resting, 3, &theme);
    let (text, base) = (theme.bell_text, theme.bell_base);
    assert!(
        output.contains(&format!(
            "38;2;{};{};{};48;2;{};{};{}",
            text.0, text.1, text.2, base.0, base.1, base.2
        )),
        "{output:?}"
    );

    // The brightest cell walks from the left edge to the right one.
    let peaks: Vec<usize> = (10..=90)
        .map(|percent| {
            (0..20)
                .max_by_key(|cell| color(BELL_SHIMMER_MICROS * percent / 100, *cell, 20).1)
                .unwrap()
        })
        .collect();
    assert!(peaks.windows(2).all(|pair| pair[0] <= pair[1]), "{peaks:?}");
    assert_eq!((peaks[0], *peaks.last().unwrap()), (0, 19), "{peaks:?}");

    // It starts and ends at the bell's base colour without jumps between.
    assert_eq!(color(0, 0, 3), theme.bell_base);
    assert_eq!(color(BELL_SHIMMER_MICROS - 1, 2, 3), theme.bell_base);
    let frames: Vec<[Rgb; 3]> = (0..BELL_SHIMMER_MICROS / 1_000)
        .map(|frame| std::array::from_fn(|cell| color(frame * 1_000, cell, 3)))
        .collect();
    assert!(frames.windows(2).all(|pair| {
        pair[0].iter().zip(pair[1]).all(|(previous, current)| {
            previous.0.abs_diff(current.0) <= 12
                && previous.1.abs_diff(current.1) <= 12
                && previous.2.abs_diff(current.2) <= 12
        })
    }));

    assert_eq!(other_session_bell_label(1), " ! ");
    assert_eq!(other_session_bell_label(2), " 2 ");
}

#[test]
fn a_bell_colour_slides_onto_and_off_its_label() {
    let coverage = |slide: BellSlide| -> [u16; 5] {
        std::array::from_fn(|cell| bell_coverage(slide, cell, 5))
    };
    assert_eq!(coverage(BellSlide::Covered), [255; 5]);
    assert_eq!(coverage(BellSlide::In(0)), [0; 5]);
    assert_eq!(coverage(BellSlide::In(255)), [255; 5]);
    assert_eq!(coverage(BellSlide::Out(255)), [255; 5]);
    assert_eq!(coverage(BellSlide::Out(0)), [0; 5]);
    // Arriving fills from the left, leaving empties from the left.
    let arriving = coverage(BellSlide::In(128));
    assert!(arriving.windows(2).all(|pair| pair[0] >= pair[1]) && arriving[0] > arriving[4]);
    let leaving = coverage(BellSlide::Out(128));
    assert!(leaving.windows(2).all(|pair| pair[0] <= pair[1]) && leaving[4] > leaving[0]);
    // Each step moves it a little, never a whole cell at once.
    let steps: Vec<_> = (0..=255)
        .map(|step| coverage(BellSlide::In(step)))
        .collect();
    assert!(steps.windows(2).all(|pair| {
        pair[0]
            .iter()
            .zip(pair[1])
            .all(|(previous, current)| current >= *previous && current - previous <= 24)
    }));
    // A slide blends against the label's own colours, so nothing jumps.
    let theme = Theme::default();
    let label = ((40, 40, 40), theme.bar_label_foreground);
    for (slide, cell) in [(BellSlide::In(0), 0), (BellSlide::Out(0), 4)] {
        let visual = BellVisual {
            shimmer: None,
            slide,
        };
        assert_eq!(bell_cell_colors(visual, cell, 5, label, &theme), label);
    }
}

fn pending_bell() -> BellState {
    BellState {
        appeared: Instant::now() - Duration::from_secs(10),
        started: Instant::now() - Duration::from_secs(10),
        render_token: 42,
        count: 3,
        repeat: true,
        pane_id: 7,
    }
}

#[test]
fn visiting_a_bell_starts_one_complete_active_pass() {
    let mut bell = Some(pending_bell());
    let appeared = bell.as_ref().unwrap().appeared;
    play_bell_once(&mut bell, true);
    let bell = bell.unwrap();
    assert!(!bell.repeat);
    assert_eq!((bell.render_token, bell.count, bell.pane_id), (0, 3, 7));
    assert!(bell.started.elapsed() < Duration::from_millis(50));
    // Replaying keeps the original arrival, so the colour does not slide in
    // again over a label it already covers.
    assert_eq!(bell.appeared, appeared);
    let visual = bell_visual(&bell, BellStyle::Shimmer).unwrap();
    assert!(matches!(visual.slide, BellSlide::Covered));
}

#[test]
fn a_bell_without_a_shimmer_rests_bright_or_is_not_drawn_at_all() {
    let theme = Theme::default();
    let bell = pending_bell();
    let steady = bell_visual(&bell, BellStyle::Steady).unwrap();
    assert!(steady.shimmer.is_none());
    assert!(matches!(steady.slide, BellSlide::Covered));
    assert!((0..3).all(|cell| {
        bell_visual_colors(steady, cell, 3, &theme) == (theme.bell_base, theme.bell_text)
    }));
    assert!(bell_visual(&bell, BellStyle::None).is_none());
    // With nobody watching a shimmer, a visited bell is done immediately.
    let mut pending = Some(bell);
    play_bell_once(&mut pending, false);
    assert!(pending.is_none());
}

#[test]
fn cursor_shapes_follow_applications_and_reset_to_each_clients_default() {
    let mut parser = new_parser(2, 8);
    let defaults = [CursorShape::Bar, CursorShape::Block, CursorShape::Underline];
    let shape = |parser: &vt100::Parser<_>, default| rendered_terminal(parser, default).1;
    for (sequence, expected) in [
        (&b"\x1b[5 q"[..], CursorShape::Bar),
        (b"\x1b[3 q", CursorShape::Underline),
        (b"\x1b[1 q", CursorShape::Block),
        (b"\x1b]50;CursorShape=1\x07", CursorShape::Bar),
    ] {
        parser.process(sequence);
        assert_eq!(parser.callbacks().cursor_shape, Some(expected));
    }
    for reset in [b"\x1b[0 q".as_slice(), b"\x1b[ q"] {
        parser.process(b"\x1b[2 q");
        assert_eq!(shape(&parser, CursorShape::Bar), CursorShape::Block);
        parser.process(reset);
        for default in defaults {
            assert_eq!(shape(&parser, default), default);
        }
    }
    // A reset inside a synchronized update waits for the update to end.
    parser.process(b"\x1b[2 q\x1b[?2026h\x1b[0 q");
    assert_eq!(shape(&parser, CursorShape::Bar), CursorShape::Block);
    parser.process(b"\x1b[?2026l");
    assert_eq!(shape(&parser, CursorShape::Bar), CursorShape::Bar);

    // Shapes are drawn without blinking.
    for (shape, sequence) in [
        (CursorShape::Block, "\x1b[?12l\x1b[2 q"),
        (CursorShape::Underline, "\x1b[?12l\x1b[4 q"),
        (CursorShape::Bar, "\x1b[?12l\x1b[6 q"),
    ] {
        let output = painted(1, 1, |frame| {
            frame.set_cursor(FrameCursor {
                row: 1,
                col: 1,
                shape,
                visible: true,
            })
        });
        assert!(output.contains(sequence), "{output:?}");
    }
}

#[test]
fn a_private_directory_is_created_and_a_shared_one_is_refused() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root = scratch("private");
    let created = root.join("runtime");
    private_directory(&created).unwrap();
    assert_eq!(fs::metadata(&created).unwrap().mode() & 0o777, 0o700);
    // An existing private directory is accepted as it is.
    private_directory(&created).unwrap();

    let shared = root.join("shared");
    fs::create_dir_all(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
    let error = private_directory(&shared).unwrap_err();
    assert!(error.to_string().contains("reachable by other users"));

    // A symlink is rejected rather than followed.
    let planted = root.join("planted");
    std::os::unix::fs::symlink(&created, &planted).unwrap();
    assert!(private_directory(&planted).is_err());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn only_one_daemon_holds_the_state_directory() {
    let directory = scratch("lock");
    let persistence = test_persistence(&directory);
    let held = persistence.lock().unwrap();
    assert!(held.is_some());
    assert!(
        persistence.lock().unwrap().is_none(),
        "a second daemon backs off"
    );
    drop(held);
    assert!(
        persistence.lock().unwrap().is_some(),
        "the lock is freed on exit"
    );
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn idle_prompt_markers_restore_the_screen_before_the_prompt() {
    let mut parser = new_parser(6, 40);
    parser.process(
        b"output\r\n\x1b]777;mux-prompt-start\x1b\\header\r\n> \x1b[?2004h\x1b]777;mux-prompt-ready\x1b\\",
    );
    let correction = restored_prompt_correction(&parser).unwrap();
    parser.process(&correction);
    let contents = parser.screen().contents();
    assert!(contents.contains("output"));
    assert!(!contents.contains("header") && !contents.contains('>'));

    // Text typed at the prompt keeps it from being replaced.
    let mut parser = new_parser(4, 40);
    parser.process(
        b"\x1b]777;mux-prompt-start\x1b\\> \x1b[?2004h\x1b]777;mux-prompt-ready\x1b\\typed",
    );
    assert!(restored_prompt_correction(&parser).is_none());
}

#[test]
fn vim_snapshot_preserves_terminal_formatting() {
    let mut parser = vt100::Parser::new(2, 12, 0);
    parser.process(b"\x1b[1;3;4;31;44mstyled\x1b[0m plain");
    let line = snapshot_vim_line(parser.screen(), 0, 12);
    assert_eq!(line.text, "styled plain");

    let attributes = line.cells[0].attributes;
    assert_eq!(attributes.foreground, vt100::Color::Idx(1));
    assert_eq!(attributes.background, vt100::Color::Idx(4));
    assert!(attributes.bold && attributes.italic);
    assert_eq!(attributes.underline, vt100::UnderlineStyle::Straight);

    // Selection changes only the background.
    let selected = vim_selected_cell_attributes(attributes, &Theme::default());
    assert_eq!(selected.foreground, attributes.foreground);
    assert!(selected.bold && selected.italic);
    assert_eq!(selected.underline, attributes.underline);
    assert_eq!(selected.background, vt100::Color::Rgb(63, 53, 82));
}

fn vim_state(rows: u16, cols: u16, text: &[u8], cursor: Option<Position>) -> VimState {
    let mut parser = vt100::Parser::new(rows, cols, 0);
    parser.process(text);
    let (buffer, end) = snapshot_screen(parser.screen_mut());
    VimState {
        mode: VimMode::new(buffer, cursor.unwrap_or(end), rows.into()),
    }
}

#[test]
fn vim_render_is_confined_to_the_active_pane_region() {
    let state = vim_state(2, 12, b"first\r\nsecond", None);
    let render = |active| {
        repainted(8, 20, |frame| {
            Server::render_vim_region(
                &state,
                frame,
                rect(3, 5, 2, 8),
                4,
                active,
                &Theme::default(),
            )
        })
    };
    let rendered = render(true);
    assert!(
        rendered.contains("first") && rendered.contains("second"),
        "{rendered:?}"
    );
    // Row 4, column 10 is the region's first cell past the strip.
    assert!(rendered.contains("\x1b[4;10H"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[1;"), "{rendered:?}");
    assert!(rendered.contains("\x1b[?25h"), "{rendered:?}");

    let inactive = render(false);
    assert!(
        inactive.contains("first") && inactive.contains("second"),
        "{inactive:?}"
    );
    assert!(!inactive.contains("\x1b[?25h"), "{inactive:?}");
}

#[test]
fn vim_render_paints_full_multi_key_jump_hints() {
    let mut state = vim_state(1, 30, &[b'a'; 30], Some(Position { row: 0, col: 0 }));
    let bindings = Bindings::defaults();
    for name in [" ", "a"] {
        let key = crate::protocol::parse_for_test(name);
        state.mode.handle(bindings.get(Mode::Vim, &key), &key);
    }
    let rendered = repainted(1, 30, |frame| {
        Server::render_vim_region(&state, frame, rect(0, 0, 1, 30), 0, true, &Theme::default())
    });
    assert!(rendered.contains("ja"), "{rendered:?}");
}

fn sample_picker(selected: usize, in_use: Option<usize>) -> ThemePicker {
    let entry = |name: &str, theme| ThemeEntry {
        name: name.into(),
        theme,
    };
    let light = Theme {
        bar_label_foreground: (0xff, 0xff, 0xff),
        popup_text: (0x00, 0x00, 0x00),
        panel_background: (0xe8, 0xe8, 0xe8),
        popup_warning: (0x66, 0x44, 0x00),
        ..Theme::default()
    };
    let dusk = Theme {
        bar_active: (0xcb, 0xa3, 0xd2),
        ..Theme::default()
    };
    ThemePicker {
        entries: vec![
            entry("dark", Theme::default()),
            entry("dusk", dusk),
            entry("light", light),
        ],
        selected,
        in_use,
    }
}

fn picker_screen(picker: &ThemePicker, rows: u16, cols: u16) -> vt100::Parser {
    shown(
        rows,
        cols,
        &painted(rows, cols, |frame| {
            render_theme_picker(picker, frame, rows, cols)
        }),
    )
}

/// The characters of a painted picker, one string per row.
fn picker_rows(picker: &ThemePicker, rows: u16, cols: u16) -> Vec<String> {
    let parser = picker_screen(picker, rows, cols);
    let screen = parser.screen();
    (0..rows)
        .map(|row| {
            let text: String = (0..cols)
                .map(|col| match screen.cell(row, col) {
                    Some(cell) if cell.has_contents() => cell.contents().to_string(),
                    _ => " ".to_string(),
                })
                .collect();
            text.trim_end().to_string()
        })
        .collect()
}

#[test]
fn the_theme_picker_lists_every_theme_and_fits_any_terminal() {
    let rows = picker_rows(&sample_picker(1, Some(2)), 30, 110);
    let screen = rows.join("\n");
    // A centred card, numbered tabs with the one in use marked, and every
    // palette role named exactly as in palettes.nix.
    assert!(rows[0].is_empty(), "{screen}");
    assert!(
        screen.contains("╭") && screen.contains("themes"),
        "{screen}"
    );
    for text in ["1 dark", "2 dusk", "● 3 light", "←→ browse"] {
        assert!(screen.contains(text), "{text} missing from {screen}");
    }
    let roles = "background foreground surface surfaceRaised muted accent secondary success warning danger selection diffAdd diffDelete diffChange";
    for color in roles.split_whitespace() {
        assert!(screen.contains(color), "{color} missing from {screen}");
    }

    // Tiny terminals still get a frame that stays on screen.
    for (rows, cols) in [(1, 1), (2, 6), (5, 20), (10, 34), (16, 50), (24, 80)] {
        let painted = picker_rows(&sample_picker(1, Some(2)), rows, cols);
        assert_eq!(painted.len(), rows as usize);
        assert!(
            painted
                .iter()
                .all(|row| row.chars().count() <= cols as usize)
        );
    }
    let painted = picker_rows(&sample_picker(0, None), 24, 80);
    assert!(painted[0].is_empty() && painted[23].is_empty());

    let names = ["dark", "dusk", "light"];
    assert_eq!(theme_tab_rows(&names, 60), vec![vec![0, 1, 2]]);
    assert_eq!(theme_tab_rows(&names, 30), vec![vec![0, 1], vec![2]]);
    assert_eq!(theme_tab_rows(&names, 4), vec![vec![0], vec![1], vec![2]]);
}

#[test]
fn the_theme_picker_paints_itself_in_the_theme_it_is_offering() {
    // Light highlighted while dark is in use: showing the theme in use
    // would be wrong.
    let picker = sample_picker(2, Some(0));
    let (rows, cols) = (30, 110);
    let parser = picker_screen(&picker, rows, cols);
    let screen = parser.screen();
    let light = &picker.entries[2].theme;

    let card = theme_card(rows, cols, &["dark", "dusk", "light"], &light.palette);
    let border = screen.cell(card.top - 1, card.left).unwrap();
    assert_eq!(border.bgcolor(), rgb(light.panel_background));
    assert_eq!(border.fgcolor(), rgb(light.palette.surface_raised));

    // Each swatch is labelled in whichever shade can be read on it.
    let swatch = (0..rows)
        .flat_map(|row| (0..cols).map(move |col| (row, col)))
        .filter_map(|(row, col)| screen.cell(row, col))
        .find(|cell| cell.bgcolor() == rgb(light.palette.diff_add))
        .expect("a swatch of the diffAdd colour");
    let label = contrasting_shade(
        light.palette.diff_add,
        light.palette.foreground,
        light.palette.background,
    );
    assert_eq!(swatch.fgcolor(), rgb(label));

    let (page, ink) = ((0xff, 0xff, 0xff), (0x00, 0x00, 0x00));
    assert_eq!(contrasting_shade((0x11, 0x11, 0x11), page, ink), page);
    assert_eq!(contrasting_shade((0xf8, 0xf8, 0xf8), page, ink), ink);
}

#[test]
fn themes_are_read_from_their_directories_and_sorted() {
    let root = scratch("themes");
    let themes = root.join("themes");
    for (name, color) in [("dusk", "#cba3d2"), ("dark", "#6e5871")] {
        fs::create_dir_all(themes.join(name)).unwrap();
        fs::write(
            themes.join(name).join(THEME_FILE),
            format!("[palette]\nsecondary = \"{color}\"\n"),
        )
        .unwrap();
    }
    // Neither an empty nor an unparsable theme is offered, nor stops the rest.
    fs::create_dir_all(themes.join("empty")).unwrap();
    fs::create_dir_all(themes.join("broken")).unwrap();
    fs::write(themes.join("broken").join(THEME_FILE), "not toml at all {").unwrap();

    let entries = scan_themes(&themes).unwrap();
    let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["dark", "dusk"]);
    assert_eq!(entries[1].theme.palette.secondary, (0xcb, 0xa3, 0xd2));

    // The theme in use is whatever the `current` link beside them points at.
    assert_eq!(current_theme_name(&themes), None);
    std::os::unix::fs::symlink(themes.join("dusk"), root.join(THEME_CURRENT_LINK)).unwrap();
    assert_eq!(current_theme_name(&themes).as_deref(), Some("dusk"));
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn compacting_a_journal_keeps_the_screen_and_recent_scrollback() {
    let mut parser = vt100::Parser::new(4, 20, SCROLLBACK_LINES);
    for line in 0..200 {
        parser.process(format!("line {line:03}\r\n").as_bytes());
    }
    parser.process(b"\x1b[1mbold tail\x1b[0m");
    let before = parser.screen().contents();

    let records = compacted_journal_records(parser.screen_mut()).unwrap();
    assert!(records.len() < 8 * 1024, "{} bytes", records.len());

    let mut restored = vt100::Parser::new(1, 1, SCROLLBACK_LINES);
    replay_pane_journal(&mut restored, records.as_slice()).unwrap();
    assert_eq!(restored.screen().size(), (4, 20));
    assert_eq!(restored.screen().contents(), before);
    // Scrollback survives, and so does the formatting inside it.
    let (buffer, _) = snapshot_screen(restored.screen_mut());
    assert!(buffer.texts().any(|text| text.contains("line 000")));
    let tail = buffer.line(buffer.len() - 1);
    assert!(tail.cells.iter().any(|cell| cell.attributes.bold));
}

#[test]
fn a_failed_journal_replacement_keeps_saved_history() {
    let directory = scratch("failed-replacement");
    let persistence = test_persistence(&directory);
    let path = persistence.pane_history_path(0);
    let mut journal = PaneJournal::new(persistence.new_pane_history(0).unwrap(), 0);
    journal.append_output(b"saved history", None).unwrap();
    journal.flush().unwrap();
    let original = fs::read(&path).unwrap();
    // Force temporary-file creation to fail, even when tests run as root.
    fs::create_dir(path.with_extension("ansi.tmp")).unwrap();
    assert!(journal.replace(path.clone(), b"replacement").is_err());
    assert_eq!(fs::read(path).unwrap(), original);
    assert_eq!(journal.length, original.len() as u64);
    drop(journal);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_journal_compacts_when_overgrown_and_gives_up_when_unwritable() {
    let path = scratch("journal-failure").join("pane.ansi");
    fs::write(&path, b"").unwrap();
    // A read-only descriptor stands in for a disk that will not take writes.
    let mut journal = PaneJournal::new(File::open(&path).unwrap(), 0);
    assert!(!journal.needs_compaction());
    journal.length = MAX_JOURNAL_BYTES + 1;
    assert!(journal.needs_compaction());

    journal.append_output(b"buffered", None).unwrap();
    assert!(journal.flush().is_err(), "the first failure is reported");
    assert!(journal.abandoned);
    assert!(
        journal.flush().is_ok() && journal.append_output(b"more", None).is_ok(),
        "later writes stay quiet instead of repeating the failure"
    );
    assert!(
        !journal.needs_compaction(),
        "abandoned journals are not compacted"
    );
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn a_shimmer_pause_schedules_a_future_wake_without_a_new_render() {
    let mut test = TestServer::new("bell-deadline");
    test.ring_bell();
    let server = &mut test.server;
    let bell = server.sessions[0].windows[0].bell.as_mut().unwrap();
    bell.started = Instant::now() - Duration::from_micros(BELL_SHIMMER_MICROS as u64 + 100_000);
    server.advance_bell_animations();
    assert!(
        !server.advance_bell_animations(),
        "the pause has no new frame"
    );
    server.dirty = false;
    let last_render = Instant::now() - Duration::from_millis(100);
    for _ in 0..3 {
        let now = Instant::now();
        assert!(server.next_wake(last_render).unwrap() >= now + ANIMATION_INTERVAL);
    }
}

#[test]
fn pending_bells_do_not_defer_session_saves_until_shutdown() {
    let mut test = TestServer::new("bell-save");
    test.ring_bell();
    let state_file = test.directory.join("state.bin");
    let server = &mut test.server;
    server.persistence.save(&server.persisted_state()).unwrap();
    server.sessions[0].name = "after".into();
    server.save_state_soon();
    let events = server.events.clone();
    let receiver = std::mem::replace(&mut test.events, mpsc::sync_channel(1).1);
    let saved = thread::scope(|scope| {
        let worker = scope.spawn(|| test.server.event_loop(receiver).unwrap());
        // Observe the save before shutdown, whose forced flush would mask it.
        let deadline = Instant::now() + Duration::from_secs(2);
        let saved = loop {
            let state = decode_persisted_state(&fs::read(&state_file).unwrap()).unwrap();
            if state.sessions[0].name == "after" {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            thread::sleep(Duration::from_millis(10));
        };
        events
            .send(Event::Client(1, ClientMessage::Shutdown))
            .unwrap();
        worker.join().unwrap();
        saved
    });
    assert!(
        test.server.sessions[0].windows[0]
            .bell
            .as_ref()
            .unwrap()
            .repeat
    );
    assert!(
        saved,
        "session changes must reach disk while the bell is pending"
    );
}

#[test]
fn clipboard_copy_finishes_on_a_worker() {
    let directory = scratch("clipboard-copy");
    let path = directory.join("copied");
    let (sender, receiver) = mpsc::sync_channel(EVENT_QUEUE_DEPTH);
    let command = [
        "sh",
        "-c",
        "sleep 0.2; cat > \"$1\"",
        "sh",
        path.to_str().unwrap(),
    ];
    copy_to_clipboard(
        sender,
        7,
        command.map(String::from).to_vec(),
        "copied text".into(),
    );
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let event = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    let Event::ClipboardCopied(id, bytes, result) = event else {
        panic!("clipboard worker sent the wrong event");
    };
    assert_eq!((id, bytes, result), (7, 11, Ok(())));
    assert_eq!(fs::read_to_string(&path).unwrap(), "copied text");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_client_that_stops_reading_falls_behind_instead_of_blocking_the_daemon() {
    let (server_end, _client_end) = UnixStream::pair().unwrap();
    let writer = ClientWriter::spawn(server_end);
    // Once the socket and queue are full the send is refused, not blocked.
    let accepted = (0..64)
        .take_while(|_| writer.send(ServerMessage::Render(vec![b'x'; 1 << 20])))
        .count();
    assert!(accepted < 64, "the queue refused nothing");
}

#[test]
fn compressed_scrollback_is_lossless_and_drops_exactly_the_oldest_rows() {
    let mut parser = vt100::Parser::new(2, 40, 300);
    for line in 0..=600 {
        parser.process(
            format!("\x1b[38;2;1;2;3mline {line:03} A\u{301}界\x1b[48;5;42m \x1b[0m\r\n")
                .as_bytes(),
        );
    }
    let compressed_bytes = parser.screen().history_bytes();
    let (lines, _) = snapshot_screen(parser.screen_mut());
    let text: Vec<_> = lines.texts().collect();
    assert!(!text.iter().any(|line| line.starts_with("line 299")));
    assert!(text[0].starts_with("line 300"), "{:?}", text[0]);
    assert!(text.iter().any(|line| line.starts_with("line 600")));

    let first = lines.line(0);
    assert_eq!(first.cells[9].contents(&first.text), "A\u{301}");
    assert_eq!(first.cells[10].contents(&first.text), "界");
    assert!(first.cells[11].wide_continuation);
    assert_eq!(first.cells[12].attributes.background, vt100::Color::Idx(42));
    assert_eq!(
        first.cells[0].attributes.foreground,
        vt100::Color::Rgb(1, 2, 3)
    );
    // Snapshotting does not inflate the stored history.
    assert_eq!(parser.screen().history_bytes(), compressed_bytes);
}

#[test]
fn a_full_scrollback_of_ascii_rows_stays_small_through_reflow() {
    let mut parser = vt100::Parser::new(1, 80, SCROLLBACK_LINES);
    let mut output = Vec::with_capacity((SCROLLBACK_LINES + 1) * 82);
    for _ in 0..=SCROLLBACK_LINES {
        output.extend_from_slice(&[b'x'; 80]);
        output.extend_from_slice(b"\r\n");
    }
    parser.process(&output);
    for cols in [80, 40] {
        parser.screen_mut().set_scrollback(0);
        parser.screen_mut().set_size(1, cols);
        parser.screen_mut().set_scrollback(usize::MAX);
        let bytes = parser.screen().history_bytes();
        assert_eq!(parser.screen().scrollback(), SCROLLBACK_LINES);
        assert!(bytes < 64 * 1024, "{cols} columns: {bytes} bytes");
        assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "x");
        assert_eq!(parser.screen().cell(0, cols - 1).unwrap().contents(), "x");
    }
    let before_snapshot = parser.screen().history_bytes();
    let (lines, _) = snapshot_screen(parser.screen_mut());
    assert_eq!(lines.len(), SCROLLBACK_LINES + 1);
    assert!(parser.screen().history_bytes() <= before_snapshot);
}

#[test]
fn only_top_anchored_scroll_regions_feed_history() {
    // Codex scrolls a region pinned to the top; its lines belong in history.
    let mut parser = vt100::Parser::new(6, 20, 100);
    parser.process(b"\x1b[1;4r\x1b[4;1H");
    for line in 0..12 {
        parser.process(format!("\r\nhistory {line:02}").as_bytes());
    }
    parser.process(b"\x1b[r");
    parser.screen_mut().set_scrollback(usize::MAX);
    assert_eq!(parser.screen().scrollback(), 12);
    let (lines, cursor) = snapshot_screen(parser.screen_mut());
    assert!(lines.lines().any(|line| line.text.contains("history 00")));
    assert!(cursor.row < lines.len());

    let mut parser = vt100::Parser::new(6, 20, 100);
    parser.process(b"\x1b[2;4r\x1b[4;1H");
    for line in 0..12 {
        parser.process(format!("\r\nregion {line:02}").as_bytes());
    }
    parser.screen_mut().set_scrollback(usize::MAX);
    assert_eq!(parser.screen().scrollback(), 0);
}

#[test]
fn snapshotting_scrollback_keeps_styled_blanks_past_the_last_character() {
    let mut parser = vt100::Parser::new(2, 8, 100);
    parser.process(b"hi\x1b[41m   \x1b[0m\r\nsecond\r\nthird\r\n");
    let (lines, _) = snapshot_screen(parser.screen_mut());
    let first = lines.line(0);
    assert_eq!(first.text, "hi   ");
    assert_eq!(first.cells.len(), 5);
    assert_eq!(first.cells[4].attributes.background, vt100::Color::Idx(1));
}

#[test]
fn quiet_panes_refresh_icons_without_output_and_reject_stale_samples() {
    let mut test = TestServer::new("process-refresh");
    test.server.clients.get_mut(&1).unwrap().initialized = true;
    let server = &mut test.server;
    server.process_sampler = process_icon_sampler(server.events.clone());
    let pane = &mut server.sessions[0].windows[0].panes[0];
    pane.process_pending = false;
    pane.process_icon = program_icon("codex");
    pane.process_sampled = Instant::now() - PROCESS_POLL_INTERVAL;
    let pane_id = pane.id;
    server.dirty = false;
    assert!(server.next_wake(Instant::now()).unwrap() <= Instant::now());

    // Only the timer and the process worker may correct the stale icon.
    let deadline = Instant::now() + Duration::from_secs(3);
    while server.pane_mut(pane_id).unwrap().process_icon == program_icon("codex") {
        assert!(Instant::now() < deadline, "quiet pane kept its old icon");
        server.sample_process_icons();
        if let Ok(event @ Event::ProcessIcon(..)) =
            test.events.recv_timeout(Duration::from_millis(10))
        {
            server.handle_event(event).unwrap();
        }
    }
    assert!(server.dirty);
    let pane = server.pane_mut(pane_id).unwrap();
    let (group, icon) = (pane.master.process_group_leader(), pane.process_icon);
    assert!(group.is_some());
    server.dirty = false;
    server
        .handle_event(Event::ProcessIcon(pane_id, group, icon))
        .unwrap();
    assert!(!server.dirty, "unchanged samples should not repaint");
    let stale = Event::ProcessIcon(pane_id, Some(i32::MAX), program_icon("codex"));
    server.handle_event(stale).unwrap();
    assert_eq!(server.pane_mut(pane_id).unwrap().process_icon, icon);

    // One outstanding request per pane keeps a slow sampler from queueing.
    let (samples, requests) = mpsc::channel();
    server.process_sampler = samples;
    server.sample_process_icons();
    assert_eq!(requests.try_recv().unwrap().len(), 1);
    server.pane_mut(pane_id).unwrap().process_sampled = Instant::now() - PROCESS_POLL_INTERVAL;
    server.sample_process_icons();
    assert!(requests.try_recv().is_err());
    assert_eq!(server.next_wake(Instant::now()), None);
}

#[test]
fn detached_sessions_do_not_poll_process_icons() {
    let mut test = TestServer::new("detached-idle");
    let server = &mut test.server;
    server.clients.clear();
    server.sessions[0].windows[0].panes[0].process_pending = false;
    let (samples, requests) = mpsc::channel();
    server.process_sampler = samples;
    server.dirty = false;
    server.sample_process_icons();
    assert!(requests.try_recv().is_err());
    assert_eq!(server.next_wake(Instant::now()), None);
}

#[test]
fn a_rejected_final_frame_stays_pending_until_the_client_catches_up() {
    let mut test = TestServer::new("render-retry");
    test.attach();
    let (messages, received) = mpsc::sync_channel(1);
    // The queue starts full, so the next frame is refused.
    messages.send(ServerMessage::Render(Vec::new())).unwrap();
    test.server.clients.get_mut(&1).unwrap().writer = ClientWriter {
        messages,
        thread: thread::spawn(|| {}),
        stream: None,
    };
    let server = &mut test.server;
    server.dirty = false;
    server.render_all();
    assert!(
        server.dirty,
        "a dropped final frame must schedule another repaint"
    );
    assert_eq!(server.clients[&1].frame.rows(), 0);
    received.recv().unwrap();
    server.dirty = false;
    server.render_all();
    assert!(!server.dirty);
    let ServerMessage::Render(bytes) = received.recv().unwrap() else {
        panic!("expected a complete replacement frame");
    };
    assert!(bytes.windows(4).any(|bytes| bytes == b"\x1b[2J"));
}

#[test]
fn two_digit_sidebar_process_tiles_keep_their_background_through_the_right_edge() {
    let mut test = TestServer::new("wide-bar");
    test.attach();
    for _ in 1..10 {
        test.server.new_window(1).unwrap();
    }
    let theme = test.server.clients[&1].rendered_theme();
    let mut terminal = vt100::Parser::new(24, 80, 0);
    render_frames(&mut test.server, 1, &mut test.client, &mut terminal);
    // Ten windows, the last selected, seven tiles visible: the first is
    // window 4 at row 2 and the last is window 10 at row 20 (one-based).
    let screen = terminal.screen();
    assert_eq!(
        screen.cell(2, 3).unwrap().bgcolor(),
        rgb(theme.bar_inactive)
    );
    assert_eq!(screen.cell(20, 3).unwrap().bgcolor(), rgb(theme.bar_active));
}
