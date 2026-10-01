//! Behaviour mux added to the emulator, each case checked against what xterm
//! and tmux do. The harness in `harness/` compares whole random streams with
//! an independent emulator; these pin the individual rules down in CI.

fn parser(rows: u16, cols: u16) -> crate::Parser {
    crate::Parser::new(rows, cols, 100)
}

fn row(parser: &crate::Parser, row: u16) -> String {
    let (_, cols) = parser.screen().size();
    parser.screen().rows(0, cols).nth(usize::from(row)).unwrap()
}

fn screen_after(rows: u16, cols: u16, input: &str) -> crate::Parser {
    let mut parser = parser(rows, cols);
    parser.process(input.as_bytes());
    parser
}

#[test]
fn dec_special_graphics_draws_lines_in_g0_and_g1() {
    let parser = screen_after(3, 10, "\x1b(0lqqk\x1b(Bx\r\n\x1b)0\x0elqk\x0fq");
    assert_eq!(row(&parser, 0), "┌──┐x");
    assert_eq!(row(&parser, 1), "┌─┐q");
}

#[test]
fn saved_cursor_keeps_the_character_sets() {
    let parser = screen_after(2, 10, "\x1b(0\x1b7\x1b(Bq\x1b8q");
    assert_eq!(row(&parser, 0), "─");
}

#[test]
fn rep_repeats_the_last_character_and_stops_at_a_screenful() {
    let parser = screen_after(2, 10, "ab\x1b[3b");
    assert_eq!(row(&parser, 0), "abbbb");
    let parser = screen_after(2, 4, "x\x1b[65535b");
    assert_eq!(row(&parser, 0), "xxxx");
    // Line drawing is repeated as drawn.
    let parser = screen_after(2, 10, "\x1b(0q\x1b[4b");
    assert_eq!(row(&parser, 0), "─────");
}

#[test]
fn insert_mode_shifts_the_rest_of_the_line() {
    let parser = screen_after(2, 10, "abc\r\x1b[4hXY\x1b[4lZ");
    assert_eq!(row(&parser, 0), "XYZbc");
}

#[test]
fn insertion_that_pushes_a_wide_character_off_the_edge_blanks_it() {
    let parser = screen_after(2, 5, "abc中\r\x1b[4hX");
    assert_eq!(row(&parser, 0), "Xabc");
    for col in 0..5 {
        let cell = parser.screen().cell(0, col).unwrap();
        assert!(!cell.is_wide() && !cell.is_wide_continuation(), "col {col}");
    }
}

#[test]
fn ich_with_a_huge_count_is_instant_and_splits_wide_characters_cleanly() {
    let started = std::time::Instant::now();
    let parser = screen_after(2, 10, "a中b\x1b[1;3H\x1b[65535@");
    assert!(started.elapsed() < std::time::Duration::from_millis(50));
    assert_eq!(row(&parser, 0), "a");
    let parser = screen_after(2, 10, "a中b\x1b[1;3H\x1b[@");
    // The insertion splits 中, so neither half survives.
    assert_eq!(row(&parser, 0), "a   b");
}

#[test]
fn tab_stops_can_be_set_cleared_and_moved_between() {
    let parser = screen_after(2, 30, "\x1b[3g\x1b[5G\x1bH\x1b[12G\x1bH\r\tx\ty");
    assert_eq!(row(&parser, 0), "    x      y");
    let mut parser = screen_after(2, 30, "\x1b[20G\x1b[2Zz");
    assert_eq!(parser.screen().cursor_position(), (0, 9));
    parser.process(b"\x1b[I");
    assert_eq!(parser.screen().cursor_position(), (0, 16));
}

#[test]
fn a_tab_at_the_right_margin_leaves_the_wrap_pending() {
    let parser = screen_after(3, 4, "abcd\tX");
    assert_eq!(row(&parser, 0), "abcd");
    assert_eq!(row(&parser, 1), "X");
}

#[test]
fn without_autowrap_the_last_column_is_overwritten() {
    let parser = screen_after(3, 5, "\x1b[?7labcdefg\x1b[?7h");
    assert_eq!(row(&parser, 0), "abcdg");
    assert_eq!(parser.screen().cursor_position(), (0, 4));
    // A combining mark belongs to the character drawn in the last column.
    let parser = screen_after(3, 5, "\x1b[?7labcde\u{301}");
    assert_eq!(parser.screen().cell(0, 4).unwrap().contents(), "e\u{301}");
    // A wide character that does not fit is dropped.
    let parser = screen_after(3, 5, "\x1b[?7labcd中");
    assert_eq!(row(&parser, 0), "abcd");
}

