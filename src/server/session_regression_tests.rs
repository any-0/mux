//! PTY histories adapted from tmux regress/input-{scroll,edit,malformed,sgr}.sh
//! and screen-redraw-status.sh. Assertions describe user-visible results, not
//! the algorithms that compute them. See docs/regression-coverage.md.

use std::{
    env, fs, thread,
    time::{Duration, Instant},
};

use super::*;
use crate::{
    frame::ColorDepth,
    protocol::{
        ClientMessage, Hello, Mouse, MouseButton, MouseKind, MuxCommand, ServerMessage,
        read_message,
    },
};

struct Session {
    server: Server,
    events: Receiver<Event>,
    client: UnixStream,
    terminal: vt100::Parser,
    directory: PathBuf,
    output_number: usize,
}

impl Session {
    fn new() -> Self {
        let directory = env::temp_dir().join(format!(
            "mux-session-{}-{:?}",
            std::process::id(),
            thread::current().id()
        ));
        let (mut server, events, client) = tests::server_with_pending_bell(&directory);
        let attached = server.clients.get_mut(&1).unwrap();
        attached.initialized = true;
        attached.session_id = Some(0);
        attached.rows = 12;
        attached.cols = 40;
        attached.colors = ColorDepth::TrueColor;
        server.sessions[0].windows[0].bell = None;
        server.resize_active(1).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let mut session = Self {
            server,
            events,
            client,
            terminal: vt100::Parser::new(12, 40, 0),
            directory,
            output_number: 0,
        };
        // Keep readline redraws out of byte-stream fixtures; daemon lifecycle
        // tests also use /bin/sh, while input still travels over a real PTY.
        session.pane().writer.send(b"exec /bin/sh\r").unwrap();
        session.output(b"\x1bcREADY", "READY");
        session
    }

    fn pane(&self) -> &Pane {
        let (s, w) = self.server.active_indices(1).unwrap();
        let window = &self.server.sessions[s].windows[w];
        window
            .panes
            .iter()
            .find(|pane| pane.id == window.active_pane)
            .unwrap()
    }

    /// Generated terminal bytes travel through an actual shell and PTY reader.
    /// A fixture file avoids canonical input's line-length limit while the
    /// command that prints it still enters through the pane's real PTY.
    fn output(&mut self, bytes: &[u8], marker: &str) {
        let path = self
            .directory
            .join(format!("output-{}", self.output_number));
        self.output_number += 1;
        fs::write(&path, bytes).unwrap();
        let command = format!("stty -echo; cat '{}'\r", path.display());
        self.pane().writer.send(command.as_bytes()).unwrap();
        self.wait_output(marker);
    }

    fn wait_output(&mut self, marker: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.pane().parser.screen().contents().contains(marker) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "PTY did not produce {marker:?}");
            let event = self.events.recv_timeout(remaining).unwrap();
            self.server.handle_event(event).unwrap();
        }
    }

    fn command(&mut self, command: MuxCommand) {
        self.server
            .handle_event(Event::Client(
                1,
                ClientMessage::Command {
                    pane_id: None,
                    command,
                },
            ))
            .unwrap();
    }

    /// Decode exactly the bytes a client receives, retaining its previous
    /// terminal contents so missed incremental clears are observable.
    fn capture(&mut self) -> &vt100::Screen {
        self.server.render_all();
        read_render(&mut self.client, &mut self.terminal);
        self.terminal.screen()
    }

    fn bar(&mut self, expected: &[(u16, &str)]) {
        let screen = self.capture();
        for &(row, text) in expected {
            let actual = (0..3)
                .map(|col| screen.cell(row - 1, col).unwrap().contents().to_owned())
                .collect::<String>();
            assert_eq!(actual, text, "sidebar row {row}");
        }
        // Terminal SGR must not leak to the bar, even on incremental repaints.
        for row in 0..12 {
            for col in 0..3 {
                let cell = screen.cell(row, col).unwrap();
                assert_eq!(cell.underline_style(), vt100::UnderlineStyle::None);
                assert_eq!(cell.underline_color(), vt100::Color::Default);
            }
        }
    }
}

struct Peer {
    client: UnixStream,
    terminal: vt100::Parser,
}

