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
    protocol::{ClientMessage, MuxCommand, ServerMessage, read_message},
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
        loop {
            match read_message::<ServerMessage>(&mut self.client) {
                Ok(Some(ServerMessage::Render(bytes))) => self.terminal.process(&bytes),
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
        let mut parser = new_parser(3, 8);
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
        (34, 3),
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
    assert_eq!(screen.cell(1, 0).unwrap().bgcolor(), selected_background);
}