#[test]
fn cursor_movement_ends_a_pending_wrap_but_line_feed_does_not() {
    let parser = screen_after(3, 4, "abcd\x1b[Dx");
    assert_eq!(row(&parser, 0), "abxd");
    let parser = screen_after(3, 4, "abcd\x08x");
    assert_eq!(row(&parser, 0), "abxd");
    let parser = screen_after(3, 4, "abcd\nx");
    assert_eq!(row(&parser, 2), "x");
}

#[test]
fn soft_reset_clears_modes_and_rendition_but_keeps_the_cursor() {
    let mut parser = screen_after(4, 10, "\x1b[2;3r\x1b[?6h\x1b[4h\x1b[1;31m\x1b[3;4H");
    parser.process(b"\x1b[!pX");
    assert_eq!(row(&parser, 2), "   X");
    let cell = parser.screen().cell(2, 3).unwrap();
    assert!(!cell.bold());
    assert_eq!(cell.fgcolor(), crate::Color::Default);
    // Margins are gone: the cursor can reach the last row.
    parser.process(b"\x1b[4;1Hlast");
    assert_eq!(row(&parser, 3), "last");
}

#[test]
fn a_full_reset_keeps_the_scrollback() {
    let mut parser = screen_after(2, 10, "one\r\ntwo\r\nthree\r\n");
    parser.process(b"\x1bc");
    assert_eq!(row(&parser, 0), "");
    parser.screen_mut().set_scrollback(10);
    assert!(parser.screen().contents().contains("one"));
}

#[test]
fn erase_saved_lines_clears_the_scrollback() {
    let mut parser = screen_after(2, 10, "one\r\ntwo\r\nthree\r\n");
    parser.process(b"\x1b[3J");
    parser.screen_mut().set_scrollback(10);
    assert_eq!(parser.screen().scrollback(), 0);
}

#[test]
fn the_alternate_screen_shares_the_cursor_and_margins_but_saves_separately() {
    let parser = screen_after(6, 10, "\x1b[2;4r\x1b[3;5H\x1b[?1049hA");
    assert_eq!(row(&parser, 2), "    A");
    // The margins set before switching still bound scrolling: a line feed
    // at the bottom margin scrolls instead of moving below it.
    let parser = screen_after(6, 10, "\x1b[2;3r\x1b[?1049h\x1b[3;1Hx\ny");
    assert_eq!(row(&parser, 1), "x");
    assert_eq!(row(&parser, 2), " y");
    // A restore on the alternate screen does not see the normal screen's save.
    let parser = screen_after(3, 10, "\x1b[31m\x1b[?1049h\x1b[uZ");
    assert_eq!(parser.screen().cell(0, 0).unwrap().fgcolor(), crate::Color::Default);
    // 1047 switches without saving, and clears on the way back.
    let parser = screen_after(3, 10, "main\x1b[?1047halt\x1b[?1047l");
    assert_eq!(row(&parser, 0), "main");
}

#[test]
fn origin_mode_addresses_rows_from_the_top_margin() {
    let parser = screen_after(6, 10, "\x1b[3;5r\x1b[?6h\x1b[2dX\x1b[9dY");
    assert_eq!(row(&parser, 3), "X");
    assert_eq!(row(&parser, 4), " Y");
}

#[test]
fn reverse_index_above_the_region_does_not_scroll_it() {
    let parser = screen_after(6, 10, "\x1b[5;1Hkeep\x1b[5;6r\x1b[1;1H\x1bM");
    assert_eq!(row(&parser, 4), "keep");
}

#[test]
fn an_empty_scroll_region_is_ignored() {
    let parser = screen_after(4, 10, "\x1b[2;3H\x1b[3;3rX");
    assert_eq!(row(&parser, 1), "  X");
}

#[test]
fn scrolled_in_lines_take_the_current_background() {
    let parser = screen_after(2, 4, "\x1b[44m\n\n\n");
    let cell = parser.screen().cell(1, 0).unwrap();
    assert_eq!(cell.bgcolor(), crate::Color::Idx(4));
    assert!(!cell.inverse());
}

