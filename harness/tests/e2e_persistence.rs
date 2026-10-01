//! The daemon going away, cleanly or not, and coming back with every
//! session, window and pane where it was and each pane's history intact.

use std::{thread, time::Duration};

use mux_harness::rig::{Client, Rig, TerminalOptions};

fn attached(rig: &Rig, options: TerminalOptions) -> Client {
    let mut client = rig.attach(options);
    client.wait_text("$");
    client
}

/// Builds two windows, the first split in two, each pane printing a marker
/// in colour, and returns the markers per window.
fn populate(rig: &Rig, client: &mut Client) -> Vec<Vec<String>> {
    let mut windows = Vec::new();
    for window in 0..2 {
        if window > 0 {
            rig.mux_ok(&["new-window"]);
            client.wait_text("$");
        }
        let mut markers = Vec::new();
        for pane in 0..(2 - window) {
            if pane > 0 {
                rig.mux_ok(&["split-window", "-h"]);
                thread::sleep(Duration::from_millis(300));
            }
            let marker = format!("KEEP-W{window}P{pane}");
            // Printed in two halves, so the typed command line cannot show
            // the marker before the command has run.
            let (head, tail) = marker.split_at(marker.len() / 2);
            client.type_str(&format!(
                "i=0; while [ $i -lt 30 ]; do echo old-$i; i=$((i+1)); done; printf '\\033[1;32m%s%s\\033[0m\\n' {head} {tail}\r"
            ));
            client.wait_text(&marker);
            markers.push(marker);
        }
        windows.push(markers);
    }
    // Journals flush within milliseconds; give them a moment.
    thread::sleep(Duration::from_millis(300));
    windows
}

fn assert_restored(rig: &Rig, client: &mut Client, windows: &[Vec<String>]) {
    let sessions: serde_json::Value =
        serde_json::from_str(&rig.mux_ok(&["list-sessions", "--json"])).unwrap();
    let session = &sessions[0];
    assert_eq!(sessions.as_array().unwrap().len(), 1, "{sessions}");
    assert_eq!(session["windows"], 2, "{sessions}");
    assert_eq!(session["panes"], 3, "{sessions}");
    for (index, markers) in windows.iter().enumerate() {
        rig.mux_ok(&["select-window", "-t", &(index + 1).to_string()]);
        client.settle(Duration::from_millis(300), Duration::from_secs(5));
        let screen = client.screen();
        for marker in markers {
            assert!(
                screen.contains(marker),
                "{marker} lost:\n{}",
                screen.render()
            );
        }
        // The colour came back with the text.
        let marker: Vec<String> = markers[0].chars().map(String::from).collect();
        // The command line that printed it shows the marker too, plainly;
        // the output itself must still be bold and green.
        let styled = (0..screen.rows)
            .flat_map(|row| (0..screen.cols).map(move |col| (row, col)))
            .filter(|(row, col)| {
                marker.iter().enumerate().all(|(offset, text)| {
                    col + offset < screen.cols && screen.cell(*row, col + offset).text == *text
                })
            })
            .any(|(row, col)| {
                let cell = screen.cell(row, col);
                cell.bold && cell.fg == mux_harness::screen::Color::Idx(2)
            });
        assert!(styled, "attributes lost:\n{}", screen.render());
    }
}

#[test]
fn a_killed_daemon_comes_back_with_every_pane_and_its_history() {
    let mut rig = Rig::new();
    let mut client = attached(&rig, TerminalOptions::default());
    let windows = populate(&rig, &mut client);
    rig.crash_daemon();
    // The client loses its daemon and ends; the terminal is its own again.
    client.wait_exit(Duration::from_secs(10));
    drop(client);
    rig.start_daemon();
    let mut client = attached(&rig, TerminalOptions::default());
    assert_restored(&rig, &mut client, &windows);
}

#[test]
fn a_stopped_daemon_comes_back_at_another_size() {
    let mut rig = Rig::new();
    let mut client = attached(&rig, TerminalOptions::default());
    let windows = populate(&rig, &mut client);
    rig.stop_daemon();
    client.wait_exit(Duration::from_secs(10));
    drop(client);
    rig.start_daemon();
    let mut client = attached(
        &rig,
        TerminalOptions {
            rows: 40,
            cols: 120,
            ..TerminalOptions::default()
        },
    );
    assert_restored(&rig, &mut client, &windows);
    // And the restored shells work.
    client.type_str("echo ALIVE-$((6*7))\r");
    client.wait_text("ALIVE-42");
}

#[test]
fn a_restarted_daemon_keeps_a_long_history_after_compaction() {
    let mut rig = Rig::new();
    let mut client = attached(&rig, TerminalOptions::default());
    // Several MiB, enough to cross the compaction threshold.
    client.type_str("i=0; while [ $i -lt 60000 ]; do echo \"history line $i with some padding to make it longer\"; i=$((i+1)); done; echo HISTORY-\"\"END\r");
    client.wait_for("the output to finish", Duration::from_secs(120), |screen| {
        screen.contains("HISTORY-END")
    });
    thread::sleep(Duration::from_secs(2));
    rig.stop_daemon();
    client.wait_exit(Duration::from_secs(10));
    drop(client);
    rig.start_daemon();
    let mut client = attached(&rig, TerminalOptions::default());
    client.settle(Duration::from_millis(300), Duration::from_secs(10));
    let screen = client.screen();
    assert!(screen.contains("HISTORY-END"), "{}", screen.render());
    assert!(screen.contains("history line 59999"), "{}", screen.render());
}