impl Peer {
    fn attach(session: &mut Session, rows: u16, cols: u16) -> Self {
        let (writer, client) = UnixStream::pair().unwrap();
        session
            .server
            .handle_event(Event::Connected(2, writer))
            .unwrap();
        let settings = crate::config::Settings::default();
        session
            .server
            .handle_event(Event::Client(
                2,
                ClientMessage::Hello(Box::new(Hello {
                    rows,
                    cols,
                    cwd: session.directory.clone(),
                    session: Some("before".into()),
                    bindings: settings.bindings,
                    clipboard_command: settings.clipboard_command,
                    terminal_clipboard: false,
                    theme: settings.theme,
                    theme_command: settings.theme_command,
                    theme_directory: settings.theme_directory,
                    mouse: true,
                    bell_style: settings.bell_style,
                    truecolor: true,
                    default_cursor_shape: settings.default_cursor_shape,
                })),
            ))
            .unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        Self {
            client,
            terminal: vt100::Parser::new(rows, cols, 0),
        }
    }

    fn capture(&mut self, server: &mut Server) -> &vt100::Screen {
        server.render_all();
        read_render(&mut self.client, &mut self.terminal);
        self.terminal.screen()
    }
}

fn read_render(client: &mut UnixStream, terminal: &mut vt100::Parser) {
    loop {
        match read_message::<ServerMessage>(client) {
            Ok(Some(ServerMessage::Render(bytes))) => terminal.process(&bytes),
            Ok(Some(ServerMessage::Error(error))) => panic!("render failed: {error}"),
            Ok(Some(_)) => {}
            Ok(None) => panic!("client disconnected"),
            Err(error)
                if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    )
                }) =>
            {
                break;
            }
            Err(error) => panic!("read client frame: {error:#}"),
        }
    }
}

