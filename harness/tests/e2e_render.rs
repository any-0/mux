//! End to end: programs run in real panes of the real binary, and what the
//! attached terminal ends up showing is compared with an independent
//! emulator that was fed the same program output directly.

use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};

use mux_harness::{
    generate::{Feature, Generator},
    reference::Reference,
    rig::{Client, Rig, TerminalOptions},
    screen::{Compare, Snapshot},
};

/// Features whose output the pane and the reference agree on exactly; the
/// emulator differential test covers the rest of the ground on its own.
const PANE_FEATURES: &[Feature] = &[
    Feature::Text,
    Feature::Unicode,
    Feature::Controls,
    Feature::Cursor,
    Feature::Erase,
    Feature::Edit,
    Feature::Scroll,
    Feature::Sgr,
    Feature::SaveRestore,
    Feature::AutoWrap,
    Feature::Origin,
    Feature::Insert,
    Feature::Charset,
    Feature::Repeat,
    Feature::Tabs,
];

fn wait_for_file(path: &Path, limit: Duration) {
    let deadline = Instant::now() + limit;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// Starts a client whose single pane shows a shell prompt.
fn attached(rig: &Rig, options: TerminalOptions) -> Client {
    let mut client = rig.attach(options);
    client.wait_text("$");
    client
}

/// Makes the pane `cat` `bytes` with echo off and then sleep, so nothing but
/// the stream decides what it shows. Returns once the output has settled.
fn show_in_pane(rig: &Rig, client: &mut Client, name: &str, bytes: &[u8]) {
    let fixture = rig.file(name, bytes);
    let done = rig.root.join(format!("{name}.done"));
    client.type_str(&format!(
        "stty -echo; cat '{}'; touch '{}'; exec sleep 100000\r",
        fixture.display(),
        done.display()
    ));
    wait_for_file(&done, Duration::from_secs(10));
    client.settle(Duration::from_millis(150), Duration::from_secs(5));
}

fn expected(rows: u16, cols: u16, tokens: &[Vec<u8>]) -> Snapshot {
    let mut reference = Reference::new(rows as usize, cols as usize);
    reference.feed(b"\x1bc");
    for token in tokens {
        reference.feed_token(token);
    }
    reference.snapshot()
}

#[test]
fn a_focused_pane_shows_exactly_what_its_program_drew() {
    let rig = Rig::new();
    let (rows, cols) = (24, 80);
    let mut client = attached(
        &rig,
        TerminalOptions {
            rows,
            cols,
            ..TerminalOptions::default()
        },
    );
    rig.mux_ok(&["focus-mode"]);
    let tokens = Generator::new(7, rows, cols, PANE_FEATURES).tokens(400);
    let mut stream = b"\x1bc".to_vec();
    stream.extend(tokens.concat());
    show_in_pane(&rig, &mut client, "stream", &stream);
    // Status messages expire; let any that the mode switch showed go.
    thread::sleep(Duration::from_millis(1800));
    client.settle(Duration::from_millis(150), Duration::from_secs(5));
    let want = expected(rows, cols, &tokens);
    if let Some(report) = want.diff(&client.screen(), Compare::FULL) {
        panic!("outer terminal differs from the pane's program:\n{report}");
    }
}

/// Asserts that `client` shows what a fresh attach would.
fn assert_matches_fresh(rig: &Rig, client: &mut Client, context: &str) {
    client.settle(Duration::from_millis(150), Duration::from_secs(5));
    let fresh = rig.fresh_view(client);
    // The fresh client leaving may repaint the first; let that land too.
    client.settle(Duration::from_millis(150), Duration::from_secs(5));
    if let Some(report) = fresh.diff(&client.screen(), Compare::FULL) {
        panic!("{context}: incrementally painted terminal drifted from a fresh paint:\n{report}");
    }
}

/// Fills the pane with a screenful of distinct, coloured lines.
fn fill_pane(client: &mut Client, marker: &str) {
    client.type_str(&format!(
        "clear; i=0; while [ $i -lt 40 ]; do printf '\\033[3%dm%s-%02d %s\\033[m\\n' $((i%7+1)) {marker} $i abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ; i=$((i+1)); done\r"
    ));
    client.wait_text(&format!("{marker}-39"));
}

#[test]
fn a_resize_that_returns_to_the_same_size_repaints_everything() {
    let rig = Rig::new();
    let mut client = attached(&rig, TerminalOptions::default());
    rig.mux_ok(&["split-window", "-h"]);
    fill_pane(&mut client, "BOUNCE");
    assert_matches_fresh(&rig, &mut client, "before resizing");
    for narrow in [30, 50, 79] {
        // Two resizes back to back, before the daemon can paint in between:
        // the terminal has cut its contents, but the frame size is unchanged.
        client.resize(24, narrow);
        client.resize(24, 80);
        assert_matches_fresh(&rig, &mut client, &format!("after 80 -> {narrow} -> 80"));
    }
}

#[test]
fn a_terminal_that_stops_reading_for_a_while_catches_up_intact() {
    let rig = Rig::new();
    let mut client = attached(&rig, TerminalOptions::default());
    fill_pane(&mut client, "BEFORE");
    // The terminal stops reading (a frozen ssh link, a suspended emulator)
    // while the pane keeps printing far more than any buffer holds.
    client.set_paused(true);
    client.type_str(
        "i=0; while [ $i -lt 30000 ]; do echo \"flood $i abcdefghijklmnopqrstuvwxyz\"; i=$((i+1)); done; echo FLOOD-\"\"DONE\r",
    );
    thread::sleep(Duration::from_secs(8));
    client.set_paused(false);
    client.wait_for(
        "the client to catch up",
        Duration::from_secs(30),
        |screen| screen.contains("FLOOD-DONE"),
    );
    assert_eq!(
        client.exited(),
        None,
        "the client was disconnected while its terminal was stalled"
    );
    assert_matches_fresh(&rig, &mut client, "after catching up");
}

fn resident_kib(pid: u32) -> u64 {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

#[test]
fn a_long_stall_under_continuous_output_neither_bloats_nor_lags_the_client() {
    let rig = Rig::new();
    let mut client = attached(&rig, TerminalOptions::default());
    let pid = client.pid().unwrap();
    // A program repainting the whole screen in colour as fast as it can, as
    // top, a game or a video player does: every frame differs everywhere.
    let painter = rig.file(
        "painter",
        b"i=0; while [ ! -e stop ]; do printf '\\033[H'; j=0; while [ $j -lt 23 ]; do printf '\\033[3%dm%08d %s\\033[K\\n' $(((i+j)%7+1)) $i abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789; j=$((j+1)); done; i=$((i+1)); done\n",
    );
    client.type_str(&format!("sh '{}'\r", painter.display()));
    thread::sleep(Duration::from_secs(2));
    client.settle(Duration::from_millis(1), Duration::from_millis(500));
    let baseline = resident_kib(pid);
    client.set_paused(true);
    thread::sleep(Duration::from_secs(25));
    let stalled = resident_kib(pid);
    client.set_paused(false);
    // What reaches the terminal once it reads again should be the present,
    // not the 25 seconds of frames it missed.
    let before = client.transcript.len();
    let resumed = Instant::now();
    while resumed.elapsed() < Duration::from_secs(2) {
        client.pump(Duration::from_millis(10));
    }
    let replayed = client.transcript.len() - before;
    println!(
        "client rss: {baseline} KiB -> {stalled} KiB while stalled; {} KiB in the 2 s after",
        replayed / 1024
    );
    assert_eq!(client.exited(), None, "client was disconnected");
    assert!(
        stalled < baseline + 2 * 1024,
        "client buffered {} KiB while its terminal was stalled",
        stalled.saturating_sub(baseline)
    );
    assert!(
        replayed < 2 * 1024 * 1024,
        "{} KiB of stale frames were replayed after the stall",
        replayed / 1024
    );
    std::fs::write(rig.root.join("home/stop"), b"").unwrap();
    client.type_str("clear; echo CAUGHT-\"\"UP\r");
    client.wait_for("the present", Duration::from_secs(10), |screen| {
        screen.contains("CAUGHT-UP")
    });
}

/// A terminal that draws some characters wider than mux thinks they are must
/// still get every other character in the column mux meant, and must never
/// scroll, even with such a character in the bottom right corner.
#[test]
fn a_terminal_that_disagrees_about_widths_stays_aligned() {
    let rig = Rig::new();
    let (rows, cols) = (6, 20);
    let options = TerminalOptions {
        rows,
        cols,
        // Emoji presentation and ambiguous-as-wide, simulated.
        substitute: vec![
            ("❤\u{fe0f}".into(), "中".into()),
            ("•".into(), "字".into()),
            ("─".into(), "文".into()),
        ],
        ..TerminalOptions::default()
    };
    let mut client = attached(&rig, options);
    rig.mux_ok(&["focus-mode"]);
    let tokens = vec![
        b"\x1b[1;1HA\xe2\x9d\xa4\xef\xb8\x8fB\xe2\x80\xa2C-D".to_vec(),
        "\x1b[2;1H──x──y──z".as_bytes().to_vec(),
        // The last cell of the screen: drawn two wide, it must not scroll.
        format!("\x1b[{rows};1HBOTTOM\x1b[{rows};{cols}H•").into_bytes(),
        b"\x1b[1;1H".to_vec(),
    ];
    let mut stream = b"\x1bc".to_vec();
    stream.extend(tokens.concat());
    show_in_pane(&rig, &mut client, "widths", &stream);
    thread::sleep(Duration::from_millis(1800));
    client.settle(Duration::from_millis(150), Duration::from_secs(5));
    let screen = client.screen();
    let at = |row: usize, col: usize| screen.cell(row, col).text.clone();
    assert_eq!(
        (at(0, 0), at(0, 2), at(0, 4), at(0, 5), at(0, 6)),
        ("A".into(), "B".into(), "C".into(), "-".into(), "D".into()),
        "\n{}",
        screen.render()
    );
    assert_eq!(
        (at(1, 2), at(1, 5), at(1, 8)),
        ("x".into(), "y".into(), "z".into()),
        "\n{}",
        screen.render()
    );
    assert!(
        screen.row_text(rows as usize - 1).starts_with("BOTTOM"),
        "the screen scrolled:\n{}",
        screen.render()
    );
}
