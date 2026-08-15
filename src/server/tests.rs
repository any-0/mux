use std::{
    env,
    fs::{File, OpenOptions},
};

use crate::{
    config::{Action, Mode},
    frame::{CursorShape, FrameCursor, Rgb, rgb},
    protocol::{Mouse, MouseButton, MouseKind},
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

/// Paints a frame and returns the escape sequences a fresh client receives.
fn painted(rows: u16, cols: u16, paint: impl FnOnce(&mut Frame)) -> String {
    let mut frame = Frame::default();
    frame.reset(rows, cols);
    paint(&mut frame);
    let mut output = Vec::new();
    frame.diff(&Frame::default(), ColorDepth::TrueColor, &mut output);
    String::from_utf8(output).unwrap()
}

/// Paints over a blank screen of the same size, so the result contains only
/// the cells the painter actually touched.
fn repainted(rows: u16, cols: u16, paint: impl FnOnce(&mut Frame)) -> String {
    let mut previous = Frame::default();
    previous.reset(rows, cols);
    let mut frame = Frame::default();
    frame.reset(rows, cols);
    paint(&mut frame);
    let mut output = Vec::new();
    frame.diff(&previous, ColorDepth::TrueColor, &mut output);
    String::from_utf8(output).unwrap()
}

#[test]
fn persisted_state_migrates_v1_and_keeps_the_last_active_pane_in_v2() {
    let legacy = PersistedStateV1 {
        version: LEGACY_STATE_VERSION,
        next_session_id: 3,
        next_pane_id: 7,
        sessions: Vec::new(),
    };
    let bytes = bincode::serde::encode_to_vec(&legacy, bincode::config::standard()).unwrap();
    let migrated = decode_persisted_state(&bytes).unwrap();
    assert_eq!(migrated.version, STATE_VERSION);
    assert_eq!(migrated.next_session_id, 3);
    assert_eq!(migrated.next_pane_id, 7);
    assert_eq!(migrated.last_active_pane, None);

    let current = PersistedState {
        version: STATE_VERSION,
        next_session_id: 3,
        next_pane_id: 7,
        sessions: Vec::new(),
        last_active_pane: Some(6),
    };
    let bytes = bincode::serde::encode_to_vec(&current, bincode::config::standard()).unwrap();
    let decoded = decode_persisted_state(&bytes).unwrap();
    assert_eq!(decoded.last_active_pane, Some(6));
}

#[test]
fn state_saved_before_resizable_splits_comes_back_split_evenly() {
    let previous = PersistedStateV2 {
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
                panes: vec![
                    PersistedPane {
                        id: 0,
                        cwd: PathBuf::from("/tmp"),
                        rows: 10,
                        cols: 40,
                    },
                    PersistedPane {
                        id: 1,
                        cwd: PathBuf::from("/tmp"),
                        rows: 10,
                        cols: 40,
                    },
                ],
            }],
        }],
    };
    let bytes = bincode::serde::encode_to_vec(&previous, bincode::config::standard()).unwrap();
    let migrated = decode_persisted_state(&bytes).unwrap();
    assert_eq!(migrated.version, STATE_VERSION);
    assert_eq!(migrated.last_active_pane, Some(1));
    let PaneLayout::Split { ratio, .. } = &migrated.sessions[0].windows[0].layout else {
        panic!("the split survived the migration");
    };
    assert_eq!(*ratio, EVEN_SPLIT);
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
    let mut parser = new_parser(24, 80);
    // A plain shell has not asked for the mouse, so mux keeps the event.
    assert_eq!(mouse_report(parser.screen(), click), None);

    // SGR mouse reporting, as vim and less turn on.
    parser.process(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(
        mouse_report(parser.screen(), click).unwrap(),
        b"\x1b[<0;10;5M".to_vec()
    );
    let release = Mouse {
        kind: MouseKind::Up,
        ..click
    };
    assert_eq!(
        mouse_report(parser.screen(), release).unwrap(),
        b"\x1b[<0;10;5m".to_vec()
    );
    let wheel = Mouse {
        kind: MouseKind::ScrollUp,
        ..click
    };
    assert_eq!(
        mouse_report(parser.screen(), wheel).unwrap(),
        b"\x1b[<64;10;5M".to_vec()
    );
    // Press-only mode reports the press but not the release.
    let mut press_only = new_parser(24, 80);
    press_only.process(b"\x1b[?9h\x1b[?1006h");
    assert!(mouse_report(press_only.screen(), click).is_some());
    assert_eq!(mouse_report(press_only.screen(), release), None);

    // Without SGR the report is the original three-byte form.
    let mut plain = new_parser(24, 80);
    plain.process(b"\x1b[?1000h");
    assert_eq!(
        mouse_report(plain.screen(), click).unwrap(),
        vec![0x1b, b'[', b'M', 32, 32 + 10, 32 + 5]
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
    // Clearing the title gives the window back its default name.
    parser.process(b"\x1b]2;\x07");
    assert_eq!(parser.callbacks().title, None);
}

#[test]
fn a_zoomed_window_shows_only_the_active_pane() {
    let area = Rect {
        row: 0,
        col: 0,
        rows: 24,
        cols: 80,
    };
    let layout = PaneLayout::Split {
        axis: SplitAxis::Vertical,
        ratio: EVEN_SPLIT,
        first: Box::new(PaneLayout::Pane(1)),
        second: Box::new(PaneLayout::Pane(2)),
    };
    let (regions, dividers) = window_regions(&layout, 2, true, area);
    assert_eq!(regions, vec![(2, area)]);
    assert!(dividers.is_empty(), "nothing to divide with one pane shown");

    // The layout itself is untouched, so unzooming restores the split.
    let (regions, dividers) = window_regions(&layout, 2, false, area);
    assert_eq!(regions.len(), 2);
    assert_eq!(dividers.len(), 1);
}

#[test]
fn dividers_that_meet_are_joined_instead_of_overwriting_each_other() {
    let area = Rect {
        row: 0,
        col: 0,
        rows: 5,
        cols: 7,
    };
    // Panes 1 and 2 stacked on the left, pane 3 down the right-hand side.
    let layout = PaneLayout::Split {
        axis: SplitAxis::Vertical,
        ratio: EVEN_SPLIT,
        first: Box::new(PaneLayout::Split {
            axis: SplitAxis::Horizontal,
            ratio: EVEN_SPLIT,
            first: Box::new(PaneLayout::Pane(1)),
            second: Box::new(PaneLayout::Pane(2)),
        }),
        second: Box::new(PaneLayout::Pane(3)),
    };
    let (_, dividers) = window_regions(&layout, 1, false, area);
    let cells = divider_cells(&dividers, 0);
    // The horizontal divider ends against the vertical one, whose cell has
    // to show the line arriving from its left.
    assert_eq!(
        cells,
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

    // A pane on each side of the vertical divider splits it into a cross.
    let layout = PaneLayout::Split {
        axis: SplitAxis::Vertical,
        ratio: EVEN_SPLIT,
        first: Box::new(PaneLayout::Split {
            axis: SplitAxis::Horizontal,
            ratio: EVEN_SPLIT,
            first: Box::new(PaneLayout::Pane(1)),
            second: Box::new(PaneLayout::Pane(2)),
        }),
        second: Box::new(PaneLayout::Split {
            axis: SplitAxis::Horizontal,
            ratio: EVEN_SPLIT,
            first: Box::new(PaneLayout::Pane(3)),
            second: Box::new(PaneLayout::Pane(4)),
        }),
    };
    let (_, dividers) = window_regions(&layout, 1, false, area);
    let junctions: Vec<_> = divider_cells(&dividers, 0)
        .into_iter()
        .filter(|(_, glyph)| !matches!(*glyph, "│" | "─"))
        .collect();
    assert_eq!(junctions, vec![((3, 4), "┼")]);

    // The bar shifts every cell right without changing which ones join.
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
    let area = Rect {
        row: 0,
        col: 0,
        rows: 24,
        cols: 81,
    };
    let mut layout = PaneLayout::Split {
        axis: SplitAxis::Vertical,
        ratio: EVEN_SPLIT,
        first: Box::new(PaneLayout::Pane(1)),
        second: Box::new(PaneLayout::Split {
            axis: SplitAxis::Vertical,
            ratio: EVEN_SPLIT,
            first: Box::new(PaneLayout::Pane(2)),
            second: Box::new(PaneLayout::Pane(3)),
        }),
    };
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
    assert_eq!(width(&layout, 1), before_1, "the outer split held still");
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
fn reads_the_working_directory_of_a_process() {
    assert_eq!(
        process_cwd(std::process::id()).unwrap(),
        env::current_dir().unwrap()
    );
}

#[test]
fn direct_tree_keys_leave_tree_commands_available() {
    assert_eq!(
        Bindings::defaults().get(Mode::Tree, &crate::config::parse_key("1").unwrap()),
        Some(Action::TreeSelect(1))
    );
    assert_eq!(
        Bindings::defaults().get(Mode::Tree, &crate::config::parse_key("9").unwrap()),
        Some(Action::TreeSelect(9))
    );
    assert_eq!(
        Bindings::defaults().get(Mode::Tree, &crate::config::parse_key("Alt-a").unwrap()),
        Some(Action::EnterLeader)
    );
    assert_eq!(
        Bindings::defaults().get(Mode::Tree, &crate::config::parse_key("Alt-s").unwrap()),
        Some(Action::SessionTree)
    );
    assert_eq!(
        Bindings::defaults().get(Mode::Tree, &crate::config::parse_key("0").unwrap()),
        Some(Action::TreeSelect(10))
    );
    assert_eq!(
        Bindings::defaults().get(Mode::Tree, &crate::config::parse_key("Alt-b").unwrap()),
        Some(Action::TreeSelect(11))
    );
    assert_eq!(
        Bindings::defaults().get(Mode::Tree, &crate::config::parse_key("Alt-z").unwrap()),
        Some(Action::TreeSelect(35))
    );
    assert_eq!(tree_shortcut(9).as_deref(), Some("0"));
    assert_eq!(tree_shortcut(10).as_deref(), Some("M-b"));
    assert_eq!(tree_shortcut(27), None);
    assert_eq!(tree_shortcut(34).as_deref(), Some("M-z"));
}

#[test]
fn terminal_keys_preserve_alt_and_modified_arrows() {
    assert_eq!(
        terminal_key_bytes(&crate::config::parse_key("Alt-x").unwrap(), false),
        b"\x1bx"
    );
    assert_eq!(
        terminal_key_bytes(&crate::config::parse_key("Ctrl-Left").unwrap(), false),
        b"\x1b[1;5D"
    );
}

#[test]
fn zle_cursor_save_restore_keeps_the_entered_command() {
    let mut parser = vt100::Parser::new(4, 40, 0);
    let mut prefix = Vec::new();
    process_terminal_bytes(
        &mut parser,
        &mut prefix,
        b"header\r\n\xe2\x9d\xaf echo kept\x1b[",
    );
    process_terminal_bytes(&mut parser, &mut prefix, b"s\x1b[1A\x1b[30Gtime\x1b");
    process_terminal_bytes(&mut parser, &mut prefix, b"[u\r\r\noutput");
    let rows: Vec<_> = parser.screen().rows(0, 40).collect();
    assert!(rows[1].contains("❯ echo kept"));
    assert_eq!(rows[2], "output");
    assert!(prefix.is_empty());
}

#[test]
fn terminal_queries_receive_chunk_safe_color_and_cursor_responses() {
    let colors = TerminalColors::from(&Theme::default());
    let mut prefix = Vec::new();
    assert!(terminal_query_responses(&mut prefix, b"\x1b]11;", (2, 4), colors).is_empty());
    let responses = terminal_query_responses(
        &mut prefix,
        b"?\x1b\\\x1b]10;?\x07\x1b[5n\x1b[6n",
        (2, 4),
        colors,
    );
    assert_eq!(
        responses,
        b"\x1b]11;rgb:2424/1e1e/2d2d\x1b\\\x1b]10;rgb:ecec/e7e7/f2f2\x1b\\\x1b[0n\x1b[3;5R"
    );
    assert!(prefix.is_empty());
}

#[test]
fn color_queries_follow_the_theme() {
    let theme = Theme {
        bar_label_foreground: (0x01, 0x02, 0x03),
        ..Theme::default()
    };
    let mut prefix = Vec::new();
    let responses = terminal_query_responses(
        &mut prefix,
        b"\x1b]11;?\x1b\\",
        (0, 0),
        TerminalColors::from(&theme),
    );
    assert_eq!(responses, b"\x1b]11;rgb:0101/0202/0303\x1b\\");
}

#[test]
fn leader_defaults_cover_session_and_pane_commands() {
    let bindings = Bindings::defaults();
    assert_eq!(
        bindings.get(Mode::Normal, &crate::config::parse_key("Alt-a").unwrap()),
        Some(Action::EnterLeader)
    );
    assert_eq!(
        bindings.get(Mode::Leader, &crate::config::parse_key("$").unwrap()),
        Some(Action::RenameSession)
    );
    assert_eq!(
        bindings.get(Mode::Leader, &crate::config::parse_key("-").unwrap()),
        Some(Action::SplitHorizontal)
    );
    assert_eq!(
        bindings.get(Mode::Leader, &crate::config::parse_key("|").unwrap()),
        Some(Action::SplitVertical)
    );
    assert_eq!(
        bindings.get(Mode::Leader, &crate::config::parse_key("d").unwrap()),
        Some(Action::Detach)
    );
    assert_eq!(
        bindings.get(Mode::Leader, &crate::config::parse_key("b").unwrap()),
        Some(Action::JumpToBell)
    );
    assert_eq!(
        bindings.get(Mode::Leader, &crate::config::parse_key("x").unwrap()),
        Some(Action::KillPane)
    );
    assert_eq!(
        bindings.get(Mode::Tree, &crate::config::parse_key("x").unwrap()),
        Some(Action::KillSession)
    );
    assert_eq!(
        bindings.get(Mode::Tree, &crate::config::parse_key("Alt-a").unwrap()),
        Some(Action::EnterLeader)
    );
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
    assert_eq!(rename.text, "wörk");
    rename.delete();
    assert_eq!(rename.text, "wök");
}

#[test]
fn rename_input_supports_terminal_style_line_editing() {
    let mut rename = RenameState {
        target: RenameTarget::Session { session_id: 1 },
        text: "one  twö three".into(),
        cursor: 9,
    };
    rename.delete_word_before_cursor();
    assert_eq!(rename.text, "one  three");
    assert_eq!(rename.cursor, 5);
    rename.delete_before_cursor();
    assert_eq!(rename.text, "three");
    assert_eq!(rename.cursor, 0);
    rename.cursor = 2;
    rename.delete_after_cursor();
    assert_eq!(rename.text, "th");
}

#[test]
fn disabled_history_stays_empty_as_terminal_output_arrives() {
    let mut parser = vt100::Parser::new(2, 20, SCROLLBACK_LINES);
    parser.process(b"one\r\ntwo\r\nthree\r\nfour");
    assert!(parser.screen().history_bytes() > 0);
    parser.screen_mut().clear_history();
    parser.screen_mut().set_history_limit(0);
    parser.process(b"\r\nfive\r\nsix");
    parser.screen_mut().set_scrollback(usize::MAX);
    assert_eq!(parser.screen().scrollback(), 0);
    assert_eq!(parser.screen().history_bytes(), 0);
}

#[test]
fn popup_text_scrolls_to_keep_the_cursor_visible() {
    assert_eq!(
        popup_text_window("rename session: abcdef", Some(22), 8),
        (" abcdef".into(), Some(7))
    );
    let output = repainted(9, 30, |frame| {
        render_popup_box(
            frame,
            (9, 30),
            PopupAnchor::Center,
            &Popup::Status("leader".into()),
            &Theme::default(),
        )
    });
    assert!(output.contains("╭────────╮"), "{output:?}");
    assert!(output.contains("leader"), "{output:?}");
    assert!(output.contains("╰────────╯"), "{output:?}");
    // Three rows tall, in the middle of nine.
    assert!(output.contains("\x1b[4;11H"), "{output:?}");
    assert!(output.contains("\x1b[6;11H╰"), "{output:?}");

    let output = painted(9, 40, |frame| {
        render_popup_box(
            frame,
            (9, 40),
            PopupAnchor::Center,
            &Popup::Rename {
                text: "rename session: smoke".into(),
                cursor: 21,
                shape: CursorShape::Block,
            },
            &Theme::default(),
        )
    });
    assert!(output.contains("rename session: smoke"), "{output:?}");
    // The rename cursor is placed on the character it will insert before.
    assert!(output.contains("\x1b[2 q"), "{output:?}");
}

#[test]
fn a_reporting_popup_sits_on_the_bottom_rows_and_stays_centred_across() {
    let output = repainted(9, 30, |frame| {
        render_popup_box(
            frame,
            (9, 30),
            PopupAnchor::Bottom,
            &Popup::Status("leader".into()),
            &Theme::default(),
        )
    });
    // Flush with the last row, and the same column as when centred.
    assert!(output.contains("\x1b[7;11H"), "{output:?}");
    assert!(output.contains("\x1b[9;11H╰"), "{output:?}");
    assert!(output.contains("leader"), "{output:?}");

    // Too short for a border: the text alone takes the last row.
    let output = repainted(2, 4, |frame| {
        render_popup_box(
            frame,
            (2, 4),
            PopupAnchor::Bottom,
            &Popup::Status("yanked".into()),
            &Theme::default(),
        )
    });
    assert!(output.contains("\x1b[2;1H"), "{output:?}");
    assert!(output.contains("yank"), "{output:?}");
}

#[test]
fn a_popup_without_an_input_field_leaves_the_pane_cursor_alone() {
    let behind = FrameCursor {
        row: 3,
        col: 8,
        shape: CursorShape::Bar,
        visible: true,
    };
    // A message and a question both draw over the pane without taking its
    // cursor, so typing continues where it left off.
    for popup in [
        Popup::Status("yanked".into()),
        Popup::Warning("kill pane 1?".into()),
    ] {
        let output = repainted(9, 30, |frame| {
            frame.set_cursor(behind);
            render_popup_box(
                frame,
                (9, 30),
                PopupAnchor::Center,
                &popup,
                &Theme::default(),
            );
        });
        assert!(output.contains("\x1b[3;8H"), "{output:?}");
        assert!(output.contains("\x1b[6 q"), "{output:?}");
        assert!(output.contains("\x1b[?25h"), "{output:?}");
    }

    // A rename field does take it, because that is where the typing goes.
    let output = repainted(9, 40, |frame| {
        frame.set_cursor(behind);
        render_popup_box(
            frame,
            (9, 40),
            PopupAnchor::Center,
            &Popup::Rename {
                text: "rename session: smoke".into(),
                cursor: 21,
                shape: CursorShape::Block,
            },
            &Theme::default(),
        );
    });
    assert!(!output.contains("\x1b[3;8H"), "{output:?}");
}

#[test]
fn a_preview_borrows_the_cursor_of_the_pane_it_shows() {
    let mut parser = vt100::Parser::new(4, 10, 0);
    parser.process(b"one\r\ntwo\r\nthree");
    let screen = parser.screen();
    // Row 2 of a three-row slice starting at row 1, column 6 of the pane.
    assert_eq!(
        preview_cursor(screen, CursorShape::Block, 1, rect(4, 20, 3, 10)),
        Some(FrameCursor {
            row: 5,
            col: 25,
            shape: CursorShape::Block,
            visible: true,
        })
    );
    // A cursor below the slice is pulled back into it rather than left to
    // land on the panel, and a narrow preview clamps the column too.
    assert_eq!(
        preview_cursor(screen, CursorShape::Block, 0, rect(4, 20, 2, 3)),
        Some(FrameCursor {
            row: 5,
            col: 22,
            shape: CursorShape::Block,
            visible: true,
        })
    );
    assert_eq!(
        preview_cursor(screen, CursorShape::Block, 0, rect(4, 20, 3, 0)),
        None
    );

    // A pane that hides its cursor still gives the preview a position.
    parser.process(b"\x1b[?25l");
    let hidden =
        preview_cursor(parser.screen(), CursorShape::Block, 1, rect(4, 20, 3, 10)).unwrap();
    assert_eq!((hidden.row, hidden.col), (5, 25));
    assert!(!hidden.visible);
}

#[test]
fn pane_layout_splits_collapses_and_finds_neighbors() {
    let mut layout = PaneLayout::Pane(1);
    assert!(layout.split(1, 2, SplitAxis::Vertical));
    assert!(layout.split(2, 3, SplitAxis::Horizontal));
    let (regions, dividers) = pane_layout_regions(
        &layout,
        Rect {
            row: 0,
            col: 0,
            rows: 10,
            cols: 20,
        },
    );
    assert_eq!(
        regions,
        vec![
            (
                1,
                Rect {
                    row: 0,
                    col: 0,
                    rows: 10,
                    cols: 9,
                }
            ),
            (
                2,
                Rect {
                    row: 0,
                    col: 10,
                    rows: 4,
                    cols: 10,
                }
            ),
            (
                3,
                Rect {
                    row: 5,
                    col: 10,
                    rows: 5,
                    cols: 10,
                }
            ),
        ]
    );
    assert_eq!(dividers.len(), 2);
    assert_eq!(
        neighboring_pane(&regions, 1, None, PaneDirection::Right),
        Some(3)
    );
    assert_eq!(
        neighboring_pane(&regions, 2, None, PaneDirection::Down),
        Some(3)
    );
    assert_eq!(
        neighboring_pane(&regions, 3, None, PaneDirection::Left),
        Some(1)
    );

    let collapsed = layout.without(2).unwrap();
    let (regions, _) = pane_layout_regions(
        &collapsed,
        Rect {
            row: 0,
            col: 0,
            rows: 10,
            cols: 20,
        },
    );
    assert_eq!(
        regions.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        vec![1, 3]
    );
}

#[test]
fn pane_navigation_returns_to_the_previous_branch_on_a_tie() {
    let layout = PaneLayout::Split {
        axis: SplitAxis::Horizontal,
        ratio: EVEN_SPLIT,
        first: Box::new(PaneLayout::Pane(1)),
        second: Box::new(PaneLayout::Split {
            axis: SplitAxis::Vertical,
            ratio: EVEN_SPLIT,
            first: Box::new(PaneLayout::Pane(2)),
            second: Box::new(PaneLayout::Pane(3)),
        }),
    };
    let (regions, _) = pane_layout_regions(
        &layout,
        Rect {
            row: 0,
            col: 0,
            rows: 24,
            cols: 75,
        },
    );
    assert_eq!(
        neighboring_pane(&regions, 3, None, PaneDirection::Up),
        Some(1)
    );
    assert_eq!(
        neighboring_pane(&regions, 1, Some(3), PaneDirection::Down),
        Some(3)
    );
    assert_eq!(
        neighboring_pane(&regions, 1, Some(2), PaneDirection::Down),
        Some(2)
    );
}

#[test]
fn session_preview_grid_shows_every_window() {
    let three = preview_grid_rects(
        3,
        Rect {
            row: 4,
            col: 20,
            rows: 20,
            cols: 60,
        },
    );
    assert_eq!(three.len(), 3);
    assert!(three.iter().all(|rect| rect.rows == 20 && rect.cols > 0));
    assert!(three.windows(2).all(|pair| pair[0].col < pair[1].col));
    let (vertical, horizontal) = preview_grid_separator_positions(
        Rect {
            row: 4,
            col: 20,
            rows: 20,
            cols: 60,
        },
        &three,
    );
    assert_eq!(vertical.len(), 2);
    assert!(horizontal.is_empty());

    let four = preview_grid_rects(
        4,
        Rect {
            row: 4,
            col: 20,
            rows: 20,
            cols: 60,
        },
    );
    assert_eq!(four.len(), 4);
    assert_eq!(four[0].row, four[1].row);
    assert!(four[2].row > four[0].row);
    let (vertical, horizontal) = preview_grid_separator_positions(
        Rect {
            row: 4,
            col: 20,
            rows: 20,
            cols: 60,
        },
        &four,
    );
    assert_eq!(vertical.len(), 1);
    assert_eq!(horizontal.len(), 1);
}

#[test]
fn bar_width_and_vertical_center_follow_window_count() {
    assert_eq!(automatic_session_name(1), "Session 1");
    assert_eq!(bar_width(1), 5);
    assert_eq!(bar_width(9), 5);
    assert_eq!(bar_width(10), 6);
    assert_eq!(bar_width(100), 7);
    assert_eq!(bar_label(1, 1), " 1 ");
    assert_eq!(bar_label(10, 2), " 10 ");
    assert_eq!(bar_window_label(0, 0, 1, false), " • ");
    assert_eq!(bar_window_label(0, 0, 1, true), " ▣ ");
    assert_eq!(bar_window_label(1, 0, 1, false), " 2 ");
    assert_eq!(bar_window_label(9, 0, 2, false), " 10 ");
    assert_eq!(
        blend_rgb((110, 88, 113), (203, 163, 210), 80),
        (139, 111, 143)
    );
    assert_eq!(centered_bar_layout(1, 0, 9), (0, 3, 1));
    assert_eq!(centered_bar_layout(3, 1, 9), (0, 0, 3));
    assert_eq!(centered_bar_layout(12, 10, 5), (10, 1, 1));
    let icon = |command: &str| process_group_icon(&[command.to_owned()]);
    assert_eq!(icon("zsh"), "❯");
    assert_eq!(icon("nvim"), "\u{e01f}\u{e020}\u{e021}");
    assert_eq!(icon("nvim README.md"), "\u{e01f}\u{e020}\u{e021}");
    assert_eq!(icon("ssh server.example"), "\u{e022}\u{e023}\u{e024}");
    assert_eq!(icon("cargo test"), "\u{e025}\u{e026}\u{e027}");
    assert_eq!(icon("rustc src/main.rs"), "\u{e025}\u{e026}\u{e027}");
    assert_eq!(icon("python script.py"), "\u{e028}\u{e029}\u{e02a}");
    assert_eq!(icon("python3.13 -m pytest"), "\u{e028}\u{e029}\u{e02a}");
    assert_eq!(icon("jj"), "");
    assert_eq!(icon("codex"), "\u{e015}\u{e016}\u{e017}");
    assert_eq!(icon("claude"), "\u{e012}\u{e013}\u{e014}");
    assert_eq!(icon("/bin/bash -l"), "$");
    assert_eq!(icon("nix build .#mux"), "\u{e019}\u{e01a}\u{e01b}");
    assert_eq!(icon("nixos-rebuild switch"), "\u{e019}\u{e01a}\u{e01b}");
    assert_eq!(icon("nh os switch"), "\u{e019}\u{e01a}\u{e01b}");
    assert_eq!(icon("direnv export zsh"), "\u{e019}\u{e01a}\u{e01b}");
    assert_eq!(icon("watch -n 1 jj log"), "\u{e01c}\u{e01d}\u{e01e}");
    // A store path is not a nix invocation, and the shell it runs still wins.
    assert_eq!(icon("/nix/store/abc-zsh-5.9/bin/zsh"), "❯");
    assert_eq!(process_group_icon(&[]), "·");
    // A shell leading a group loses to whatever it started.
    assert_eq!(
        process_group_icon(&[
            "bash /Users/me/nix/scripts/cli/switch".to_owned(),
            "nix build --no-link".to_owned(),
        ]),
        "\u{e019}\u{e01a}\u{e01b}"
    );
    assert_eq!(
        process_group_icon(&["-zsh".to_owned(), "direnv export zsh".to_owned()]),
        "\u{e019}\u{e01a}\u{e01b}"
    );
    assert_eq!(tree_panel_width(76), 25);
    assert_eq!(tree_panel_width(30), 15);

    let separator = painted(3, 4, |frame| {
        render_bar_separator(
            frame,
            3,
            bar_width(1),
            Some(2),
            Theme::default().bar_active,
            None,
        )
    });
    assert!(separator.contains("38;2;203;163;210"), "{separator:?}");
    assert!(separator.contains(""), "{separator:?}");
    assert!(separator.contains(""), "{separator:?}");
    assert!(separator.contains(""), "{separator:?}");
    assert!(!separator.contains("48;2"), "{separator:?}");

    let theme = Theme::default();
    let vim_separator = painted(1, bar_width(1), |frame| {
        render_bar_separator(
            frame,
            1,
            bar_width(1),
            Some(1),
            theme.bar_active,
            Some(theme.bar_vim_background),
        )
    });
    assert!(
        vim_separator.contains(&format!(
            "38;2;{};{};{};48;2;{};{};{}",
            theme.bar_active.0,
            theme.bar_active.1,
            theme.bar_active.2,
            theme.bar_vim_background.0,
            theme.bar_vim_background.1,
            theme.bar_vim_background.2,
        )),
        "{vim_separator:?}"
    );
}

#[test]
fn terminal_bells_ignore_osc_terminators_and_render_truecolor_shimmer() {
    let mut parser = vt100::Parser::new_with_callbacks(2, 8, 0, TerminalCallbacks::default());
    parser.process(b"\x1b]2;window title\x07");
    assert_eq!(parser.callbacks().bell_count, 0);
    parser.process(b"\x07\x1bg");
    assert_eq!(parser.callbacks().bell_count, 2);

    let shimmering = |elapsed: u128| BellVisual {
        shimmer: Some(elapsed),
        slide: BellSlide::Covered,
    };
    let theme = Theme::default();
    let bar = ((40, 40, 40), theme.bar_label_foreground);
    let midway = shimmering(BELL_SHIMMER_MICROS / 2);
    let output = painted(1, 4, |frame| {
        render_bell_label(
            frame,
            (1, 1),
            " 1 ",
            BellLabel {
                visual: midway,
                animation_width: 4,
                resting: bar,
                bold: false,
            },
            &theme,
        )
    });
    let (red, green, blue) = bell_visual_colors(midway, 1, 4, &theme).0;
    assert!(
        output.contains(&format!("48;2;{red};{green};{blue}")),
        "{output:?}"
    );
    assert!(!output.contains("48;2;255;255;255"), "{output:?}");
    let resting = BellVisual {
        shimmer: None,
        slide: BellSlide::Covered,
    };
    let output = painted(1, 3, |frame| {
        render_bell_label(
            frame,
            (1, 1),
            " ! ",
            BellLabel {
                visual: resting,
                animation_width: 3,
                resting: bar,
                bold: false,
            },
            &theme,
        )
    });
    let (text, base) = (theme.bell_text, theme.bell_base);
    assert!(
        output.contains(&format!(
            "38;2;{};{};{};48;2;{};{};{}",
            text.0, text.1, text.2, base.0, base.1, base.2
        )),
        "{output:?}"
    );
    assert_eq!(
        output.chars().filter(|character| *character == '!').count(),
        1
    );

    const SAMPLE_MICROS: u128 = 1_000;
    let frames: Vec<[Rgb; 3]> = (0..BELL_SHIMMER_MICROS / SAMPLE_MICROS)
        .map(|frame| {
            std::array::from_fn(|cell| {
                bell_visual_colors(shimmering(frame * SAMPLE_MICROS), cell, 3, &theme).0
            })
        })
        .collect();
    // The highlight is a band that slides across the label: its brightest
    // cell walks from the left edge to the right one, and brightness falls
    // away on either side of it rather than fading everywhere at once.
    let brightest = |elapsed: u128, width: usize| -> usize {
        (0..width)
            .max_by_key(|cell| {
                bell_visual_colors(shimmering(elapsed), *cell, width, &theme)
                    .0
                    .1
            })
            .unwrap()
    };
    let peaks: Vec<usize> = (10..=90)
        .map(|percent| brightest(BELL_SHIMMER_MICROS * percent / 100, 20))
        .collect();
    assert!(peaks.windows(2).all(|pair| pair[0] <= pair[1]), "{peaks:?}");
    assert_eq!(peaks.first(), Some(&0), "{peaks:?}");
    assert_eq!(peaks.last(), Some(&19), "{peaks:?}");
    let entering = shimmering(BELL_SHIMMER_MICROS / 4);
    let leaving = shimmering(BELL_SHIMMER_MICROS * 3 / 4);
    let entering_colors: [Rgb; 20] =
        std::array::from_fn(|cell| bell_visual_colors(entering, cell, 20, &theme).0);
    let leaving_colors: [Rgb; 20] =
        std::array::from_fn(|cell| bell_visual_colors(leaving, cell, 20, &theme).0);
    assert!(entering_colors[0].1 > entering_colors[19].1);
    assert!(leaving_colors[19].1 > leaving_colors[0].1);
    for colors in [&entering_colors, &leaving_colors] {
        let peak = colors
            .iter()
            .enumerate()
            .max_by_key(|(_, color)| color.1)
            .unwrap()
            .0;
        assert!(
            colors[..=peak]
                .windows(2)
                .all(|pair| pair[0].1 <= pair[1].1)
        );
        assert!(colors[peak..].windows(2).all(|pair| pair[0].1 >= pair[1].1));
    }
    let tab_frames: Vec<[Rgb; 4]> = (0..BELL_SHIMMER_MICROS / SAMPLE_MICROS)
        .map(|sample| {
            std::array::from_fn(|cell| {
                bell_visual_colors(shimmering(sample * SAMPLE_MICROS), cell, 4, &theme).0
            })
        })
        .collect();
    assert!(tab_frames.windows(2).all(|pair| {
        pair[0].iter().zip(pair[1]).all(|(previous, current)| {
            previous.0.abs_diff(current.0) <= 2
                && previous.1.abs_diff(current.1) <= 2
                && previous.2.abs_diff(current.2) <= 2
        })
    }));
    assert!(frames.iter().copied().collect::<HashSet<_>>().len() > 80);
    assert!(frames.iter().flatten().all(|color| color.0 >= color.1));
    assert!(
        frames
            .iter()
            .flatten()
            .all(|color| { *color != (203, 163, 210) && *color != (110, 88, 113) })
    );
    assert!(frames.windows(2).all(|pair| {
        pair[0].iter().zip(pair[1]).all(|(previous, current)| {
            previous.0.abs_diff(current.0) <= 12
                && previous.1.abs_diff(current.1) <= 12
                && previous.2.abs_diff(current.2) <= 12
        })
    }));
    assert_eq!(
        bell_visual_colors(shimmering(0), 0, 3, &theme).0,
        theme.bell_base
    );
    assert_eq!(
        bell_visual_colors(shimmering(BELL_SHIMMER_MICROS - 1), 2, 3, &theme).0,
        theme.bell_base
    );

    // The bell colour moves onto the label and later off it, instead of
    // appearing and vanishing in place.
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
    assert!(arriving.windows(2).all(|pair| pair[0] >= pair[1]));
    assert!(arriving[0] > arriving[4]);
    let leaving = coverage(BellSlide::Out(128));
    assert!(leaving.windows(2).all(|pair| pair[0] <= pair[1]));
    assert!(leaving[4] > leaving[0]);
    // Each step of the slide moves it a little, never a whole cell at once.
    let steps: Vec<[u16; 5]> = (0..=255)
        .map(|step| coverage(BellSlide::In(step)))
        .collect();
    assert!(steps.windows(2).all(|pair| {
        pair[0]
            .iter()
            .zip(pair[1])
            .all(|(previous, current)| current >= *previous && current - previous <= 24)
    }));
    // A slide blends against the label's own colours, so nothing jumps when
    // the bell arrives or is cleared.
    let bar_background = ((40, 40, 40), theme.bar_label_foreground);
    assert_eq!(
        bell_cell_colors(
            BellVisual {
                shimmer: None,
                slide: BellSlide::In(0),
            },
            0,
            5,
            bar_background,
            &theme,
        ),
        bar_background
    );
    assert_eq!(
        bell_cell_colors(
            BellVisual {
                shimmer: None,
                slide: BellSlide::Out(0),
            },
            4,
            5,
            bar_background,
            &theme,
        ),
        bar_background
    );
    assert_eq!(bell_render_token(0, true), 0);
    assert_eq!(bell_render_token(1_000, true), 1_000);
    assert_eq!(
        bell_render_token(BELL_SHIMMER_MICROS, true),
        BELL_SHIMMER_MICROS as u64
    );
    assert_eq!(
        bell_render_token(BELL_SHIMMER_MICROS + 400_000, true),
        BELL_SHIMMER_MICROS as u64
    );
    assert_eq!(
        bell_render_token(BELL_SHIMMER_MICROS + BELL_BREAK_MICROS, true),
        BELL_SHIMMER_MICROS as u64 + 1
    );
    assert_eq!(
        bell_render_token(BELL_SHIMMER_MICROS * 2, false),
        BELL_SHIMMER_MICROS as u64
    );
    assert_eq!(other_session_bell_label(1), " ! ");
    assert_eq!(other_session_bell_label(2), " 2 ");
}

#[test]
fn application_cursor_shapes_are_preserved_without_blinking() {
    let mut parser = vt100::Parser::new_with_callbacks(2, 8, 0, TerminalCallbacks::default());

    parser.process(b"\x1b[5 q");
    assert_eq!(parser.callbacks().cursor_shape, CursorShape::Bar);
    parser.process(b"\x1b[3 q");
    assert_eq!(parser.callbacks().cursor_shape, CursorShape::Underline);
    parser.process(b"\x1b[1 q");
    assert_eq!(parser.callbacks().cursor_shape, CursorShape::Block);
    parser.process(b"\x1b]50;CursorShape=1\x07");
    assert_eq!(parser.callbacks().cursor_shape, CursorShape::Bar);

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
fn visiting_a_bell_starts_one_complete_active_pass() {
    let appeared = Instant::now() - Duration::from_secs(10);
    let mut bell = Some(BellState {
        appeared,
        started: Instant::now() - Duration::from_secs(10),
        render_token: 42,
        count: 3,
        repeat: true,
        pane_id: 7,
    });
    play_bell_once(&mut bell, true);
    let bell = bell.as_ref().unwrap();
    assert!(!bell.repeat);
    assert_eq!(bell.render_token, 0);
    assert_eq!(bell.count, 3);
    assert_eq!(bell.pane_id, 7);
    assert!(bell.started.elapsed() < Duration::from_millis(50));
    // Replaying keeps the original arrival, so the colour does not slide in
    // again over a label it already covers.
    assert_eq!(bell.appeared, appeared);
    let visual = bell_visual(bell, BellStyle::Shimmer).unwrap();
    assert!(matches!(visual.slide, BellSlide::Covered));
}

#[test]
fn a_bell_without_a_shimmer_rests_bright_or_is_not_drawn_at_all() {
    let theme = Theme::default();
    let bell = BellState {
        appeared: Instant::now(),
        started: Instant::now(),
        render_token: 0,
        count: 1,
        repeat: true,
        pane_id: 7,
    };

    // Steady is the resting end of the sweep: covered, bright, and the same
    // in every cell, so nothing about it changes between frames.
    let steady = bell_visual(&bell, BellStyle::Steady).unwrap();
    assert!(steady.shimmer.is_none());
    assert!(matches!(steady.slide, BellSlide::Covered));
    let colors: Vec<_> = (0..3)
        .map(|cell| bell_visual_colors(steady, cell, 3, &theme))
        .collect();
    assert!(colors.iter().all(|colors| *colors == colors_of(&theme)));

    assert!(bell_visual(&bell, BellStyle::None).is_none());

    // With nobody watching a shimmer, a visited bell is done immediately
    // rather than waiting out a pass that is never drawn.
    let mut pending = Some(bell);
    play_bell_once(&mut pending, false);
    assert!(pending.is_none());
}

fn colors_of(theme: &Theme) -> (Rgb, Rgb) {
    (theme.bell_base, theme.bell_text)
}

#[test]
fn a_private_directory_is_created_and_a_shared_one_is_refused() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root = std::env::temp_dir().join(format!("mux-private-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
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
    let directory = std::env::temp_dir().join(format!("mux-lock-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let persistence = Persistence {
        state_file: directory.join("state.bin"),
        directory: directory.clone(),
    };
    let held = persistence.lock().unwrap();
    assert!(held.is_some(), "the first daemon takes the lock");
    assert!(
        persistence.lock().unwrap().is_none(),
        "a second daemon backs off instead of sharing the state"
    );
    drop(held);
    assert!(
        persistence.lock().unwrap().is_some(),
        "the lock is free again once the daemon exits"
    );
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn zsh_startup_keeps_mux_visible_while_sourcing_zshrc() {
    assert!(ZSHRC_WRAPPER.contains("source \"$ZDOTDIR/.zshrc\""));
    assert!(!ZSHRC_WRAPPER.contains("TMUX="));
    assert!(ZSHRC_WRAPPER.contains("unset MUX_ORIGINAL_ZDOTDIR"));
}

#[test]
fn idle_prompt_markers_restore_the_screen_before_the_prompt() {
    let mut parser = vt100::Parser::new_with_callbacks(6, 40, 0, TerminalCallbacks::default());
    parser.process(
        b"output\r\n\x1b]777;mux-prompt-start\x1b\\header\r\n> \x1b[?2004h\x1b]777;mux-prompt-ready\x1b\\",
    );
    let correction = restored_prompt_correction(&parser).unwrap();
    parser.process(&correction);
    assert!(parser.screen().contents().contains("output"));
    assert!(!parser.screen().contents().contains("header"));
    assert!(!parser.screen().contents().contains(">"));
}

#[test]
fn entered_prompt_text_prevents_prompt_replacement() {
    let mut parser = vt100::Parser::new_with_callbacks(4, 40, 0, TerminalCallbacks::default());
    parser.process(
        b"\x1b]777;mux-prompt-start\x1b\\> \x1b[?2004h\x1b]777;mux-prompt-ready\x1b\\typed",
    );
    assert!(restored_prompt_correction(&parser).is_none());
}

#[test]
fn tree_starts_folded_and_arrows_control_it() {
    let tree = TreeState::folded(2);
    assert!(tree.expanded.is_empty());
    assert_eq!(tree.selected, 2);
    let bindings = Bindings::defaults();
    assert_eq!(
        bindings.get(Mode::Tree, &crate::config::parse_key("Up").unwrap()),
        Some(Action::TreeUp)
    );
    assert_eq!(
        bindings.get(Mode::Tree, &crate::config::parse_key("Down").unwrap()),
        Some(Action::TreeDown)
    );
    assert_eq!(
        bindings.get(Mode::Tree, &crate::config::parse_key("Left").unwrap()),
        Some(Action::TreeCollapse)
    );
    assert_eq!(
        bindings.get(Mode::Tree, &crate::config::parse_key("Right").unwrap()),
        Some(Action::TreeExpand)
    );
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
fn vim_snapshot_preserves_terminal_formatting() {
    let mut parser = vt100::Parser::new(2, 12, 0);
    parser.process(b"\x1b[1;3;4;31;44mstyled\x1b[0m plain");
    let line = snapshot_vim_line(parser.screen(), 0, 12);
    assert_eq!(line.text, "styled plain");

    let attributes = line.cells[0].attributes;
    assert_eq!(attributes.foreground, vt100::Color::Idx(1));
    assert_eq!(attributes.background, vt100::Color::Idx(4));
    assert!(attributes.bold);
    assert!(attributes.italic);
    assert!(attributes.underline);

    let theme = Theme::default();
    let selected = vim_selected_cell_attributes(attributes, &theme);
    assert_eq!(selected.foreground, attributes.foreground);
    assert_eq!(selected.bold, attributes.bold);
    assert_eq!(selected.italic, attributes.italic);
    assert_eq!(selected.underline, attributes.underline);
    assert_eq!(selected.background, vt100::Color::Rgb(63, 53, 82));
}

#[test]
fn vim_render_is_confined_to_the_active_pane_region() {
    let mut parser = vt100::Parser::new(2, 12, 0);
    parser.process(b"first\r\nsecond");
    let (lines, cursor) = snapshot_screen(parser.screen_mut());
    let text = lines.iter().map(|line| line.text.clone()).collect();
    let state = VimState {
        mode: VimMode::new(text, cursor, 2),
        lines,
    };
    let region = Rect {
        row: 3,
        col: 5,
        rows: 2,
        cols: 8,
    };
    let theme = Theme::default();
    let rendered = repainted(8, 20, |frame| {
        Server::render_vim_region(&state, frame, region, 4, true, &theme)
    });
    assert!(rendered.contains("first"), "{rendered:?}");
    assert!(rendered.contains("second"), "{rendered:?}");
    // Row 4, column 10 is the region's first cell past the strip.
    assert!(rendered.contains("\x1b[4;10H"), "{rendered:?}");
    assert!(!rendered.contains("\x1b[1;"), "{rendered:?}");
    assert!(rendered.contains("\x1b[?25h"), "{rendered:?}");

    let inactive = repainted(8, 20, |frame| {
        Server::render_vim_region(&state, frame, region, 4, false, &theme)
    });
    assert!(inactive.contains("first"), "{inactive:?}");
    assert!(inactive.contains("second"), "{inactive:?}");
    assert!(!inactive.contains("\x1b[?25h"), "{inactive:?}");
}

#[test]
fn vim_render_paints_full_multi_key_jump_hints() {
    let mut parser = vt100::Parser::new(1, 30, 0);
    parser.process("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".as_bytes());
    let (lines, _) = snapshot_screen(parser.screen_mut());
    let text = lines.iter().map(|line| line.text.clone()).collect();
    let mut mode = VimMode::new(text, Position { row: 0, col: 0 }, 1);
    let bindings = Bindings::defaults();
    for name in [" ", "a"] {
        let key = crate::protocol::parse_for_test(name);
        mode.handle(bindings.get(Mode::Vim, &key), &key);
    }
    let state = VimState { mode, lines };
    let rendered = repainted(1, 30, |frame| {
        Server::render_vim_region(
            &state,
            frame,
            Rect {
                row: 0,
                col: 0,
                rows: 1,
                cols: 30,
            },
            0,
            true,
            &Theme::default(),
        )
    });
    assert!(rendered.contains("ja"), "{rendered:?}");
}

#[test]
fn a_status_message_disappears_on_its_own() {
    let message = StatusMessage::new("yanked 12 bytes".into());
    assert!(!message.expired(Instant::now()));
    assert!(!message.expired(Instant::now() + MESSAGE_DURATION / 2));
    assert!(message.expired(Instant::now() + MESSAGE_DURATION));
}

fn sample_picker(selected: usize, in_use: Option<usize>) -> ThemePicker {
    let dusk = Theme {
        bar_active: (0xcb, 0xa3, 0xd2),
        ..Theme::default()
    };
    let light = Theme {
        bar_label_foreground: (0xff, 0xff, 0xff),
        popup_text: (0x00, 0x00, 0x00),
        panel_background: (0xe8, 0xe8, 0xe8),
        popup_warning: (0x66, 0x44, 0x00),
        ..Theme::default()
    };
    ThemePicker {
        entries: vec![
            ThemeEntry {
                name: "dark".into(),
                theme: Theme::default(),
            },
            ThemeEntry {
                name: "dusk".into(),
                theme: dusk,
            },
            ThemeEntry {
                name: "light".into(),
                theme: light,
            },
        ],
        selected,
        in_use,
    }
}

/// The characters of a painted picker, one string per row, which is what
/// the layout amounts to once the colours are taken away.
fn picker_rows(picker: &ThemePicker, rows: u16, cols: u16) -> Vec<String> {
    let painted = painted(rows, cols, |frame| {
        render_theme_picker(picker, frame, rows, cols)
    });
    let mut parser = vt100::Parser::new(rows, cols, 0);
    parser.process(painted.as_bytes());
    let screen = parser.screen();
    (0..rows)
        .map(|row| {
            (0..cols)
                .map(|col| match screen.cell(row, col) {
                    Some(cell) if cell.has_contents() => cell.contents().to_string(),
                    _ => " ".to_string(),
                })
                .collect::<Vec<_>>()
                .concat()
                .trim_end()
                .to_string()
        })
        .collect()
}

#[test]
fn the_theme_picker_opens_with_alt_c_and_is_driven_like_a_list() {
    let bindings = Bindings::defaults();
    assert_eq!(
        bindings.get(Mode::Normal, &crate::config::parse_key("Alt-c").unwrap()),
        Some(Action::ThemePicker)
    );
    assert_eq!(
        bindings.get(Mode::Leader, &crate::config::parse_key("c").unwrap()),
        None
    );
    for (key, action) in [
        ("l", Action::ThemeNext),
        ("Right", Action::ThemeNext),
        ("j", Action::ThemeNext),
        ("h", Action::ThemePrevious),
        ("Left", Action::ThemePrevious),
        ("k", Action::ThemePrevious),
        ("Enter", Action::ThemeChoose),
        ("Escape", Action::ThemeCancel),
        ("q", Action::ThemeCancel),
        ("2", Action::ThemeSelect(2)),
    ] {
        assert_eq!(
            bindings.get(Mode::Theme, &crate::config::parse_key(key).unwrap()),
            Some(action),
            "{key}"
        );
    }
    // Nothing that would act on a session reaches the keyboard while the
    // picker owns the screen.
    assert_eq!(
        bindings.get(Mode::Theme, &crate::config::parse_key("Alt-t").unwrap()),
        None
    );
}

#[test]
fn the_theme_picker_lists_every_theme_and_shows_the_palette_of_one() {
    let picker = sample_picker(1, Some(2));
    let rows = picker_rows(&picker, 30, 110);
    let screen = rows.join("\n");
    // A card, not a screen: it is centred, and the rows outside it are
    // whatever was on screen before, which is nothing in this test.
    assert!(rows[0].is_empty(), "{screen}");
    assert!(
        screen.contains("╭") && screen.contains("themes"),
        "{screen}"
    );
    // Every theme is offered along the top, numbered, with the one in use
    // marked and the highlighted one previewed underneath.
    assert!(screen.contains("1 dark"), "{screen}");
    assert!(screen.contains("2 dusk"), "{screen}");
    assert!(screen.contains("● 3 light"), "{screen}");
    // Every colour from the source palette is an explicit swatch, using the
    // exact role name from palettes.nix and nothing about mux.
    for color in [
        "background",
        "foreground",
        "surface",
        "surfaceRaised",
        "muted",
        "accent",
        "secondary",
        "success",
        "warning",
        "danger",
        "selection",
        "diffAdd",
        "diffDelete",
        "diffChange",
    ] {
        assert!(screen.contains(color), "{color} missing from {screen}");
    }
    assert!(!screen.contains("cargo"), "{screen}");
    assert!(screen.contains("←→ browse"), "{screen}");
}

#[test]
fn the_theme_picker_paints_itself_in_the_theme_it_is_offering() {
    // The light theme is highlighted while a dark one is in use, which is
    // the case where showing the theme in use would be wrong.
    let picker = sample_picker(2, Some(0));
    let (rows, cols) = (30, 110);
    let painted = painted(rows, cols, |frame| {
        render_theme_picker(&picker, frame, rows, cols)
    });
    let mut parser = vt100::Parser::new(rows, cols, 0);
    parser.process(painted.as_bytes());
    let screen = parser.screen();
    let light = &picker.entries[2].theme;

    // The card still previews the theme around the explicit swatches.
    let card = theme_card(rows, cols, &["dark", "dusk", "light"], &light.palette);
    let border = screen.cell(card.top - 1, card.left).unwrap();
    assert_eq!(border.bgcolor(), rgb(light.panel_background));
    assert_eq!(border.fgcolor(), rgb(light.palette.surface_raised));

    // Every role is a filled swatch labelled in whichever of the theme's
    // shades can be read on it.
    let (row, col) = (0..rows)
        .flat_map(|row| (0..cols).map(move |col| (row, col)))
        .find(|(row, col)| {
            screen
                .cell(*row, *col)
                .is_some_and(|cell| cell.bgcolor() == rgb(light.palette.diff_add))
        })
        .expect("a swatch of the diffAdd colour");
    assert_eq!(
        screen.cell(row, col).unwrap().fgcolor(),
        rgb(contrasting_shade(
            light.palette.diff_add,
            light.palette.foreground,
            light.palette.background
        ))
    );
    assert!(
        (0..rows)
            .flat_map(|row| (0..cols).map(move |col| (row, col)))
            .any(|(row, col)| screen
                .cell(row, col)
                .is_some_and(|cell| cell.bgcolor() == rgb(light.palette.accent)))
    );
}

#[test]
fn the_theme_picker_fits_whatever_terminal_it_is_opened_in() {
    // Nothing here is worth showing at the smallest sizes, but a client
    // still gets a frame rather than a panicking daemon, and the card never
    // paints outside the screen.
    for (rows, cols) in [(1, 1), (2, 6), (5, 20), (10, 34), (16, 50), (24, 80)] {
        let picker = sample_picker(1, Some(2));
        let painted = picker_rows(&picker, rows, cols);
        assert_eq!(painted.len(), rows as usize);
        assert!(
            painted
                .iter()
                .all(|row| row.chars().count() <= cols as usize)
        );
    }
    // Given room, the card is centred and leaves the screen around it.
    let picker = sample_picker(0, None);
    let painted = picker_rows(&picker, 24, 80);
    assert!(painted[0].is_empty() && painted[23].is_empty());
    assert!(painted.iter().any(|row| row.contains("╭")));
}

#[test]
fn theme_tabs_wrap_onto_as_many_rows_as_they_need() {
    let names = ["dark", "dusk", "light"];
    assert_eq!(theme_tab_rows(&names, 60), vec![vec![0, 1, 2]]);
    // 13 + 13 + 14 cells: the third tab starts a second row.
    assert_eq!(theme_tab_rows(&names, 30), vec![vec![0, 1], vec![2]]);
    // A tab wider than the card still gets a row of its own.
    assert_eq!(theme_tab_rows(&names, 4), vec![vec![0], vec![1], vec![2]]);
}

#[test]
fn a_swatch_is_never_labelled_in_its_own_colour() {
    let page = (0xff, 0xff, 0xff);
    let ink = (0x00, 0x00, 0x00);
    assert_eq!(contrasting_shade((0x11, 0x11, 0x11), page, ink), page);
    assert_eq!(contrasting_shade(page, page, ink), ink);
    assert_eq!(contrasting_shade((0xf8, 0xf8, 0xf8), page, ink), ink);
}

#[test]
fn themes_are_read_from_their_directories_and_sorted() {
    let root = std::env::temp_dir().join(format!("mux-themes-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let themes = root.join("themes");
    for (name, color) in [("dusk", "#cba3d2"), ("dark", "#6e5871")] {
        fs::create_dir_all(themes.join(name)).unwrap();
        fs::write(
            themes.join(name).join(THEME_FILE),
            format!("[palette]\nsecondary = \"{color}\"\n"),
        )
        .unwrap();
    }
    // Neither a directory without a theme file nor an unparsable one is
    // offered, and neither stops the rest from being.
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
    replay_pane_journal(&mut restored, &mut Vec::new(), records.as_slice()).unwrap();
    assert_eq!(restored.screen().size(), (4, 20));
    assert_eq!(restored.screen().contents(), before);

    // Scrollback survives, and so does the formatting inside it.
    let (lines, _) = snapshot_screen(restored.screen_mut());
    assert!(lines.iter().any(|line| line.text.contains("line 000")));
    assert!(lines.iter().any(|line| line.text.contains("line 199")));
    let tail = lines.last().unwrap();
    assert!(tail.cells.iter().any(|cell| cell.attributes.bold));
}

#[test]
fn an_overgrown_journal_asks_to_be_compacted() {
    let path = std::env::temp_dir().join("mux-journal-threshold.ansi");
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .unwrap();
    let mut journal = PaneJournal::new(file, 0);
    assert!(!journal.needs_compaction());
    journal.length = MAX_JOURNAL_BYTES + 1;
    assert!(journal.needs_compaction());
    fs::remove_file(&path).unwrap();
}

#[test]
fn clipboard_copy_finishes_on_a_worker() {
    let path = std::env::temp_dir().join(format!("mux-clipboard-copy-{}", std::process::id()));
    let (sender, receiver) = mpsc::channel();
    let command = vec![
        "sh".into(),
        "-c".into(),
        "sleep 0.2; cat > \"$1\"".into(),
        "sh".into(),
        path.to_string_lossy().into_owned(),
    ];

    copy_to_clipboard(sender, 7, command, "copied text".into());
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    let event = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    let Event::ClipboardCopied(id, bytes, result) = event else {
        panic!("clipboard worker sent the wrong event");
    };
    assert_eq!(id, 7);
    assert_eq!(bytes, 11);
    assert_eq!(result, Ok(()));
    assert_eq!(fs::read_to_string(&path).unwrap(), "copied text");
    fs::remove_file(path).unwrap();
}

#[test]
fn a_client_that_stops_reading_falls_behind_instead_of_blocking_the_daemon() {
    let (server_end, client_end) = UnixStream::pair().unwrap();
    let writer = ClientWriter::spawn(server_end);
    // The client never reads. Once its socket and its queue are both full,
    // the daemon is told to stop waiting rather than blocking here; the
    // caller then repaints from scratch instead of sending another diff.
    let mut accepted = 0;
    for _ in 0..64 {
        if !writer.send(ServerMessage::Render(vec![b'x'; 1 << 20])) {
            break;
        }
        accepted += 1;
    }
    assert!(
        accepted < 64,
        "the queue refused nothing after {accepted} unread frames"
    );
    drop(client_end);
}

#[test]
fn a_journal_that_cannot_be_written_gives_up_instead_of_failing_forever() {
    let path = std::env::temp_dir().join(format!("mux-journal-failure-{}", std::process::id()));
    fs::write(&path, b"").unwrap();
    // A read-only descriptor stands in for a disk that will not take writes.
    let mut journal = PaneJournal::new(File::open(&path).unwrap(), 0);
    journal.append_output(b"buffered").unwrap();
    assert!(journal.flush().is_err(), "the first failure is reported");
    assert!(journal.abandoned);
    assert!(
        journal.flush().is_ok() && journal.append_output(b"more").is_ok(),
        "later writes stay quiet instead of repeating the failure"
    );
    journal.length = MAX_JOURNAL_BYTES + 1;
    assert!(
        !journal.needs_compaction(),
        "an abandoned journal is not worth compacting"
    );
    fs::remove_file(&path).unwrap();
}

#[test]
fn pane_journal_replays_output_resizes_and_ignores_a_torn_tail() {
    let mut journal = encode_journal_record(JOURNAL_RESIZE, &[0, 3, 0, 12]).unwrap();
    journal.extend(encode_journal_record(JOURNAL_OUTPUT, b"persistent").unwrap());
    let valid_length = journal.len();
    journal.extend([JOURNAL_OUTPUT, 0, 0]);

    let mut parser = vt100::Parser::new(1, 1, SCROLLBACK_LINES);
    assert_eq!(
        replay_pane_journal(&mut parser, &mut Vec::new(), journal.as_slice()).unwrap(),
        valid_length as u64
    );
    assert_eq!(parser.screen().size(), (3, 12));
    assert!(parser.screen().contents().contains("persistent"));
}

#[test]
fn resized_scrollback_reflows_instead_of_truncating_lines() {
    let mut parser = vt100::Parser::new(2, 8, 100);
    parser.process(b"abcdefg\r\nhijklmn\r\nopqrstu");
    parser.screen_mut().set_size(2, 4);
    let (lines, _) = snapshot_screen(parser.screen_mut());
    let text: Vec<_> = lines.iter().map(|line| line.text.as_str()).collect();
    assert!(text.windows(2).any(|line| line == ["abcd", "efg"]));
}

#[test]
fn widening_a_pane_keeps_scrollback_cells_consistent() {
    let mut parser = vt100::Parser::new(2, 4, 100);
    parser.process(b"abc\r\ndef\r\nghi");
    parser.screen_mut().set_size(2, 8);
    let (lines, cursor) = snapshot_screen(parser.screen_mut());
    assert!(lines.iter().any(|line| line.text == "abc"));
    assert!(cursor.row < lines.len());
}

#[test]
fn resized_scrollback_keeps_wide_characters_whole() {
    let mut parser = vt100::Parser::new(2, 6, 100);
    parser.process("abcd界\r\nsecond\r\nthird".as_bytes());
    parser.screen_mut().set_size(2, 5);
    let (lines, _) = snapshot_screen(parser.screen_mut());
    assert!(lines.iter().any(|line| line.text.contains('界')));
    assert!(lines.iter().all(|line| {
        !line
            .cells
            .first()
            .is_some_and(|cell| cell.wide_continuation)
    }));
}

#[test]
fn compact_scrollback_preserves_text_width_and_colors_losslessly() {
    let mut parser = vt100::Parser::new(2, 12, 100);
    parser.process("\x1b[38;2;1;2;3mA\u{301}界\x1b[48;5;42m \x1b[0m\r\nnext\r\nlast".as_bytes());
    parser.screen_mut().set_scrollback(usize::MAX);
    let screen = parser.screen();

    let accented = screen.cell(0, 0).unwrap();
    assert_eq!(accented.contents(), "A\u{301}");
    assert_eq!(accented.fgcolor(), vt100::Color::Rgb(1, 2, 3));

    let wide = screen.cell(0, 1).unwrap();
    assert_eq!(wide.contents(), "界");
    assert!(wide.is_wide());
    assert!(screen.cell(0, 2).unwrap().is_wide_continuation());

    let colored_blank = screen.cell(0, 3).unwrap();
    assert_eq!(colored_blank.contents(), " ");
    assert_eq!(colored_blank.bgcolor(), vt100::Color::Idx(42));
}

#[test]
fn block_compressed_scrollback_preserves_terminal_cells_losslessly() {
    let mut parser = vt100::Parser::new(2, 40, 400);
    for line in 0..=300 {
        parser.process(
            format!("\x1b[38;2;1;2;3mline {line:03} A\u{301}界\x1b[48;5;42m \x1b[0m\r\n")
                .as_bytes(),
        );
    }
    let compressed_bytes = parser.screen().history_bytes();
    let (lines, _) = snapshot_screen(parser.screen_mut());
    let first = lines
        .iter()
        .find(|line| line.text.starts_with("line 000"))
        .unwrap();
    let last = lines
        .iter()
        .find(|line| line.text.starts_with("line 300"))
        .unwrap();

    assert_eq!(first.cells[9].contents(&first.text), "A\u{301}");
    assert_eq!(first.cells[10].contents(&first.text), "界");
    assert!(first.cells[11].wide_continuation);
    assert_eq!(first.cells[12].attributes.background, vt100::Color::Idx(42));
    assert_eq!(
        first.cells[0].attributes.foreground,
        vt100::Color::Rgb(1, 2, 3)
    );
    assert!(last.text.contains("line 300"));
    assert_eq!(parser.screen().history_bytes(), compressed_bytes);
}

#[test]
fn block_compressed_scrollback_discards_exactly_the_oldest_rows() {
    let mut parser = vt100::Parser::new(1, 20, 300);
    for line in 0..=600 {
        parser.process(format!("line {line:03}\r\n").as_bytes());
    }
    let (lines, _) = snapshot_screen(parser.screen_mut());
    let text: Vec<_> = lines.iter().map(|line| line.text.as_str()).collect();

    assert_eq!(lines.len(), 301);
    assert!(!text.contains(&"line 300"));
    assert!(text.contains(&"line 301"));
    assert!(text.contains(&"line 600"));
}

#[test]
fn twenty_thousand_full_ascii_rows_use_less_than_three_mibibytes() {
    let mut parser = vt100::Parser::new(1, 80, SCROLLBACK_LINES);
    let mut output = Vec::with_capacity((SCROLLBACK_LINES + 1) * 82);
    for _ in 0..=SCROLLBACK_LINES {
        output.extend_from_slice(&[b'x'; 80]);
        output.extend_from_slice(b"\r\n");
    }
    parser.process(&output);

    parser.screen_mut().set_scrollback(usize::MAX);
    let history_bytes = parser.screen().history_bytes();
    println!("20,000 compact 80-column ASCII rows: {history_bytes} bytes");
    assert_eq!(parser.screen().scrollback(), SCROLLBACK_LINES);
    assert!(
        history_bytes < 64 * 1024,
        "20,000 rows allocated {history_bytes} bytes"
    );
    assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "x");
    assert_eq!(parser.screen().cell(0, 78).unwrap().contents(), "x");
    assert_eq!(parser.screen().cell(0, 79).unwrap().contents(), "x");

    parser.screen_mut().set_scrollback(0);
    parser.screen_mut().set_size(1, 40);
    parser.screen_mut().set_scrollback(usize::MAX);
    let reflowed_history_bytes = parser.screen().history_bytes();
    println!("20,000 reflowed compact 40-column ASCII rows: {reflowed_history_bytes} bytes");
    assert_eq!(parser.screen().scrollback(), SCROLLBACK_LINES);
    assert!(
        reflowed_history_bytes < 64 * 1024,
        "20,000 reflowed rows allocated {reflowed_history_bytes} bytes"
    );
    assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "x");
    assert_eq!(parser.screen().cell(0, 39).unwrap().contents(), "x");

    let before_snapshot = parser.screen().history_bytes();
    let started = std::time::Instant::now();
    let (lines, _) = snapshot_screen(parser.screen_mut());
    println!(
        "snapshotted {} rows in {:?}",
        lines.len(),
        started.elapsed()
    );
    assert_eq!(lines.len(), SCROLLBACK_LINES + 1);
    assert_eq!(parser.screen().history_bytes(), reflowed_history_bytes);
    assert!(parser.screen().history_bytes() <= before_snapshot);

    let started = std::time::Instant::now();
    for _ in 0..1_000 {
        for row in 0..parser.screen().size().0 {
            for col in 0..parser.screen().size().1 {
                std::hint::black_box(parser.screen().cell(row, col));
            }
        }
    }
    println!("rendered 1,000 live viewports in {:?}", started.elapsed());
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}

#[test]
fn representative_terminal_histories_stay_small() {
    const ROWS: usize = 20_000;
    const COLS: u16 = 80;

    let mut source = Vec::new();
    let mut logs = Vec::new();
    let mut unicode = Vec::new();
    let mut entropy = Vec::new();
    let mut random = 0x9e37_79b9_u32;
    for line in 0..=ROWS {
        source.extend_from_slice(
            format!(
                "\x1b[38;5;{color}m  src/module_{module:02}.rs:{number:05}: fn render_item_{item:03}() {{\x1b[0m\r\n",
                color = 32 + line % 6,
                module = line % 37,
                number = line * 3,
                item = line % 211,
            )
            .as_bytes(),
        );
        logs.extend_from_slice(
            format!(
                "2026-08-12T15:{minute:02}:{second:02}Z worker={worker:02} level=INFO processed batch {line:05}\r\n",
                minute = line / 60 % 60,
                second = line % 60,
                worker = line % 12,
            )
            .as_bytes(),
        );
        unicode.extend_from_slice(
            format!(
                "\x1b[3{}m行 {line:05} — café A\u{301} 界面 Δοκιμή 🚀\x1b[0m\r\n",
                line % 8,
            )
            .as_bytes(),
        );
        for _ in 0..usize::from(COLS) {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            entropy.push(32 + (random % 95) as u8);
        }
        entropy.extend_from_slice(b"\r\n");
    }

    let histories = [
        ("source", source, 600 * 1024),
        ("logs", logs, 600 * 1024),
        ("unicode", unicode, 800 * 1024),
        ("high-entropy ASCII", entropy, 2_500 * 1024),
    ];
    for (name, output, limit) in histories {
        let mut parser = vt100::Parser::new(1, COLS, ROWS);
        parser.process(&output);
        let bytes = parser.screen().history_bytes();
        println!("{name}: {bytes} bytes");
        assert!(bytes < limit, "{name} retained {bytes} bytes");
    }
}

#[test]
fn top_anchored_scroll_regions_preserve_codex_history() {
    let mut parser = vt100::Parser::new(6, 20, 100);
    parser.process(b"\x1b[1;4r\x1b[4;1H");
    for line in 0..12 {
        parser.process(format!("\r\nhistory {line:02}").as_bytes());
    }
    parser.process(b"\x1b[r");
    parser.screen_mut().set_scrollback(usize::MAX);
    assert_eq!(parser.screen().scrollback(), 12);
    assert!(parser.screen().contents().contains("history 00"));
    let (lines, cursor) = snapshot_screen(parser.screen_mut());
    assert!(lines.iter().any(|line| line.text.contains("history 00")));
    assert!(cursor.row < lines.len());
}

#[test]
fn lower_scroll_regions_do_not_pollute_history() {
    let mut parser = vt100::Parser::new(6, 20, 100);
    parser.process(b"\x1b[2;4r\x1b[4;1H");
    for line in 0..12 {
        parser.process(format!("\r\nregion {line:02}").as_bytes());
    }
    parser.screen_mut().set_scrollback(usize::MAX);
    assert_eq!(parser.screen().scrollback(), 0);
}

#[test]
fn tree_display_adds_root_rows_without_consuming_shortcuts() {
    let items = vec![
        TreeItem {
            session_id: 1,
            window: None,
            pane: None,
            label: "work".into(),
        },
        TreeItem {
            session_id: 1,
            window: Some(0),
            pane: None,
            label: "window 1 · 1 pane".into(),
        },
    ];
    let rows = tree_display_rows(&items);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].item_index, 0);
    assert!(matches!(rows[1].kind, TreeRowKind::Root));
    assert_eq!(rows[2].item_index, 1);
    assert_eq!(
        two_sided_line(" work", "1 window ", 20),
        " work      1 window "
    );
}
