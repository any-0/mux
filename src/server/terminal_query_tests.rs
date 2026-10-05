use super::{ClipboardWrite, TerminalCallbacks, TerminalColors, process_terminal_bytes};
use base64::{Engine, engine::general_purpose::STANDARD};

fn parser() -> vt100::Parser<TerminalCallbacks> {
    vt100::Parser::new_with_callbacks(4, 20, 0, TerminalCallbacks::default())
}

#[test]
fn queries_are_answered_in_stream_order_even_when_split() {
    let mut parser = parser();
    process_terminal_bytes(&mut parser, b"\x1b[2;3H\x1b[6n\x1b[4;8H\x1b[?6n");
    assert_eq!(parser.callbacks().responses, b"\x1b[2;3R\x1b[?4;8R");

    let mut parser = self::parser();
    parser.callbacks_mut().set_colors(TerminalColors {
        foreground: (0x01, 0x02, 0x03),
        background: (0x04, 0x05, 0x06),
        cursor: (0x07, 0x08, 0x09),
    });
    for byte in b"\x1b]11;?\x1b\\\x1b]10;?\x07\x1b[5n\x1b[c\x1b[>c" {
        process_terminal_bytes(&mut parser, &[*byte]);
    }
    assert_eq!(
        parser.callbacks().responses,
        b"\x1b]11;rgb:0404/0505/0606\x1b\\\x1b]10;rgb:0101/0202/0303\x1b\\\x1b[0n\x1b[?1;2c\x1b[>0;100;0c"
    );
}

#[test]
fn unfinished_osc_input_does_not_accumulate_in_mux_storage() {
    let mut parser = parser();
    process_terminal_bytes(&mut parser, b"\x1b]11;");
    for _ in 0..10_000 {
        process_terminal_bytes(&mut parser, b"x");
    }
    assert!(parser.callbacks().responses.is_empty());
    process_terminal_bytes(&mut parser, b"\x07\x1b[5n");
    assert_eq!(parser.callbacks().responses, b"\x1b[0n");
}

#[test]
fn clipboard_writes_are_reported_whole_or_not_at_all() {
    let clipboard = |selection: &str, data: &[u8]| {
        let mut parser = parser();
        let sequence = format!("\x1b]52;{selection};{}\x07", STANDARD.encode(data));
        parser.process(sequence.as_bytes());
        std::mem::take(&mut parser.callbacks_mut().clipboard_writes)
    };
    let data = vec![b'x'; 8 * 1024];
    assert_eq!(
        clipboard("c", &data),
        [ClipboardWrite {
            selection: b"c".to_vec(),
            data,
        }]
    );
    // An oversized sequence is discarded instead of truncated.
    assert!(clipboard("cp", &vec![b'x'; vt100::MAX_OSC_BYTES]).is_empty());
}

#[test]
fn decrqss_reports_the_current_sgr_so_neovim_sends_undercurls() {
    let mut parser = parser();

    // Neovim's probe, then a setting mux does not report.
    process_terminal_bytes(&mut parser, b"\x1b[0m\x1b[4:3m\x1bP$qm\x1b\\\x1bP$qr\x1b\\");

    assert_eq!(
        parser.callbacks().responses,
        b"\x1bP1$r0;4:3m\x1b\\\x1bP0$r\x1b\\"
    );
}

#[test]
fn programs_set_query_and_reset_their_cursor_colour() {
    let mut parser = parser();
    let theme_cursor = parser.callbacks().cursor_color;
    assert_eq!(theme_cursor, None);

    for (spec, expected) in [
        ("#ff8000", (0xff, 0x80, 0x00)),
        ("#f80", (0xff, 0x88, 0x00)),
        ("#ffff80000000", (0xff, 0x80, 0x00)),
        ("rgb:ff/80/00", (0xff, 0x80, 0x00)),
        ("rgb:f/8/0", (0xff, 0x88, 0x00)),
        ("rgb:ffff/0000/8000", (0xff, 0x00, 0x80)),
    ] {
        process_terminal_bytes(&mut parser, format!("\x1b]12;{spec}\x07").as_bytes());
        assert_eq!(parser.callbacks().cursor_color, Some(expected), "{spec}");
    }
    // Names, malformed specs and non-ASCII are ignored, keeping the last colour.
    for spec in ["red", "#12345", "rgb:1/2", "rgb:+f/0/0", "#a€bcd"] {
        process_terminal_bytes(&mut parser, format!("\x1b]12;{spec}\x07").as_bytes());
        assert_eq!(
            parser.callbacks().cursor_color,
            Some((0xff, 0x00, 0x80)),
            "{spec}"
        );
    }

    process_terminal_bytes(&mut parser, b"\x1b]12;?\x07");
    assert_eq!(
        parser.callbacks().responses,
        b"\x1b]12;rgb:ffff/0000/8080\x1b\\"
    );
    process_terminal_bytes(&mut parser, b"\x1b]112\x07");
    assert_eq!(parser.callbacks().cursor_color, None);
}
