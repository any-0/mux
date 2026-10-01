//! Behaviour a person notices directly: what survives a split, what a
//! terminal is sent according to what it supports, focus reports.

use std::{
    fs, thread,
    time::{Duration, Instant},
};

use mux_harness::{
    escape,
    rig::{Client, Rig, TerminalOptions},
};

fn attached(rig: &Rig, options: TerminalOptions) -> Client {
    let mut client = rig.attach(options);
    client.wait_text("$");
    client
}

fn wait_for_file(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn splitting_keeps_the_prompt_and_the_newest_output_in_view() {
    let rig = Rig::new();
    let mut client = attached(&rig, TerminalOptions::default());
    client.type_str(
        "i=0; while [ $i -lt 60 ]; do echo line-$i; i=$((i+1)); done; echo LAST-\"\"LINE\r",
    );
    client.wait_text("LAST-LINE");
    client.settle(Duration::from_millis(150), Duration::from_secs(3));
    // Top/bottom: the original pane keeps the top half.
    rig.mux_ok(&["split-window"]);
    rig.mux_ok(&["select-pane", "-U"]);
    client.settle(Duration::from_millis(200), Duration::from_secs(3));
    let screen = client.screen();
    let text = screen.text();
    assert!(
        text.contains("LAST-LINE"),
        "the newest output left the shrunk pane:\n{}",
        screen.render()
    );
    assert!(
        text.contains("line-59"),
        "the newest output left the shrunk pane:\n{}",
        screen.render()
    );
    // Closing the split gives the rows, and the history in them, back.
    rig.mux_ok(&["select-pane", "-D"]);
    rig.mux_ok(&["kill-pane"]);
    client.settle(Duration::from_millis(200), Duration::from_secs(3));
    let screen = client.screen();
    for line in ["line-45", "line-59", "LAST-LINE"] {
        assert!(
            screen.contains(line),
            "{line} did not come back:\n{}",
            screen.render()
        );
    }
}

/// The parameters of every SGR sequence the client wrote.
fn sgr_parameters(transcript: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(transcript);
    let mut found = Vec::new();
    let mut rest = text.as_ref();
    while let Some(start) = rest.find("\x1b[") {
        rest = &rest[start + 2..];
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == ';' || c == ':'))
            .unwrap_or(rest.len());
        if rest[end..].starts_with('m') {
            found.push(rest[..end].to_string());
        }
        rest = &rest[end..];
    }
    found
}

fn styled_output(term: &str, extra_env: Vec<(String, String)>) -> String {
    let rig = Rig::new();
    let mut client = attached(
        &rig,
        TerminalOptions {
            term: term.into(),
            extra_env,
            ..TerminalOptions::default()
        },
    );
    client.type_str("printf '\\033[4:3;58:2::255:0:0mCUR''LY\\033[0m\\n'\r");
    client.wait_text("CURLY");
    client.settle(Duration::from_millis(150), Duration::from_secs(3));
    sgr_parameters(&client.transcript).join(" ")
}

#[test]
fn styled_underlines_are_only_sent_to_terminals_that_draw_them() {
    let plain = styled_output("xterm-256color", Vec::new());
    assert!(
        !plain.contains("58:"),
        "underline colour sent to a plain xterm: {}",
        escape(plain.as_bytes())
    );
    assert!(
        !plain.contains("4:3"),
        "curly underline sent to a plain xterm: {}",
        escape(plain.as_bytes())
    );
    assert!(
        plain.split(' ').any(|sgr| sgr.split(';').any(|p| p == "4")),
        "the underline itself was lost: {}",
        escape(plain.as_bytes())
    );
    let kitty = styled_output("xterm-kitty", Vec::new());
    assert!(kitty.contains("4:3"), "{}", escape(kitty.as_bytes()));
    assert!(
        kitty.contains("58:2::255:0:0"),
        "{}",
        escape(kitty.as_bytes())
    );
    let forced = styled_output(
        "xterm-256color",
        vec![("VTE_VERSION".into(), "7600".into())],
    );
    assert!(forced.contains("4:3"), "{}", escape(forced.as_bytes()));
}

#[test]
fn a_terminal_without_truecolor_gets_no_24_bit_sequences() {
    let rig = Rig::new();
    let mut client = attached(
        &rig,
        TerminalOptions {
            colorterm: None,
            ..TerminalOptions::default()
        },
    );
    client.type_str("printf '\\033[38;2;10;200;30mR''GB\\033[0m\\n'\r");
    client.wait_text("RGB");
    client.settle(Duration::from_millis(150), Duration::from_secs(3));
    let output = sgr_parameters(&client.transcript).join(" ");
    assert!(
        !output.contains("38;2;") && !output.contains("48;2;"),
        "{}",
        escape(output.as_bytes())
    );
    assert!(
        output.contains("38;5;"),
        "no palette colour at all: {}",
        escape(output.as_bytes())
    );
}

#[test]
fn programs_that_ask_are_told_about_focus() {
    let rig = Rig::new();
    let mut client = attached(&rig, TerminalOptions::default());
    // A second pane to move focus to and from.
    rig.mux_ok(&["split-window", "-h"]);
    rig.mux_ok(&["select-pane", "-L"]);
    let captured = rig.root.join("focus");
    let ready = rig.root.join("focus-ready");
    client.type_str(&format!(
        "stty raw -echo; printf '\\033[?1004h'; touch '{}'; dd bs=1 count=15 of='{}' 2>/dev/null; stty sane\r",
        ready.display(),
        captured.display()
    ));
    wait_for_file(&ready);
    thread::sleep(Duration::from_millis(200));
    // The terminal loses and regains focus; then mux moves to the other pane
    // and back.
    client.type_bytes(b"\x1b[O");
    thread::sleep(Duration::from_millis(100));
    client.type_bytes(b"\x1b[I");
    thread::sleep(Duration::from_millis(100));
    rig.mux_ok(&["select-pane", "-R"]);
    thread::sleep(Duration::from_millis(100));
    rig.mux_ok(&["select-pane", "-L"]);
    thread::sleep(Duration::from_millis(100));
    client.type_str("x");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got = Vec::new();
    while Instant::now() < deadline {
        got = fs::read(&captured).unwrap_or_default();
        if got.len() >= 15 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(escape(&got), escape(b"\x1b[O\x1b[I\x1b[O\x1b[Ix"));
}

#[test]
fn text_glyphs_draw_the_sidebar_with_characters_every_font_has() {
    let rig = Rig::with_config("glyphs = \"text\"\n");
    let mut client = attached(&rig, TerminalOptions::default());
    rig.mux_ok(&["new-window"]);
    client.type_str("vi -c 'sleep 1' -c q 2>/dev/null; echo GLY\"\"PHS\r");
    client.wait_text("GLYPHS");
    client.settle(Duration::from_millis(300), Duration::from_secs(3));
    let text = String::from_utf8_lossy(&client.transcript).into_owned();
    let private: Vec<char> = text
        .chars()
        .filter(|c| ('\u{e000}'..='\u{f8ff}').contains(c))
        .collect();
    assert!(private.is_empty(), "private-use glyphs sent: {private:?}");
    let screen = client.screen();
    assert!(
        (0..screen.rows).any(|row| screen.row_text(row).contains('┤')),
        "no separator marks the active window:\n{}",
        screen.render()
    );
}
