//! Real programs, run twice at the same size: once straight in a
//! pseudo-terminal whose output the reference emulator draws, and once in a
//! mux pane in focus mode. What the person sees must be the same.
//!
//! Programs that are not installed are skipped, so this runs anywhere; the
//! Linux container (harness/docker) has all of them.

use std::{
    io::{Read, Write},
    path::Path,
    process::Command,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use mux_harness::{
    reference::Reference,
    rig::{Rig, TerminalOptions},
    screen::{Compare, Snapshot},
};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const ROWS: u16 = 24;
const COLS: u16 = 80;

fn installed(program: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {program}")])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Runs `command` in a plain pseudo-terminal and returns what the reference
/// shows once the output has been quiet for a while.
fn direct(command: &str, dir: &Path) -> Snapshot {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut builder = CommandBuilder::new("sh");
    builder.args(["-c", command]);
    builder.cwd(dir);
    builder.env("TERM", "xterm-256color");
    builder.env("COLORTERM", "truecolor");
    builder.env("LANG", "en_US.UTF-8");
    builder.env("LC_ALL", "en_US.UTF-8");
    builder.env("HOME", dir);
    let mut child = pair.slave.spawn_command(builder).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (sender, output) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = [0; 65536];
        while let Ok(length) = reader.read(&mut buffer) {
            if length == 0 || sender.send(buffer[..length].to_vec()).is_err() {
                return;
            }
        }
    });
    let mut reference = Reference::new(ROWS as usize, COLS as usize);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut quiet_since = Instant::now();
    while Instant::now() < deadline && quiet_since.elapsed() < Duration::from_millis(800) {
        if let Ok(bytes) = output.recv_timeout(Duration::from_millis(50)) {
            // Answer the device attribute queries programs send at startup,
            // as mux does, so both runs take the same path.
            if bytes.windows(3).any(|window| window == b"\x1b[c") {
                let _ = writer.write_all(b"\x1b[?1;2c");
            }
            reference.feed(&bytes);
            quiet_since = Instant::now();
        }
    }
    let _ = child.kill();
    reference.snapshot()
}

fn in_mux(command: &str, rig: &Rig) -> Snapshot {
    let mut client = rig.attach(TerminalOptions {
        rows: ROWS,
        cols: COLS,
        ..TerminalOptions::default()
    });
    client.wait_text("$");
    rig.mux_ok(&["focus-mode"]);
    client.settle(Duration::from_millis(200), Duration::from_secs(3));
    client.type_str(&format!(
        "cd '{}'; clear; exec sh -c '{command}'\r",
        rig.root.join("home").display()
    ));
    // Let the focus-mode message expire and the program draw.
    thread::sleep(Duration::from_millis(1800));
    client.settle(Duration::from_millis(800), Duration::from_secs(10));
    client.screen()
}

fn fixture(rig: &Rig) {
    let mut text = String::new();
    for index in 0..60 {
        text.push_str(&format!(
            "{index:03} the quick brown fox jumps over the lazy dog \u{00e9}\u{4e2d}\u{6587} {}\n",
            "=".repeat(index % 20)
        ));
    }
    std::fs::write(rig.root.join("home/sample.txt"), text).unwrap();
    // No clock or host name in tmux's status line, which would differ.
    std::fs::write(
        rig.root.join("home/tmux.conf"),
        "set -g status-right ''\nset -g status-left '[x] '\n",
    )
    .unwrap();
}

fn compare(name: &str, command: &str, program: &str, compare: Compare) {
    if !installed(program) {
        eprintln!("skipping {name}: {program} is not installed");
        return;
    }
    let rig = Rig::new();
    fixture(&rig);
    let want = direct(command, &rig.root.join("home"));
    let got = in_mux(command, &rig);
    if let Some(report) = want.diff(&got, compare) {
        panic!("{name}: mux shows something else than the program drew:\n{report}");
    }
}

#[test]
fn vim_looks_the_same() {
    compare(
        "vim",
        "vim -u NONE -i NONE -N -c \"set nomore laststatus=2 number cursorline background=dark\" -c \"syntax on\" +10 sample.txt",
        "vim",
        Compare::FULL,
    );
}

#[test]
fn less_looks_the_same() {
    compare("less", "LESS= less -N sample.txt", "less", Compare::FULL);
}

#[test]
fn nano_looks_the_same() {
    // macOS ships UW PICO under the name nano.
    let gnu = Command::new("nano")
        .arg("--version")
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("GNU nano"));
    if gnu {
        compare("nano", "nano -l sample.txt", "nano", Compare::FULL);
    }
}

#[test]
fn dialog_boxes_look_the_same() {
    compare(
        "dialog",
        "dialog --title Title --checklist Choose 15 50 5 a Alpha on b Beta off c Gamma off",
        "dialog",
        Compare::FULL,
    );
}

#[test]
fn tmux_inside_mux_looks_the_same() {
    compare(
        "tmux",
        "tmux -f tmux.conf -L harness new-session \"cat sample.txt; sleep 30\"",
        "tmux",
        Compare::TEXT,
    );
}

#[test]
fn midnight_commander_looks_the_same() {
    compare("mc", "mc -u --nosubshell", "mc", Compare::TEXT);
}
