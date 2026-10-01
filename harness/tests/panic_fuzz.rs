//! Nothing a program writes, at any size, through any resize, may panic the
//! emulator: in mux a panic there takes the daemon, and every shell it
//! owns, with it.

use mux_harness::generate::{Feature, Generator};
use rand::{Rng, SeedableRng, rngs::SmallRng};

#[derive(Clone, Debug)]
enum Op {
    Bytes(Vec<u8>),
    Resize(u16, u16),
    Scroll(usize),
    Persist,
}

fn operations(seed: u64) -> (u16, u16, usize, Vec<Op>) {
    let mut rng = SmallRng::seed_from_u64(seed);
    let (mut rows, mut cols) = (rng.random_range(1..8), rng.random_range(1..12));
    let scrollback = rng.random_range(0..50);
    let (first_rows, first_cols) = (rows, cols);
    let mut ops = Vec::new();
    for _ in 0..rng.random_range(1..40) {
        ops.push(match rng.random_range(0..10) {
            0..4 => Op::Bytes(
                Generator::new(rng.random(), rows, cols, Feature::ALL)
                    .stream(rng.random_range(1..30)),
            ),
            4..6 => {
                // Raw bytes: control characters, truncated sequences, invalid
                // UTF-8, parameters far out of range.
                Op::Bytes(
                    (0..rng.random_range(1..200))
                        .map(|_| match rng.random_range(0..6) {
                            0 => 0x1b,
                            1 => b'[',
                            2 => rng.random_range(b'0'..=b'9'),
                            3 => rng.random_range(0x00..0x20),
                            4 => rng.random_range(0x80..=0xff),
                            _ => rng.random_range(0x20..0x7f),
                        })
                        .collect(),
                )
            }
            6 => {
                let huge = [
                    &b"\x1b[65535;65535H"[..],
                    b"\x1b[65535b",
                    b"\x1b[65535@",
                    b"\x1b[65535P",
                    b"\x1b[65535L",
                    b"\x1b[65535M",
                    b"\x1b[65535S",
                    b"\x1b[65535T",
                    b"\x1b[65535X",
                    b"\x1b[65535I",
                    b"\x1b[65535Z",
                    b"\x1b[0;0r",
                    b"\x1b[65535;1r",
                    b"\x1b[?6h\x1b[65535d",
                    b"\x1b[?1049h\x1b[?1049h\x1b[?1049l\x1b[?1049l",
                    b"\x1b[?47h\x1b[?1047l\x1b[?1048l",
                    b"\x1b[3J\x1bc\x1b[!p",
                    "\u{1f600}\u{fe0f}\u{200d}\u{1f525}".as_bytes(),
                ];
                Op::Bytes(huge[rng.random_range(0..huge.len())].to_vec())
            }
            7 => {
                rows = rng.random_range(1..10);
                cols = rng.random_range(1..14);
                Op::Resize(rows, cols)
            }
            8 => Op::Scroll(rng.random_range(0..100)),
            _ => Op::Persist,
        });
    }
    (first_rows, first_cols, scrollback, ops)
}