#[test]
fn bold_and_faint_combine_and_every_rendition_survives_history() {
    let mut parser = crate::Parser::new(1, 20, 100);
    parser.process(b"\x1b[1;2mA\x1b[22;9mB\x1b[29;5mC\x1b[25;8mD\x1b[28;53mE\x1b[55;21mF\r\n\r\n");
    let history = parser.screen().encode_history();
    let mut restored = crate::Parser::new(1, 20, 100);
    assert!(restored.screen_mut().restore_history(&history));
    restored.screen_mut().set_scrollback(2);
    let cell = |col| restored.screen().cell(0, col).unwrap();
    assert!(cell(0).bold() && cell(0).dim());
    assert!(cell(1).strikethrough() && !cell(1).bold());
    assert!(cell(2).blink() && !cell(2).strikethrough());
    assert!(cell(3).hidden() && !cell(3).blink());
    assert!(cell(4).overline() && !cell(4).hidden());
    assert_eq!(cell(5).underline_style(), crate::UnderlineStyle::Double);
    assert!(!cell(5).overline());
}

#[test]
fn rows_without_new_renditions_keep_the_original_history_format() {
    let mut parser = crate::Parser::new(1, 10, 10);
    parser.process(b"\x1b[1;4;31mold\r\n\r\n");
    let history = parser.screen().encode_history();
    let mut raw = if history[12] == 1 {
        zstd::bulk::decompress(&history[13..], 1 << 20).unwrap()
    } else {
        history[13..].to_vec()
    };
    // The first row's flag byte is the one after its two-byte width.
    assert_eq!(raw[2] & 0b10, 0);
    raw.clear();
}

#[test]
fn resizing_rewraps_lines_and_keeps_the_cursor_after_the_text() {
    let mut parser = crate::Parser::new(3, 10, 100);
    parser.process(b"0123456789abcdef\r\n$ ");
    parser.screen_mut().set_size(3, 6);
    assert_eq!(row(&parser, 0), "6789ab");
    assert_eq!(row(&parser, 1), "cdef");
    assert_eq!(row(&parser, 2), "$ ");
    assert_eq!(parser.screen().cursor_position(), (2, 2));
    parser.screen_mut().set_size(3, 20);
    assert_eq!(row(&parser, 0), "0123456789abcdef");
    assert_eq!(parser.screen().cursor_position(), (1, 2));
    // A line that exactly fills the width keeps the cursor after it.
    let mut parser = crate::Parser::new(3, 4, 100);
    parser.process(b"abcd");
    parser.screen_mut().set_size(3, 8);
    parser.process(b"e");
    assert_eq!(row(&parser, 0), "abcde");
}

#[test]
fn shrinking_keeps_the_newest_rows_and_growing_brings_history_back() {
    let mut parser = crate::Parser::new(6, 10, 100);
    parser.process(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\n$ ");
    parser.screen_mut().set_size(3, 10);
    assert_eq!(row(&parser, 0), "four");
    assert_eq!(row(&parser, 2), "$ ");
    parser.screen_mut().set_size(6, 10);
    assert_eq!(row(&parser, 0), "one");
    assert_eq!(row(&parser, 5), "$ ");
    // Blank rows below the cursor go first.
    let mut parser = crate::Parser::new(6, 10, 100);
    parser.process(b"top\r\n$ ");
    parser.screen_mut().set_size(3, 10);
    assert_eq!(row(&parser, 0), "top");
}

#[test]
fn text_printed_after_a_clear_does_not_join_the_line_before_it() {
    let mut parser = crate::Parser::new(2, 5, 100);
    parser.process(b"abcdefgh");
    parser.process(b"\x1b[H\x1b[2Jnew");
    parser.screen_mut().set_size(2, 20);
    assert_eq!(row(&parser, 0), "new");
}

#[test]
fn one_column_screens_never_panic() {
    let inputs = [
        "中\x1b[2J中a\u{301}",
        "😀\u{fe0f}\u{200d}🔥\r\nx\x08\u{301}",
        "\x1b[?7l中a\u{301}\x1b[P\x1b[@",
    ];
    for input in inputs {
        let mut parser = crate::Parser::new(3, 4, 20);
        parser.process(input.as_bytes());
        for (rows, cols) in [(3, 1), (1, 1), (2, 3), (4, 1), (2, 6)] {
            parser.screen_mut().set_size(rows, cols);
            parser.process(input.as_bytes());
            let _ = parser.screen().contents_formatted();
            let history = parser.screen().encode_history();
            assert!(crate::Parser::new(rows, cols, 20)
                .screen_mut()
                .restore_history(&history));
        }
    }
}

