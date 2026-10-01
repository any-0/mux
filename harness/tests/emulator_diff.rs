//! Differential testing of the vendored `vt100` emulator against alacritty.
//!
//! Every stream is fed to both; the screens must agree. A disagreement is
//! shrunk to the fewest tokens that still reproduce it.

use mux_harness::{
    escape,
    generate::{Feature, Generator},
    reference::Reference,
    screen::{Compare, Snapshot},
    shrink,
};

use std::cell::Cell;

thread_local! {
    static SIZE: Cell<(u16, u16)> = const { Cell::new((8, 20)) };
}

fn size() -> (u16, u16) {
    SIZE.with(Cell::get)
}

fn vt100_screen(tokens: &[Vec<u8>]) -> Snapshot {
    let (rows, cols) = size();
    let mut parser = vt100::Parser::new(rows, cols, 0);
    for token in tokens {
        parser.process(token);
    }
    Snapshot::from_vt100(parser.screen())
}

/// `(character, count)` for a generated REP token: one character then
/// `CSI count b`.
fn repeat_token(token: &[u8]) -> Option<(String, u16)> {
    let text = std::str::from_utf8(token).ok()?;
    let mut chars = text.chars();
    let character = chars.next()?;
    let count = chars.as_str().strip_prefix("\x1b[")?.strip_suffix('b')?;
    Some((character.to_string(), count.parse().unwrap_or(0)))
}

/// The reference's screen, or `None` once alacritty has torn a wide
/// character, after which it cannot judge anything.
fn reference_screen(tokens: &[Vec<u8>]) -> Option<Snapshot> {
    let (rows, cols) = size();
    let mut reference = Reference::new(rows as usize, cols as usize);
    for token in tokens {
        // REP is the character before it printed again, so the reference is
        // given exactly that, one character at a time.
        let expanded;
        let token = match repeat_token(token) {
            Some((character, count)) => {
                let count = usize::from(count.max(1)).min(usize::from(rows) * usize::from(cols));
                expanded = character.repeat(count + 1).into_bytes();
                &expanded
            }
            None => token,
        };
        // Plain text goes in a character at a time so a tear that a later
        // character papers over is still seen.
        if !token.contains(&0x1b)
            && let Ok(text) = std::str::from_utf8(token)
        {
            let mut buffer = [0; 4];
            for character in text.chars() {
                // alacritty's insert mode mishandles wide characters (it tears
                // them at the edge and skips the insert after a wrap).
                if reference.insert_mode()
                    && unicode_width::UnicodeWidthChar::width(character) == Some(2)
                {
                    return None;
                }
                reference.feed_token(character.encode_utf8(&mut buffer).as_bytes());
                if reference.torn() {
                    return None;
                }
            }
            continue;
        }
        reference.feed_token(token);
        if reference.torn() {
            return None;
        }
    }
    Some(reference.snapshot())
}

fn disagreement(tokens: &[Vec<u8>], compare: Compare) -> Option<String> {
    reference_screen(tokens)?.diff(&vt100_screen(tokens), compare)
}

/// Runs `cases` random streams over `features` and returns a shrunk report of
/// the first disagreement.
fn explore(features: &[Feature], cases: u64, compare: Compare) -> Option<String> {
    explore_seeds(features, 0..cases, 60, compare)
}

fn explore_seeds(
    features: &[Feature],
    seeds: std::ops::Range<u64>,
    length: usize,
    compare: Compare,
) -> Option<String> {
    let (rows, cols) = size();
    for seed in seeds {
        let tokens = Generator::new(seed, rows, cols, features).tokens(length);
        if disagreement(&tokens, compare).is_some() {
            let small = shrink(tokens, |tokens| disagreement(tokens, compare).is_some());
            let report = disagreement(&small, compare).unwrap();
            return Some(format!(
                "size {rows}x{cols} seed {seed}: {}\n{report}",
                escape(&small.concat())
            ));
        }
    }
    None
}

#[test]
#[ignore = "exploratory: prints every feature's first disagreement"]
fn survey_each_feature() {
    let base = [Feature::Text, Feature::Controls, Feature::Cursor];
    for feature in Feature::ALL {
        let mut features = base.to_vec();
        features.push(*feature);
        for (name, compare) in [("text", Compare::TEXT), ("full", Compare::FULL)] {
            match explore(&features, 300, compare) {
                None => println!("== {feature:?} [{name}]: agree"),
                Some(report) => println!("== {feature:?} [{name}]: DISAGREE\n{report}"),
            }
        }
    }
}