impl Drop for Session {
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
fn pty_scroll_regions_edits_and_malformed_strings_keep_the_sidebar_intact() {
    let mut session = Session::new();
    session.output(b"\x1bc\x1b[1;1H11111\x1b[2;1H22222\x1b[3;1H33333\x1b[4;1H44444\x1b[2;3r\x1b[3;1HAAAAA\r\nBBBBB\x1b[r\x1b[10;1HREGION", "REGION");
    let screen = session.pane().parser.screen();
    assert_eq!(
        screen.rows(0, 5).take(4).collect::<Vec<_>>(),
        ["11111", "AAAAA", "BBBBB", "44444"]
    );
    session.bar(&[(5, " • ")]);

    session.output(
        b"\x1b[1;1Habcdef\x1b[3G\x1b[2@XY\x1b[3G\x1b[2P\x1b[2X\x1b[10;1HEDITED",
        "EDITED",
    );
    assert_eq!(
        session.pane().parser.screen().rows(0, 8).next().unwrap(),
        "ab  ef"
    );
    session.bar(&[(5, " • ")]);

    let mut payload = b"\x1b]2;".to_vec();
    payload.extend(vec![b'x'; vt100::MAX_OSC_BYTES + 100]);
    payload.extend_from_slice(
        b"\x07\x1b[?9999z\x1b]999;bad\x07\x1b[1;1H\x1b[2KRECOVERED\x1b[10;1HMALFORMED",
    );
    session.output(&payload, "MALFORMED");
    assert_eq!(
        session.pane().parser.screen().rows(0, 9).next().unwrap(),
        "RECOVERED"
    );
    session.bar(&[(5, " • ")]);
}

#[test]
fn pty_undercurl_clear_alt_screen_zoom_and_window_switch_restore_sidebar() {
    let mut session = Session::new();
    session.output(
        b"\x1bc\x1b[4:3;58:2::255:0:0mWAVE\x1b[24;59mPLAIN\x1b[10;1HSTYLED",
        "STYLED",
    );
    let bar = session.server.active_bar_width(1);
    let screen = session.capture();
    assert_eq!(
        screen.cell(0, bar).unwrap().underline_style(),
        vt100::UnderlineStyle::Curly
    );
    assert_eq!(
        screen.cell(0, bar).unwrap().underline_color(),
        vt100::Color::Rgb(255, 0, 0)
    );
    assert_eq!(
        screen.cell(0, bar + 4).unwrap().underline_style(),
        vt100::UnderlineStyle::None
    );
    assert_eq!(
        screen.cell(0, bar + 4).unwrap().underline_color(),
        vt100::Color::Default
    );
    session.bar(&[(5, " • ")]);
    session.output(b"\x1b[?1049h\x1b[2J\x1b[HALTERNATE", "ALTERNATE");
    session.command(MuxCommand::ZoomPane);
    assert!(session.capture().contents().contains("ALTERNATE"));
    session.command(MuxCommand::ZoomPane);
    session.bar(&[(5, " • ")]);
    session.output(b"\x1b[?1049l\x1b[10;1HRETURNED", "RETURNED");
    let pane_contents = session.pane().parser.screen().contents();
    assert!(
        session.capture().contents().contains("WAVEPLAIN"),
        "main screen missing after interaction: {pane_contents:?}"
    );
    session.command(MuxCommand::NewWindow);
    session.output(b"\x1bcSECOND", "SECOND");
    session.bar(&[(4, " 1 "), (7, " • ")]);
    session.command(MuxCommand::SelectWindow(1));
    session.bar(&[(4, " • "), (7, " 2 ")]);
    let pane_contents = session.pane().parser.screen().contents();
    assert!(
        session.capture().contents().contains("WAVEPLAIN"),
        "main screen missing after interaction: {pane_contents:?}"
    );
}

#[test]
fn every_torn_journal_boundary_replays_only_complete_records() {
    let mut records = encode_journal_record(JOURNAL_RESIZE, &[0, 3, 0, 8]).unwrap();
    let first = records.len();
    records.extend(encode_journal_record(JOURNAL_OUTPUT, b"FIRST").unwrap());
    let second = records.len();
    records.extend(encode_journal_record(JOURNAL_OUTPUT, b"\r\nSECOND").unwrap());
    for cut in 0..=records.len() {
        let mut parser = new_parser(7, 17);
        let consumed =
            replay_pane_journal(&mut parser, &mut Vec::new(), &records[..cut]).unwrap() as usize;
        let expected = if cut == records.len() {
            records.len()
        } else if cut >= second {
            second
        } else if cut >= first {
            first
        } else {
            0
        };
        assert_eq!(consumed, expected, "cut {cut}");
        let size = if cut >= first { (3, 8) } else { (7, 17) };
        assert_eq!(parser.screen().size(), size, "resize cut {cut}");
        assert_eq!(
            parser.screen().contents(),
            if cut == records.len() {
                "FIRST\nSECOND"
            } else if cut >= second {
                "FIRST"
            } else {
                ""
            },
            "cut {cut}"
        );
    }
}

fn packed_row(row: &[u8]) -> Vec<u8> {
    let mut packed = 1u32.to_le_bytes().to_vec();
    packed.extend((row.len() as u64).to_le_bytes());
    packed.push(0);
    packed.extend(row);
    packed
}

#[test]
fn malformed_persisted_history_is_rejected_before_render_or_reflow() {
    // One two-column compact row holding "A" with a red attribute span.
    let mut row = vec![2, 0, 0, 2, 0, 0, 0, 1, 0, 3, 0, 0, 0, 1, 0, 1, 1, b'A'];
    row.extend([0, 0, 1, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut parser = new_parser(3, 8);
    assert!(parser.screen_mut().restore_history(&packed_row(&row)));
    for cut in 0..row.len() {
        assert!(
            !parser
                .screen_mut()
                .restore_history(&packed_row(&row[..cut])),
            "truncation {cut}"
        );
        assert_eq!(
            parser.screen().history_rows(),
            1,
            "failed restore is atomic"
        );
    }
    for (offset, byte) in [
        (0, 0),
        (2, 2),
        (3, 4),
        (7, 3),
        (9, 255),
        (15, 0),
        (16, 31),
        (17, 255),
        (18, 2),
        (22, 3),
        // Bold+faint (3) is legal; underline styles 6/7 are not.
        (34, 6 << 5),
        (34, 224),
    ] {
        let mut invalid = row.clone();
        invalid[offset] = byte;
        assert!(
            !parser.screen_mut().restore_history(&packed_row(&invalid)),
            "mutation at {offset}"
        );
    }
    let mut huge = packed_row(&row);
    huge[..4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(!parser.screen_mut().restore_history(&huge));
    huge[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(!parser.screen_mut().restore_history(&huge));
    let bad_journal = encode_journal_record(JOURNAL_HISTORY, &packed_row(&[0])).unwrap();
    assert!(replay_pane_journal(&mut parser, &mut Vec::new(), bad_journal.as_slice()).is_err());
    parser.screen_mut().set_size(3, 2);
    parser.screen_mut().set_scrollback(1);
    assert!(parser.screen().contents().contains('A'));
    assert_eq!(
        parser.screen().cell(0, 0).unwrap().fgcolor(),
        vt100::Color::Idx(1)
    );

    let mut source = vt100::Parser::new(2, 8, 10);
    source.process(b"A\r\nB\r\nC\r\nD");
    let packed = source.screen().encode_history();
    let mut bounded = vt100::Parser::new(2, 8, 1);
    assert!(bounded.screen_mut().restore_history(&packed));
    assert_eq!(bounded.screen().history_rows(), 1);
    bounded.screen_mut().set_scrollback(1);
    assert!(bounded.screen().contents().starts_with('B'));
    let mut empty = vt100::Parser::new(2, 8, 0);
    assert!(empty.screen_mut().restore_history(&packed));
    assert_eq!(empty.screen().history_rows(), 0);
}

#[test]
fn resizing_through_a_wide_cell_then_erasing_does_not_panic() {
    let mut parser = new_parser(3, 8);
    parser.process("界tail".as_bytes());
    parser.screen_mut().set_size(3, 1);
    // Previously resize retained a wide leading half at the right edge;
    // erase tried to clear its now-missing continuation and indexed past it.
    parser.process(b"\x1b[H\x1b[XOK");
    assert!(!parser.screen().cell(0, 0).unwrap().is_wide());
    assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "O");
    assert_eq!(parser.screen().cell(1, 0).unwrap().contents(), "K");
    parser.screen_mut().set_size(3, 8);
    parser.process(b"\x1b[H\x1b[2KALIVE");
    assert!(parser.screen().contents().starts_with("ALIVE"));
}

#[test]
fn generated_scrollback_keeps_text_and_styles_through_resize_and_compaction() {
    let mut parser = new_parser(3, 12);
    for index in 0..40 {
        parser.process(
            format!("\x1b[4:3;58;5;45m{index:02}:界e\u{301}\x1b[24;59m:end\r\n").as_bytes(),
        );
    }
    let original: Vec<_> = parser.screen().all_rows().cloned().collect();
    let text = |rows: &[vt100::Row]| {
        rows.iter()
            .flat_map(|row| row.cells())
            .filter(|cell| !cell.is_wide_continuation())
            .map(|cell| cell.contents().to_owned())
            .collect::<String>()
    };
    let expected = text(&original);
    for cols in [7, 3, 2, 1, 2, 4, 12] {
        parser.screen_mut().set_size(3, cols);
        let records = compacted_journal_records(parser.screen_mut()).unwrap();
        let mut restored = new_parser(3, cols);
        replay_pane_journal(&mut restored, &mut Vec::new(), records.as_slice()).unwrap();
        let actual: Vec<_> = restored.screen().all_rows().cloned().collect();
        assert_eq!(
            text(&actual),
            text(&parser.screen().all_rows().cloned().collect::<Vec<_>>()),
            "compacted width {cols}"
        );
        // All earlier numbered lines are in history; visible rows are allowed
        // to be clipped when a client shrinks, as mux documents.
        assert!(text(&actual).contains("00:界e\u{301}:end"));
        let styled = actual
            .iter()
            .flat_map(|row| row.cells())
            .find(|cell| cell.contents() == "界")
            .unwrap();
        assert_eq!(styled.underline_style(), vt100::UnderlineStyle::Curly);
        assert_eq!(styled.underline_color(), vt100::Color::Idx(45));
    }
    assert!(expected.contains("00:界e\u{301}:end"));
}

#[test]
fn background_pty_bell_cannot_overwrite_the_mode_tile_after_narrow_resize() {
    let mut session = Session::new();
    session.server.clients.get_mut(&1).unwrap().bell_style = BellStyle::Steady;
    session.command(MuxCommand::NewSession(Some("other".into())));
    session.output(b"\x1bcOTHER\x07", "OTHER");
    session.command(MuxCommand::ChooseTree);
    session
        .server
        .handle_key(1, crate::protocol::parse_for_test("Up"))
        .unwrap();
    session
        .server
        .handle_key(1, crate::protocol::parse_for_test("Enter"))
        .unwrap();
    assert_eq!(session.server.clients[&1].session_id, Some(0));
    session
        .server
        .handle_event(Event::Client(
            1,
            ClientMessage::Resize { cols: 40, rows: 1 },
        ))
        .unwrap();
    session.terminal.screen_mut().set_size(1, 40);
    // The tile remains the normal-mode dot. The pending-bell badge belongs
    // on a separate bottom row, and must not overwrite it when none exists.
    assert_eq!(session.capture().cell(0, 1).unwrap().contents(), "●");
}

#[test]
fn tree_keeps_the_selected_session_when_an_earlier_background_shell_exits() {
    tree_selection_survives_exit(false);
}

#[test]
fn tree_keeps_the_selected_pane_when_an_earlier_background_shell_exits() {
    tree_selection_survives_exit(true);
}

fn tree_selection_survives_exit(expanded: bool) {
    let mut session = Session::new();
    session.command(MuxCommand::NewSession(Some("other".into())));
    session.output(b"\x1bcOTHER", "OTHER");
    session.command(MuxCommand::NewSession(Some("third".into())));
    session.output(b"\x1bcTHIRD", "THIRD");
    session.command(MuxCommand::ChooseTree);
    session
        .server
        .handle_key(1, crate::protocol::parse_for_test("Up"))
        .unwrap();
    if expanded {
        for key in ["Right", "Down", "Down"] {
            session
                .server
                .handle_key(1, crate::protocol::parse_for_test(key))
                .unwrap();
        }
    }
    assert!(session.capture().contents().contains("OTHER"));

    session.server.sessions[0].windows[0].panes[0]
        .writer
        .send(b"exit\r")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while session.server.sessions.len() != 2 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "background shell did not exit");
        let event = session.events.recv_timeout(remaining).unwrap();
        session.server.handle_event(event).unwrap();
    }
    let selected_background = crate::frame::rgb(session.server.clients[&1].theme.panel_selected);
    let screen = session.capture();
    assert!(
        screen.contents().contains("OTHER"),
        "preview changed after unrelated exit"
    );
    let row = if expanded { 4 } else { 1 };
    assert_eq!(screen.cell(row, 0).unwrap().bgcolor(), selected_background);
}

#[test]
fn mouse_clicks_follow_the_visible_sidebar_after_narrow_resize() {
    let mut session = Session::new();
    session.server.clients.get_mut(&1).unwrap().mouse = true;
    session.output(b"\x1bcFIRST", "FIRST");
    session.command(MuxCommand::NewWindow);
    session.output(b"\x1bcSECOND", "SECOND");
    session.command(MuxCommand::NewWindow);
    session.output(b"\x1bcTHIRD", "THIRD");
    session
        .server
        .handle_event(Event::Client(
            1,
            ClientMessage::Resize { rows: 7, cols: 40 },
        ))
        .unwrap();
    session.terminal.screen_mut().set_size(7, 40);
    assert_eq!(session.capture().cell(2, 1).unwrap().contents(), "•");
    session
        .server
        .handle_event(Event::Client(
            1,
            ClientMessage::Mouse(Mouse {
                kind: MouseKind::Down,
                button: MouseButton::Left,
                row: 2,
                col: 1,
                modifiers: 0,
            }),
        ))
        .unwrap();
    assert!(
        session.capture().contents().contains("THIRD"),
        "clicking the displayed current window must keep its PTY"
    );
    // The mode tile owns its row and must not select an offscreen window.
    session
        .server
        .handle_event(Event::Client(
            1,
            ClientMessage::Mouse(Mouse {
                kind: MouseKind::Down,
                button: MouseButton::Left,
                row: 0,
                col: 1,
                modifiers: 0,
            }),
        ))
        .unwrap();
    assert!(session.capture().contents().contains("THIRD"));
}

#[test]
fn two_clients_observe_shared_windows_but_keep_independent_history_viewports() {
    let mut session = Session::new();
    let mut peer = Peer::attach(&mut session, 8, 20);
    session.server.clients.get_mut(&1).unwrap().mouse = true;
    let mut lines = b"\x1bc".to_vec();
    for line in 0..30 {
        lines.extend_from_slice(format!("LINE-{line:02}\r\n").as_bytes());
    }
    lines.extend_from_slice(b"LIVE-END");
    session.output(&lines, "LIVE-END");
    assert!(session.capture().contents().contains("LIVE-END"));
    assert!(
        peer.capture(&mut session.server)
            .contents()
            .contains("LIVE-END")
    );
    // Confirm the actual PTY geometry, rather than just the parser's geometry.
    let size = session.directory.join("pty-size");
    let marker = session.directory.join("size-ready");
    fs::write(&marker, b"SIZE-READY").unwrap();
    session
        .pane()
        .writer
        .send(
            format!(
                "stty size > '{}'; cat '{}'\r",
                size.display(),
                marker.display()
            )
            .as_bytes(),
        )
        .unwrap();
    session.wait_output("SIZE-READY");
    assert_eq!(fs::read_to_string(&size).unwrap().trim(), "8 15");
    session
        .server
        .handle_event(Event::Client(
            1,
            ClientMessage::Mouse(Mouse {
                kind: MouseKind::ScrollUp,
                button: MouseButton::Left,
                row: 3,
                col: 8,
                modifiers: 0,
            }),
        ))
        .unwrap();
    assert!(!session.capture().contents().contains("LIVE-END"));
    assert!(
        peer.capture(&mut session.server)
            .contents()
            .contains("LIVE-END")
    );
    session
        .server
        .handle_event(Event::Client(
            2,
            ClientMessage::Resize { rows: 6, cols: 18 },
        ))
        .unwrap();
    peer.terminal.screen_mut().set_size(6, 18);
    // Shrinking clips bottom screen rows. Emit fresh live output afterward.
    session.output(b"\x1b[H\x1b[2KLIVE-END", "LIVE-END");
    // A peer's resize must not force this client's private history view live.
    assert!(!session.capture().contents().contains("LIVE-END"));
    for _ in 0..20 {
        session
            .server
            .handle_event(Event::Client(
                1,
                ClientMessage::Mouse(Mouse {
                    kind: MouseKind::ScrollDown,
                    button: MouseButton::Left,
                    row: 3,
                    col: 8,
                    modifiers: 0,
                }),
            ))
            .unwrap();
    }
    assert!(session.capture().contents().contains("LIVE-END"));
    session.command(MuxCommand::NewWindow);
    session.output(b"\x1bcSHARED-SECOND", "SHARED-SECOND");
    assert!(session.capture().contents().contains("SHARED-SECOND"));
    assert!(
        peer.capture(&mut session.server)
            .contents()
            .contains("SHARED-SECOND")
    );
    session
        .server
        .handle_event(Event::Client(
            2,
            ClientMessage::Command {
                pane_id: None,
                command: MuxCommand::SelectWindow(1),
            },
        ))
        .unwrap();
    assert!(session.capture().contents().contains("LIVE-END"));
    assert!(
        peer.capture(&mut session.server)
            .contents()
            .contains("LIVE-END")
    );
    assert_eq!(session.capture().cell(3, 1).unwrap().contents(), "•");
    assert_eq!(
        peer.capture(&mut session.server)
            .cell(1, 1)
            .unwrap()
            .contents(),
        "•"
    );
}

#[test]
fn real_pty_mouse_reports_are_local_to_the_clicked_split_and_popups_consume_clicks() {
    let mut session = Session::new();
    session.server.clients.get_mut(&1).unwrap().mouse = true;
    session.output(b"\x1bcLEFT-PANE", "LEFT-PANE");
    session.command(MuxCommand::SplitVertical);
    session.pane().writer.send(b"exec /bin/sh\r").unwrap();
    session.output(b"\x1bcRIGHT-PANE", "RIGHT-PANE");
    // At 40 columns: a 5-column bar, 17 left cells, one divider, 17 right cells.
    assert!(session.capture().contents().contains("LEFT-PANE"));
    assert!(session.capture().contents().contains("RIGHT-PANE"));
    let setup = session.directory.join("mouse-setup");
    let done = session.directory.join("mouse-done");
    let captured = session.directory.join("mouse-input");
    fs::write(&setup, b"\x1b[?1002h\x1b[?1006h\r\nMOUSE-READY").unwrap();
    fs::write(&done, b"\x1b[?1002l\x1b[?1006l\r\nMOUSE-DONE").unwrap();
    let expected = b"\x1b[<0;2;4M\x1b[<32;3;5M\x1b[<0;3;5m\x1b[<64;2;4M";
    session.pane().writer.send(format!("stty raw -echo; cat '{}'; dd bs=1 count={} of='{}' 2>/dev/null; stty -raw -echo; cat '{}'\r", setup.display(), expected.len(), captured.display(), done.display()).as_bytes()).unwrap();
    session.wait_output("MOUSE-READY");
    let click = Mouse {
        kind: MouseKind::Down,
        button: MouseButton::Left,
        row: 3,
        col: 24,
        modifiers: 0,
    };
    session.command(MuxCommand::ChooseTree);
    session
        .server
        .handle_event(Event::Client(1, ClientMessage::Mouse(click)))
        .unwrap();
    session
        .server
        .handle_key(1, crate::protocol::parse_for_test("Escape"))
        .unwrap();
    for event in [
        click,
        Mouse {
            kind: MouseKind::Drag,
            row: 4,
            col: 25,
            ..click
        },
        Mouse {
            kind: MouseKind::Up,
            row: 4,
            col: 25,
            ..click
        },
        Mouse {
            kind: MouseKind::ScrollUp,
            ..click
        },
    ] {
        session
            .server
            .handle_event(Event::Client(1, ClientMessage::Mouse(event)))
            .unwrap();
    }
    session.wait_output("MOUSE-DONE");
    assert_eq!(fs::read(&captured).unwrap(), expected);
    assert!(session.capture().contents().contains("LEFT-PANE"));
    // With reporting disabled, a click selects the other pane; it doesn't
    // send an escape sequence to the shell or alter the right pane's bytes.
    session
        .server
        .handle_event(Event::Client(
            1,
            ClientMessage::Mouse(Mouse {
                col: 5,
                row: 1,
                ..click
            }),
        ))
        .unwrap();
    assert_eq!(
        session.pane().parser.screen().rows(0, 9).next().unwrap(),
        "LEFT-PANE"
    );
    assert!(session.capture().contents().contains("MOUSE-DONE"));
}

#[test]
fn supported_style_transitions_and_active_attributes_survive_journal_compaction() {
    let mut parser = new_parser(4, 24);
    parser.process(b"\x1b[1;3;7;38;2;12;34;56;48;2;65;43;21;58;2;9;8;7m\x1b[4:2mD\x1b[4:4mO\x1b[4:5mS\x1b[2;23;27mI\x1b[0mP\x1b[3;4:4;58;2;1;2;3m");
    let records = compacted_journal_records(parser.screen_mut()).unwrap();
    let mut restored = new_parser(4, 24);
    replay_pane_journal(&mut restored, &mut Vec::new(), records.as_slice()).unwrap();
    let screen = restored.screen();
    for (col, style) in [
        (0, vt100::UnderlineStyle::Double),
        (1, vt100::UnderlineStyle::Dotted),
        (2, vt100::UnderlineStyle::Dashed),
    ] {
        let cell = screen.cell(0, col).unwrap();
        assert!(cell.bold() && cell.italic() && cell.inverse());
        assert_eq!(cell.fgcolor(), vt100::Color::Rgb(12, 34, 56));
        assert_eq!(cell.bgcolor(), vt100::Color::Rgb(65, 43, 21));
        assert_eq!(cell.underline_color(), vt100::Color::Rgb(9, 8, 7));
        assert_eq!(cell.underline_style(), style);
    }
    let dim = screen.cell(0, 3).unwrap();
    // SGR 2 adds faint without clearing bold; only SGR 22 clears both.
    assert!(dim.dim() && dim.bold() && !dim.italic() && !dim.inverse());
    let plain = screen.cell(0, 4).unwrap();
    assert_eq!(plain.underline_style(), vt100::UnderlineStyle::None);
    assert_eq!(plain.fgcolor(), vt100::Color::Default);
    restored.process(b"N\x1b[0mZ");
    let next = restored.screen().cell(0, 5).unwrap();
    assert_eq!(next.contents(), "N");
    assert!(next.italic());
    assert_eq!(next.underline_style(), vt100::UnderlineStyle::Dotted);
    assert_eq!(next.underline_color(), vt100::Color::Rgb(1, 2, 3));
    assert_eq!(restored.screen().cell(0, 6).unwrap().contents(), "Z");
    assert_eq!(
        restored.screen().cell(0, 6).unwrap().underline_style(),
        vt100::UnderlineStyle::None
    );
}