/// Runs `ops`, returning a description of the first thing that went wrong.
fn run(rows: u16, cols: u16, scrollback: usize, ops: &[Op]) -> Option<String> {
    let result = std::panic::catch_unwind(|| {
        let mut parser = vt100::Parser::new(rows, cols, scrollback);
        for op in ops {
            match op {
                Op::Bytes(bytes) => parser.process(bytes),
                Op::Resize(rows, cols) => parser.screen_mut().set_size(*rows, *cols),
                Op::Scroll(offset) => {
                    parser.screen_mut().set_scrollback(*offset);
                    let _ = parser.screen().contents();
                    parser.screen_mut().set_scrollback(0);
                }
                Op::Persist => {
                    // What mux does with a screen: format it for the journal,
                    // diff it for prompt corrections, pack and restore its
                    // history.
                    let (rows, cols) = parser.screen().size();
                    let formatted = parser.screen().contents_formatted();
                    let state = parser.screen().state_formatted();
                    let copy = parser.screen().clone();
                    let _ = parser.screen().state_diff(&copy);
                    let history = parser.screen().encode_history();
                    let mut replay = vt100::Parser::new(rows, cols, 50);
                    if !replay.screen_mut().restore_history(&history) {
                        return Some("history did not restore".to_string());
                    }
                    replay.process(&formatted);
                    replay.process(&state);
                }
            }
            let (rows, cols) = parser.screen().size();
            for row in 0..rows {
                for col in 0..cols {
                    let _ = parser.screen().cell(row, col);
                }
            }
            let _ = parser.screen().cursor_position();
        }
        None
    });
    match result {
        Ok(outcome) => outcome,
        Err(_) => Some("panicked".to_string()),
    }
}

fn check(seed: u64) {
    let (rows, cols, scrollback, ops) = operations(seed);
    let Some(failure) = run(rows, cols, scrollback, &ops) else {
        return;
    };
    // Shrink to the fewest operations that still fail the same way.
    let mut ops = ops;
    let mut chunk = ops.len() / 2;
    while chunk >= 1 {
        let mut start = 0;
        while start < ops.len() {
            let end = (start + chunk).min(ops.len());
            let mut candidate = ops.clone();
            candidate.drain(start..end);
            if run(rows, cols, scrollback, &candidate).as_ref() == Some(&failure) {
                ops = candidate;
            } else {
                start += chunk;
            }
        }
        chunk /= 2;
    }
    let steps: Vec<String> = ops
        .iter()
        .map(|op| match op {
            Op::Bytes(bytes) => format!("bytes {}", mux_harness::escape(bytes)),
            other => format!("{other:?}"),
        })
        .collect();
    // The expected panics were silenced; this one has to be heard.
    let _ = std::panic::take_hook();
    panic!(
        "seed {seed}: {failure} on a {rows}x{cols} screen with {scrollback} rows of history after:\n{}",
        steps.join("\n")
    );
}

#[test]
fn nothing_panics_the_emulator() {
    let seeds: u64 = std::env::var("MUX_PANIC_SEEDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20_000);
    std::panic::set_hook(Box::new(|_| {}));
    for seed in 0..seeds {
        check(seed);
    }
    let _ = std::panic::take_hook();
}

#[test]
#[ignore = "replays MUX_PANIC_SEED"]
fn replay_one() {
    let seed = std::env::var("MUX_PANIC_SEED").unwrap().parse().unwrap();
    check(seed);
}

/// No single sequence may take long: the daemon handles every pane on one
/// thread, so a slow one freezes all of them. Each CSI sequence, with the
/// largest parameters a parser accepts, must cost well under a frame.
#[test]
fn no_sequence_stalls_the_emulator() {
    use std::time::{Duration, Instant};
    let mut slowest = (Duration::ZERO, String::new());
    for prefix in ["", "?", ">", "!", " ", "=", "<"] {
        for final_byte in 0x40u8..=0x7e {
            for params in ["65535", "65535;65535", "0", "65535;65535;65535;65535"] {
                let sequence = format!("\x1b[{prefix}{params}{}", final_byte as char);
                let mut parser = vt100::Parser::new(100, 300, 1000);
                parser.process("x中".repeat(20_000).as_bytes());
                let start = Instant::now();
                parser.process(sequence.as_bytes());
                parser.process(b"x");
                let elapsed = start.elapsed();
                if elapsed > slowest.0 {
                    slowest = (elapsed, mux_harness::escape(sequence.as_bytes()));
                }
            }
        }
    }
    println!("slowest sequence: {} took {:?}", slowest.1, slowest.0);
    assert!(
        slowest.0 < Duration::from_millis(20),
        "{} took {:?}",
        slowest.1,
        slowest.0
    );
}
