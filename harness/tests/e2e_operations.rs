//! Random sequences of real operations against the real binary. After every
//! few steps the terminal kept up to date incrementally must match a fresh
//! client's full paint, the daemon must still be alive, and the session's
//! structure must be consistent.

use std::{
    thread,
    time::{Duration, Instant},
};

use mux_harness::rig::{Client, Rig, TerminalOptions};
use rand::{Rng, SeedableRng, rngs::SmallRng, seq::IndexedRandom};

#[derive(Debug, Clone, Copy)]
enum Step {
    SplitHorizontal,
    SplitVertical,
    NewWindow,
    SelectWindow(u8),
    FocusPane(&'static str),
    ResizePane(&'static str, u8),
    KillPane,
    FocusMode,
    BreakPane,
    SwapWindow(u8),
    Resize(u16, u16),
    ResizeBounce(u16),
    Output(u16),
    ColorOutput,
    FullScreenApp,
    Detach,
    Refresh,
    Typing,
}

fn random_step(rng: &mut SmallRng) -> Step {
    const DIRECTIONS: &[&str] = &["-L", "-R", "-U", "-D"];
    match rng.random_range(0..100) {
        0..8 => Step::SplitHorizontal,
        8..16 => Step::SplitVertical,
        16..21 => Step::NewWindow,
        21..28 => Step::SelectWindow(rng.random_range(1..=4)),
        28..36 => Step::FocusPane(DIRECTIONS.choose(rng).unwrap()),
        36..41 => Step::ResizePane(DIRECTIONS.choose(rng).unwrap(), rng.random_range(1..6)),
        41..46 => Step::KillPane,
        46..50 => Step::FocusMode,
        50..53 => Step::BreakPane,
        53..56 => Step::SwapWindow(rng.random_range(1..=3)),
        56..64 => Step::Resize(rng.random_range(8..40), rng.random_range(20..120)),
        64..68 => Step::ResizeBounce(rng.random_range(10..70)),
        68..78 => Step::Output(rng.random_range(1..200)),
        78..84 => Step::ColorOutput,
        84..88 => Step::FullScreenApp,
        88..91 => Step::Detach,
        91..93 => Step::Refresh,
        _ => Step::Typing,
    }
}

fn wait_quiet(client: &mut Client) {
    client.settle(Duration::from_millis(120), Duration::from_secs(10));
}

/// Sends a shell command to the active pane and waits for its marker, which
/// is quoted apart in the command so the echoed line cannot match it.
fn run(client: &mut Client, command: &str, marker: &str) {
    client.type_str(&format!("{command}; echo {marker}-\"\"DONE\r"));
    let needle = format!("{marker}-DONE");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        client.pump(Duration::from_millis(20));
        if client.screen().contains(&needle) {
            return;
        }
    }
    // A pane too small to show the marker, or hidden behind another, still
    // ran the command; carry on rather than fail on visibility.
}

fn apply(rig: &Rig, client: &mut Client, step: Step, index: usize) -> Option<Client> {
    let ok = |args: &[&str]| {
        // Operations that make no sense in the current state (killing the last
        // pane of the last window, selecting a missing window) may be refused;
        // what matters is that the daemon stays consistent.
        let _ = rig.mux(args);
    };
    match step {
        Step::SplitHorizontal => ok(&["split-window"]),
        Step::SplitVertical => ok(&["split-window", "-h"]),
        Step::NewWindow => ok(&["new-window"]),
        Step::SelectWindow(number) => ok(&["select-window", "-t", &number.to_string()]),
        Step::FocusPane(direction) => ok(&["select-pane", direction]),
        Step::ResizePane(direction, cells) => ok(&["resize-pane", direction, &cells.to_string()]),
        Step::KillPane => {
            // Never the last pane: that would end the session and the client.
            if rig.panes().len() > 1 {
                ok(&["kill-pane"]);
            }
        }
        Step::FocusMode => ok(&["focus-mode"]),
        Step::BreakPane => ok(&["break-pane"]),
        Step::SwapWindow(number) => ok(&["swap-window", "-t", &number.to_string()]),
        Step::Resize(rows, cols) => client.resize(rows, cols),
        Step::ResizeBounce(cols) => {
            let (rows, original) = client.size();
            client.resize(rows, cols);
            client.resize(rows, original);
        }
        Step::Output(lines) => run(
            client,
            &format!(
                "i=0; while [ $i -lt {lines} ]; do echo \"step {index} line $i\"; i=$((i+1)); done"
            ),
            &format!("OUT{index}"),
        ),
        Step::ColorOutput => run(
            client,
            "printf '\\033[1;31mred\\033[0m \\033[4:3;58:2::0:255:0mcurly\\033[0m \\033[9mstrike\\033[0m 中文 \\033[48;5;200mbg\\033[0m e\\314\\201\\n'",
            &format!("COLOR{index}"),
        ),
        Step::FullScreenApp => {
            // What a curses program does: alternate screen, a scroll region,
            // line drawing, then back to the shell as it was.
            run(
                client,
                "printf '\\033[?1049h\\033[H\\033[2J\\033(0lqqk\\033(B\\033[2;5r\\033[5;1Hscroll\\n\\n\\033[r\\033[1;1H\\033[4hINS\\033[4l'; sleep 0.2; printf '\\033[?1049l'",
                &format!("APP{index}"),
            );
        }
        Step::Detach => {
            let options = client.options.clone();
            let (rows, cols) = client.size();
            rig.mux_ok(&["detach"]);
            client.wait_exit(Duration::from_secs(10));
            let mut fresh = rig.attach(TerminalOptions {
                rows,
                cols,
                ..options
            });
            wait_quiet(&mut fresh);
            return Some(fresh);
        }
        Step::Refresh => ok(&["refresh-client"]),
        Step::Typing => {
            client.type_str("echo typed-");
            client.type_bytes(b"\x7f\x7f");
            client.type_str("x\x15");
        }
    }
    None
}

fn check(rig: &Rig, client: &mut Client, seed: u64, history: &[Step]) {
    // A refused command leaves a transient message on the client that sent
    // it, which a fresh client would not show; let it expire (1.6 s) first.
    thread::sleep(Duration::from_millis(1700));
    wait_quiet(client);
    let fresh = rig.fresh_view(client);
    wait_quiet(client);
    let screen = client.screen();
    if let Some(report) = fresh.diff(&screen, mux_harness::screen::Compare::FULL) {
        panic!(
            "seed {seed}: incremental paint drifted after {history:?}\n{report}\ndaemon log:\n{}",
            rig.daemon_log()
        );
    }
    assert_eq!(
        client.exited(),
        None,
        "seed {seed}: client exited after {history:?}"
    );
}

fn exercise(seed: u64, steps: usize) {
    let rig = Rig::new();
    let mut rng = SmallRng::seed_from_u64(seed);
    let mut client = rig.attach(TerminalOptions {
        rows: 24,
        cols: 80,
        ..TerminalOptions::default()
    });
    client.wait_text("$");
    let mut history = Vec::new();
    for index in 0..steps {
        let step = random_step(&mut rng);
        history.push(step);
        if let Some(fresh) = apply(&rig, &mut client, step, index) {
            client = fresh;
        }
        wait_quiet(&mut client);
        if index % 4 == 3 {
            check(&rig, &mut client, seed, &history);
        }
        // The daemon must answer queries throughout.
        let _ = rig.mux_ok(&["list-panes"]);
    }
    check(&rig, &mut client, seed, &history);
    thread::sleep(Duration::from_millis(10));
}

#[test]
fn random_operations_keep_every_terminal_consistent() {
    let seeds: u64 = std::env::var("MUX_OPERATION_SEEDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3);
    let steps: usize = std::env::var("MUX_OPERATION_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(24);
    for seed in 0..seeds {
        exercise(seed, steps);
    }
}