#[test]
#[ignore = "debug helper: MUX_CASE is one stream, escaped as the reports print it"]
fn show_case() {
    let case = std::env::var("MUX_CASE").expect("set MUX_CASE");
    let mut bytes = Vec::new();
    let mut rest = case.as_str();
    while let Some(index) = rest.find('\\') {
        bytes.extend_from_slice(rest[..index].as_bytes());
        rest = &rest[index + 1..];
        let (byte, used) = match rest.as_bytes().first() {
            Some(b'e') => (0x1b, 1),
            Some(b'r') => (b'\r', 1),
            Some(b'n') => (b'\n', 1),
            Some(b't') => (b'\t', 1),
            Some(b'x') => (u8::from_str_radix(&rest[1..3], 16).unwrap(), 3),
            _ => (b'\\', 0),
        };
        bytes.push(byte);
        rest = &rest[used..];
    }
    bytes.extend_from_slice(rest.as_bytes());
    match disagreement(&[bytes], Compare::FULL) {
        None => println!("agree"),
        Some(report) => println!("{report}"),
    }
}

/// Every feature at once, at sizes from a single cell upwards, so feature
/// interactions (charsets under insert mode, REP across a wrap, scrolling
/// regions with origin mode, ...) are covered too.
#[test]
fn vt100_matches_reference_on_mixed_streams() {
    let seeds: u64 = std::env::var("MUX_FUZZ_SEEDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(400);
    for (rows, cols) in [(8, 20), (24, 80), (3, 7), (1, 2), (2, 2), (1, 30), (40, 3)] {
        SIZE.with(|size| size.set((rows, cols)));
        if let Some(report) = explore_seeds(Feature::ALL, 0..seeds, 120, Compare::FULL) {
            panic!("{report}");
        }
    }
}

/// Output, a resize, more output: the screen after must match a reflowing
/// terminal's. Both keep history, since growing pulls it back into view.
fn resize_disagreement(
    before: &[Vec<u8>],
    sizes: &[(u16, u16)],
    after: &[Vec<u8>],
) -> Option<String> {
    let (rows, cols) = size();
    let mut reference = Reference::with_history(rows as usize, cols as usize, 1000);
    let mut parser = vt100::Parser::new(rows, cols, 1000);
    for token in before {
        reference.feed_token(token);
        parser.process(token);
    }
    for (rows, cols) in sizes {
        reference.resize(*rows as usize, *cols as usize);
        parser.screen_mut().set_size(*rows, *cols);
    }
    for token in after {
        reference.feed_token(token);
        parser.process(token);
    }
    if reference.torn() {
        return None;
    }
    reference
        .snapshot()
        .diff(&Snapshot::from_vt100(parser.screen()), Compare::TEXT)
}

#[test]
#[ignore = "exploratory: how resizing compares with a reflowing terminal"]
fn survey_resizes() {
    use rand::{Rng, SeedableRng, rngs::SmallRng};
    // Shell-like output only: text, newlines, colours. Full-screen programs
    // redraw after a resize, so only what a shell leaves behind matters.
    let features = [Feature::Text, Feature::Unicode, Feature::Sgr];
    let mut failures = 0;
    for seed in 0..400u64 {
        let mut rng = SmallRng::seed_from_u64(seed);
        let (rows, cols) = (rng.random_range(3..12), rng.random_range(5..30));
        SIZE.with(|size| size.set((rows, cols)));
        let mut generator = Generator::new(seed, rows, cols, &features);
        let lines = rng.random_range(0..30);
        let mut before: Vec<Vec<u8>> = Vec::new();
        for _ in 0..lines {
            before.extend(generator.tokens(rng.random_range(1..4)));
            before.push(b"\r\n".to_vec());
        }
        let sizes: Vec<(u16, u16)> = (0..rng.random_range(1..3))
            .map(|_| (rng.random_range(2..14), rng.random_range(4..34)))
            .collect();
        let after: Vec<Vec<u8>> = generator.tokens(2);
        if let Some(report) = resize_disagreement(&before, &sizes, &after) {
            println!(
                "seed {seed} {rows}x{cols} -> {sizes:?}\nbefore {}\nafter {}\n{report}",
                escape(&before.concat()),
                escape(&after.concat())
            );
            failures += 1;
            if failures == 8 {
                return;
            }
        }
    }
    println!("all resizes agree");
}
