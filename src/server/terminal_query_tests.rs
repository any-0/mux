use super::{TerminalCallbacks, TerminalColors, process_terminal_bytes};
use base64::{Engine, engine::general_purpose::STANDARD};

fn parser() -> vt100::Parser<TerminalCallbacks> {
    vt100::Parser::new_with_callbacks(4, 20, 0, TerminalCallbacks::default())
}

#[test]
fn queries_are_answered_at_their_position_in_the_stream() {
    let mut parser = parser();
    let mut prefix = Vec::new();

    process_terminal_bytes(
        &mut parser,
        &mut prefix,
        b"\x1b[2;3H\x1b[6n\x1b[4;8H\x1b[?6n",
    );

    assert_eq!(parser.callbacks().responses, b"\x1b[2;3R\x1b[?4;8R");
}

#[test]
fn split_queries_are_completed_by_the_parser_callbacks() {
    let mut parser = parser();
    let mut prefix = Vec::new();
    parser.callbacks_mut().set_colors(TerminalColors {
        foreground: (0x01, 0x02, 0x03),
        background: (0x04, 0x05, 0x06),
        cursor: (0x07, 0x08, 0x09),
    });

    for byte in b"\x1b]11;?\x1b\\\x1b]10;?\x07\x1b[5n\x1b[c\x1b[>c" {
        process_terminal_bytes(&mut parser, &mut prefix, &[*byte]);
    }

    assert_eq!(
        parser.callbacks().responses,
        b"\x1b]11;rgb:0404/0505/0606\x1b\\\x1b]10;rgb:0101/0202/0303\x1b\\\x1b[0n\x1b[?1;2c\x1b[>0;100;0c"
    );
    assert!(prefix.is_empty());
}

#[test]
fn unfinished_osc_input_does_not_accumulate_in_mux_storage() {
    let mut parser = parser();
    let mut prefix = Vec::new();

    process_terminal_bytes(&mut parser, &mut prefix, b"\x1b]11;");
    for _ in 0..10_000 {
        process_terminal_bytes(&mut parser, &mut prefix, b"x");
    }

    assert!(prefix.is_empty());
    assert!(parser.callbacks().responses.is_empty());

    process_terminal_bytes(&mut parser, &mut prefix, b"\x07\x1b[5n");
    assert_eq!(parser.callbacks().responses, b"\x1b[0n");
}

#[test]
fn ordinary_clipboard_payloads_fit_in_the_bounded_osc_buffer() {
    let data = vec![b'x'; 8 * 1024];
    let sequence = format!("\x1b]52;c;{}\x07", STANDARD.encode(&data));
    let mut parser = parser();

    parser.process(sequence.as_bytes());

    assert_eq!(parser.callbacks().clipboard_writes.len(), 1);
    assert_eq!(parser.callbacks().clipboard_writes[0].data, data);
}

#[test]
fn an_oversized_clipboard_sequence_is_discarded_instead_of_truncated() {
    let data = vec![b'x'; vt100::MAX_OSC_BYTES];
    let sequence = format!("\x1b]52;cp;{}\x07", STANDARD.encode(data));
    let mut parser = parser();

    parser.process(sequence.as_bytes());

    assert!(parser.callbacks().clipboard_writes.is_empty());
}
