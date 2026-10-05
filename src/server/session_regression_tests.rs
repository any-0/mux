//! PTY histories adapted from tmux regress/input-{scroll,edit,malformed,sgr}.sh
//! and screen-redraw-status.sh. Assertions describe user-visible results.

use std::{
    fs,
    ops::{Deref, DerefMut},
    time::Duration,
};

use super::{
    tests::{TestServer, hello, read_frames},
    *,
};
use crate::protocol::{Mouse, MouseButton, MouseKind, MuxCommand, parse_for_test};

/// Client 1 attached at 12x40 to a pane running /bin/sh, with the terminal
/// that client sees.
struct Session {
    test: TestServer,
    terminal: vt100::Parser,
    output_number: usize,
}

impl Deref for Session {
    type Target = TestServer;
    fn deref(&self) -> &TestServer {
        &self.test
    }
}

impl DerefMut for Session {
    fn deref_mut(&mut self) -> &mut TestServer {
        &mut self.test
    }
}

impl Session {
    fn new() -> Self {
        let mut test = TestServer::new("session");
        test.attach();
        let attached = test.server.clients.get_mut(&1).unwrap();
        attached.rows = 12;
        attached.cols = 40;
        attached.terminal = crate::frame::TerminalFeatures::FULL;
        test.server.resize_active(1).unwrap();
        let mut session = Self {
            test,
            terminal: vt100::Parser::new(12, 40, 0),
            output_number: 0,
        };
        // Keep readline redraws out of byte-stream fixtures.
        session.pane().writer.send(b"exec /bin/sh\r").unwrap();
        session.output(b"\x1bcREADY", "READY");
        session
    }

    /// Prints `bytes` through the shell and real PTY. A fixture file avoids
    /// canonical input's line-length limit.
    fn output(&mut self, bytes: &[u8], marker: &str) {
        let path = self
            .directory
            .join(format!("output-{}", self.output_number));
        self.output_number += 1;
        fs::write(&path, bytes).unwrap();
        self.shell(&format!("stty -echo; cat '{}'", path.display()));
        self.wait_output(marker);
    }

    fn shell(&mut self, command: &str) {
        let line = format!("{command}\r");
        self.pane().writer.send(line.as_bytes()).unwrap();
    }

    fn command(&mut self, command: MuxCommand) {
        self.send(
            1,
            ClientMessage::Command {
                pane_id: None,
                command,
            },
        );
    }

    fn keys(&mut self, keys: &[&str]) {
        for key in keys {
            self.server.handle_key(1, parse_for_test(key)).unwrap();
        }
    }

    fn mouse(&mut self, kind: MouseKind, row: u16, col: u16) {
        let mouse = Mouse {
            kind,
            button: MouseButton::Left,
            row,
            col,
            modifiers: 0,
        };
        self.send(1, ClientMessage::Mouse(mouse));
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.send(1, ClientMessage::Resize { rows, cols });
        self.terminal.screen_mut().set_size(rows, cols);
    }

    /// Decodes exactly the bytes the client receives, on top of what it
    /// showed before, so missed incremental clears are observable.
    fn capture(&mut self) -> &vt100::Screen {
        self.test.server.render_all();
        read_frames(&mut self.test.client, &mut self.terminal);
        self.terminal.screen()
    }

    fn contents(&mut self) -> String {
        self.capture().contents()
    }

    fn first_row(&self, width: u16) -> String {
        self.pane().parser.screen().rows(0, width).next().unwrap()
    }

    fn bar(&mut self, expected: &[(u16, &str)]) {
        let screen = self.capture();
        for &(row, text) in expected {
            let actual: String = (0..3)
                .map(|col| screen.cell(row - 1, col).unwrap().contents().to_owned())
                .collect();
            assert_eq!(actual, text, "sidebar row {row}");
        }
        // Pane SGR must not leak into the bar, even on incremental repaints.
        for row in 0..12 {
            for col in 0..3 {
                let cell = screen.cell(row, col).unwrap();
                assert_eq!(cell.underline_style(), vt100::UnderlineStyle::None);
                assert_eq!(cell.underline_color(), vt100::Color::Default);
            }
        }
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
    assert_eq!(session.first_row(8), "ab  ef");
    session.bar(&[(5, " • ")]);

    let mut payload = b"\x1b]2;".to_vec();
    payload.extend(vec![b'x'; vt100::MAX_OSC_BYTES + 100]);
    payload.extend_from_slice(
        b"\x07\x1b[?9999z\x1b]999;bad\x07\x1b[1;1H\x1b[2KRECOVERED\x1b[10;1HMALFORMED",
    );
    session.output(&payload, "MALFORMED");
    assert_eq!(session.first_row(9), "RECOVERED");
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
    let wave = screen.cell(0, bar).unwrap();
    assert_eq!(wave.underline_style(), vt100::UnderlineStyle::Curly);
    assert_eq!(wave.underline_color(), vt100::Color::Rgb(255, 0, 0));
    let plain = screen.cell(0, bar + 4).unwrap();
    assert_eq!(plain.underline_style(), vt100::UnderlineStyle::None);
    assert_eq!(plain.underline_color(), vt100::Color::Default);
    session.bar(&[(5, " • ")]);

    session.output(b"\x1b[?1049h\x1b[2J\x1b[HALTERNATE", "ALTERNATE");
    session.command(MuxCommand::ZoomPane);
    assert!(session.contents().contains("ALTERNATE"));
    session.command(MuxCommand::ZoomPane);
    session.bar(&[(5, " • ")]);
    session.output(b"\x1b[?1049l\x1b[10;1HRETURNED", "RETURNED");
    assert!(session.contents().contains("WAVEPLAIN"));

    session.command(MuxCommand::NewWindow);
    session.output(b"\x1bcSECOND", "SECOND");
    session.bar(&[(4, " 1 "), (7, " • ")]);
    session.command(MuxCommand::SelectWindow(1));
    session.bar(&[(4, " • "), (7, " 2 ")]);
    assert!(session.contents().contains("WAVEPLAIN"));
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
        let consumed = replay_pane_journal(&mut parser, &records[..cut]).unwrap() as usize;
        let (expected, contents) = if cut == records.len() {
            (cut, "FIRST\nSECOND")
        } else if cut >= second {
            (second, "FIRST")
        } else if cut >= first {
            (first, "")
        } else {
            (0, "")
        };
        assert_eq!(consumed, expected, "cut {cut}");
        let size = if cut >= first { (3, 8) } else { (7, 17) };
        assert_eq!(parser.screen().size(), size, "cut {cut}");
        assert_eq!(parser.screen().contents(), contents, "cut {cut}");
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
        let truncated = packed_row(&row[..cut]);
        assert!(
            !parser.screen_mut().restore_history(&truncated),
            "cut {cut}"
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
        let packed = packed_row(&invalid);
        assert!(
            !parser.screen_mut().restore_history(&packed),
            "mutation at {offset}"
        );
    }
    let mut huge = packed_row(&row);
    huge[..4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(!parser.screen_mut().restore_history(&huge));
    huge[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(!parser.screen_mut().restore_history(&huge));
    let bad_journal = encode_journal_record(JOURNAL_HISTORY, &packed_row(&[0])).unwrap();
    assert!(replay_pane_journal(&mut parser, bad_journal.as_slice()).is_err());
    parser.screen_mut().set_size(3, 2);
    parser.screen_mut().set_scrollback(1);
    assert!(parser.screen().contents().contains('A'));
    assert_eq!(
        parser.screen().cell(0, 0).unwrap().fgcolor(),
        vt100::Color::Idx(1)
    );

    // Restored history is trimmed to the receiving pane's capacity.
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
    // Resize used to keep a wide leading half at the right edge, which erase
    // then indexed past.
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
    let text = |screen: &vt100::Screen| {
        screen
            .all_rows()
            .flat_map(|row| row.cells())
            .filter(|cell| !cell.is_wide_continuation())
            .map(|cell| cell.contents().to_owned())
            .collect::<String>()
    };
    for cols in [7, 3, 2, 1, 2, 4, 12] {
        parser.screen_mut().set_size(3, cols);
        let records = compacted_journal_records(parser.screen_mut()).unwrap();
        let mut restored = new_parser(3, cols);
        replay_pane_journal(&mut restored, records.as_slice()).unwrap();
        let restored_text = text(restored.screen());
        assert_eq!(restored_text, text(parser.screen()), "width {cols}");
        // Visible rows may be clipped when a client shrinks, but every
        // numbered line before them is in history.
        assert!(restored_text.contains("00:界e\u{301}:end"));
        let styled = restored
            .screen()
            .all_rows()
            .flat_map(|row| row.cells())
            .find(|cell| cell.contents() == "界")
            .unwrap();
        assert_eq!(styled.underline_style(), vt100::UnderlineStyle::Curly);
        assert_eq!(styled.underline_color(), vt100::Color::Idx(45));
    }
}

#[test]
fn background_pty_bell_cannot_overwrite_the_mode_tile_after_narrow_resize() {
    let mut session = Session::new();
    session.server.clients.get_mut(&1).unwrap().bell_style = BellStyle::Steady;
    session.command(MuxCommand::NewSession(Some("other".into())));
    session.output(b"\x1bcOTHER\x07", "OTHER");
    session.command(MuxCommand::ChooseTree);
    session.keys(&["Up", "Enter"]);
    assert_eq!(session.server.clients[&1].session_id, Some(0));
    session.resize(1, 40);
    // The tile stays the normal-mode dot; a pending-bell badge would need a
    // row of its own.
    assert_eq!(session.capture().cell(0, 1).unwrap().contents(), "●");
}

#[test]
fn tree_keeps_its_selection_when_an_earlier_background_shell_exits() {
    for expanded in [false, true] {
        let mut session = Session::new();
        session.command(MuxCommand::NewSession(Some("other".into())));
        session.output(b"\x1bcOTHER", "OTHER");
        session.command(MuxCommand::NewSession(Some("third".into())));
        session.output(b"\x1bcTHIRD", "THIRD");
        session.command(MuxCommand::ChooseTree);
        session.keys(&["Up"]);
        if expanded {
            // Select the pane rather than the session.
            session.keys(&["Right", "Down", "Down"]);
        }
        assert!(session.contents().contains("OTHER"));

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
        let selected = crate::frame::rgb(session.server.clients[&1].theme.panel_selected);
        let screen = session.capture();
        assert!(screen.contents().contains("OTHER"), "preview changed");
        let row = if expanded { 4 } else { 1 };
        assert_eq!(screen.cell(row, 0).unwrap().bgcolor(), selected);
    }
}

#[test]
fn mouse_clicks_follow_the_visible_sidebar_after_narrow_resize() {
    let mut session = Session::new();
    session.server.clients.get_mut(&1).unwrap().mouse = true;
    session.output(b"\x1bcFIRST", "FIRST");
    for marker in ["SECOND", "THIRD"] {
        session.command(MuxCommand::NewWindow);
        session.output(format!("\x1bc{marker}").as_bytes(), marker);
    }
    session.resize(7, 40);
    assert_eq!(session.capture().cell(2, 1).unwrap().contents(), "•");
    session.mouse(MouseKind::Down, 2, 1);
    assert!(
        session.contents().contains("THIRD"),
        "clicking the displayed current window must keep its PTY"
    );
    // The mode tile owns its row and must not select an offscreen window.
    session.mouse(MouseKind::Down, 0, 1);
    assert!(session.contents().contains("THIRD"));
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
        let cwd = session.directory.clone();
        session.send(2, hello(rows, cols, cwd, "before"));
        client
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        Self {
            client,
            terminal: vt100::Parser::new(rows, cols, 0),
        }
    }

    fn contents(&mut self, server: &mut Server) -> String {
        server.render_all();
        read_frames(&mut self.client, &mut self.terminal);
        self.terminal.screen().contents()
    }
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
    assert!(session.contents().contains("LIVE-END"));
    assert!(peer.contents(&mut session.server).contains("LIVE-END"));

    // The PTY itself takes the smallest client's size.
    let size = session.directory.join("pty-size");
    let marker = session.directory.join("size-ready");
    fs::write(&marker, b"SIZE-READY").unwrap();
    session.shell(&format!(
        "stty size > '{}'; cat '{}'",
        size.display(),
        marker.display()
    ));
    session.wait_output("SIZE-READY");
    assert_eq!(fs::read_to_string(&size).unwrap().trim(), "8 15");

    // Scrolling back is private to the client that scrolled.
    session.mouse(MouseKind::ScrollUp, 3, 8);
    assert!(!session.contents().contains("LIVE-END"));
    assert!(peer.contents(&mut session.server).contains("LIVE-END"));
    session.send(2, ClientMessage::Resize { rows: 6, cols: 18 });
    peer.terminal.screen_mut().set_size(6, 18);
    session.output(b"\x1b[H\x1b[2KLIVE-END", "LIVE-END");
    // A peer's resize must not force this client's history view live.
    assert!(!session.contents().contains("LIVE-END"));
    for _ in 0..20 {
        session.mouse(MouseKind::ScrollDown, 3, 8);
    }
    assert!(session.contents().contains("LIVE-END"));

    // Windows are shared, but each client selects its own.
    session.command(MuxCommand::NewWindow);
    session.output(b"\x1bcSHARED-SECOND", "SHARED-SECOND");
    assert!(session.contents().contains("SHARED-SECOND"));
    assert!(peer.contents(&mut session.server).contains("SHARED-SECOND"));
    session.send(
        2,
        ClientMessage::Command {
            pane_id: None,
            command: MuxCommand::SelectWindow(1),
        },
    );
    assert!(session.contents().contains("LIVE-END"));
    assert!(peer.contents(&mut session.server).contains("LIVE-END"));
    assert_eq!(session.capture().cell(3, 1).unwrap().contents(), "•");
    peer.contents(&mut session.server);
    assert_eq!(peer.terminal.screen().cell(1, 1).unwrap().contents(), "•");
}

/// In raw mode, prints `setup`, records the next `length` bytes of pane
/// input into the returned file, then prints `done`.
fn record_input(session: &mut Session, setup: &[u8], length: usize, done: &[u8]) -> PathBuf {
    let [setup_file, captured, done_file] =
        ["record-setup", "record-input", "record-done"].map(|name| session.directory.join(name));
    fs::write(&setup_file, setup).unwrap();
    fs::write(&done_file, done).unwrap();
    session.shell(&format!(
        "stty raw -echo; cat '{}'; dd bs=1 count={length} of='{}' 2>/dev/null; stty -raw -echo; cat '{}'",
        setup_file.display(),
        captured.display(),
        done_file.display()
    ));
    captured
}

#[test]
fn real_pty_mouse_reports_are_local_to_the_clicked_split_and_popups_consume_clicks() {
    let mut session = Session::new();
    session.server.clients.get_mut(&1).unwrap().mouse = true;
    session.output(b"\x1bcLEFT-PANE", "LEFT-PANE");
    session.command(MuxCommand::SplitVertical);
    session.shell("exec /bin/sh");
    session.output(b"\x1bcRIGHT-PANE", "RIGHT-PANE");
    // At 40 columns: a 5-column bar, 17 left cells, a divider, 17 right.
    let contents = session.contents();
    assert!(contents.contains("LEFT-PANE") && contents.contains("RIGHT-PANE"));
    let expected = b"\x1b[<0;2;4M\x1b[<32;3;5M\x1b[<0;3;5m\x1b[<64;2;4M";
    let captured = record_input(
        &mut session,
        b"\x1b[?1002h\x1b[?1006h\r\nMOUSE-READY",
        expected.len(),
        b"\x1b[?1002l\x1b[?1006l\r\nMOUSE-DONE",
    );
    session.wait_output("MOUSE-READY");
    // A click while the tree is open goes to the tree, not the pane.
    session.command(MuxCommand::ChooseTree);
    session.mouse(MouseKind::Down, 3, 24);
    session.keys(&["Escape"]);
    session.mouse(MouseKind::Down, 3, 24);
    session.mouse(MouseKind::Drag, 4, 25);
    session.mouse(MouseKind::Up, 4, 25);
    session.mouse(MouseKind::ScrollUp, 3, 24);
    session.wait_output("MOUSE-DONE");
    assert_eq!(fs::read(&captured).unwrap(), expected);
    // Without reporting, a click selects the other pane and sends nothing.
    session.mouse(MouseKind::Down, 1, 5);
    assert_eq!(session.first_row(9), "LEFT-PANE");
    assert!(session.contents().contains("MOUSE-DONE"));
}

#[test]
fn pasted_paste_delimiters_cannot_end_a_bracketed_paste_early() {
    let mut session = Session::new();
    let expected = b"\x1b[200~ab\ncd\x1b[201~";
    let captured = record_input(
        &mut session,
        b"\x1b[?2004h\r\nPASTE-READY",
        expected.len(),
        b"\r\nPASTE-DONE",
    );
    session.wait_output("PASTE-READY");
    session.send(1, ClientMessage::Paste("a\x1b[201~b\ncd\x1b[200~".into()));
    session.wait_output("PASTE-DONE");
    assert_eq!(fs::read(&captured).unwrap(), expected);
}

#[test]
fn style_transitions_and_the_active_pen_survive_journal_compaction() {
    let mut parser = new_parser(4, 24);
    parser.process(b"\x1b[1;3;7;38;2;12;34;56;48;2;65;43;21;58;2;9;8;7m\x1b[4:2mD\x1b[4:4mO\x1b[4:5mS\x1b[2;23;27mI\x1b[0mP\x1b[3;4:4;58;2;1;2;3m");
    let records = compacted_journal_records(parser.screen_mut()).unwrap();
    let mut restored = new_parser(4, 24);
    replay_pane_journal(&mut restored, records.as_slice()).unwrap();
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
    // SGR 2 adds faint without clearing bold, as in xterm.
    let dim = screen.cell(0, 3).unwrap();
    assert!(dim.dim() && dim.bold() && !dim.italic() && !dim.inverse());
    assert_eq!(
        screen.cell(0, 4).unwrap().underline_style(),
        vt100::UnderlineStyle::None
    );
    // Output after the restore continues with the pen that was active.
    restored.process(b"N\x1b[0mZ");
    let next = restored.screen().cell(0, 5).unwrap();
    assert!(next.italic());
    assert_eq!(next.underline_style(), vt100::UnderlineStyle::Dotted);
    assert_eq!(next.underline_color(), vt100::Color::Rgb(1, 2, 3));
    assert_eq!(
        restored.screen().cell(0, 6).unwrap().underline_style(),
        vt100::UnderlineStyle::None
    );
}

#[test]
fn a_resize_or_refresh_forgets_what_the_terminal_showed() {
    let mut session = Session::new();
    let frame_rows = |session: &Session| session.server.clients[&1].frame.rows();
    session.capture();
    assert_eq!(frame_rows(&session), 12);
    // Even at the same size the terminal may have cropped or cleared its
    // screen, so the next frame is a full repaint.
    session.resize(12, 40);
    assert_eq!(frame_rows(&session), 0);
    session.capture();
    assert_eq!(frame_rows(&session), 12);
    session.command(MuxCommand::RefreshClient);
    assert_eq!(frame_rows(&session), 0);
    assert!(session.contents().contains("READY"));
}
