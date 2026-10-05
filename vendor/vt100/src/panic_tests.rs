//! Nothing a program writes, at any size, through any resize, may panic the
//! emulator: in mux a panic there takes the daemon, and every shell it owns,
//! with it. Seeded and dependency-free, so a failure names its seed.

/// `SplitMix64`: small, deterministic, and good enough to pick test input.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn size(&mut self, max: u64) -> u16 {
        u16::try_from(1 + self.below(max)).unwrap()
    }
}

const PIECES: &[&str] = &[
    "a",
    "xyz ",
    "あ",
    "\u{1f600}",
    "\u{1f600}\u{fe0f}\u{200d}\u{1f525}",
    "e\u{301}",
    "\u{301}",
    "\r",
    "\n",
    "\x08",
    "\t",
    "\x1b[H",
    "\x1b[2;3H",
    "\x1b[K",
    "\x1b[1K",
    "\x1b[2J",
    "\x1b[3J",
    "\x1b[J",
    "\x1b[@",
    "\x1b[3P",
    "\x1b[2L",
    "\x1b[2M",
    "\x1b[S",
    "\x1b[T",
    "\x1b[2X",
    "\x1b[b",
    "\x1b[I",
    "\x1b[Z",
    "\x1b[2;3r",
    "\x1b[r",
    "\x1b7",
    "\x1b8",
    "\x1bM",
    "\x1bD",
    "\x1bE",
    "\x1b[?6h",
    "\x1b[?6l",
    "\x1b[?7l",
    "\x1b[?7h",
    "\x1b[4h",
    "\x1b[4l",
    "\x1b[?1049h",
    "\x1b[?1049l",
    "\x1b[?47h",
    "\x1b[?1047l",
    "\x1b[1;4:3;38;2;1;2;3;58:2::9:8:7m",
    "\x1b[0m",
    "\x1b(0lqk\x1b(B",
    "\x1b[65535;65535H",
    "\x1b[65535b",
    "\x1b[65535@",
    "\x1b[65535P",
    "\x1b[65535L",
    "\x1b[65535S",
    "\x1b[0;0r",
    "\x1b[65535;1r",
    "\x1bc",
    "\x1b[!p",
    "\x1b]2;title\x07",
    "\x1bP$qm\x1b\\",
];

fn stream(rng: &mut Rng) -> Vec<u8> {
    let mut bytes = Vec::new();
    for _ in 0..rng.below(30) {
        if rng.below(4) == 0 {
            // Raw bytes: truncated sequences, controls, invalid UTF-8.
            let byte = match rng.below(6) {
                0 => 0x1b,
                1 => b'[',
                2 => b'0' + u8::try_from(rng.below(10)).unwrap(),
                3 => u8::try_from(rng.below(0x20)).unwrap(),
                4 => 0x80 + u8::try_from(rng.below(0x80)).unwrap(),
                _ => 0x20 + u8::try_from(rng.below(0x5f)).unwrap(),
            };
            bytes.push(byte);
        } else {
            let piece =
                PIECES[usize::try_from(rng.below(u64::try_from(PIECES.len()).unwrap())).unwrap()];
            bytes.extend_from_slice(piece.as_bytes());
        }
    }
    bytes
}

/// What mux does with a screen besides drawing it: format it for the
/// journal, diff it, pack and restore its history.
fn persist(parser: &crate::Parser) {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    let formatted = screen.contents_formatted();
    let state = screen.state_formatted();
    let _ = screen.state_diff(&screen.clone());
    let blank = crate::Parser::new(rows, cols, 0);
    let _ = screen.state_diff(blank.screen());
    let _ = blank.screen().state_diff(screen);
    let mut replay = crate::Parser::new(rows, cols, 50);
    assert!(replay
        .screen_mut()
        .restore_history(&screen.encode_history()));
    replay.process(&formatted);
    replay.process(&state);
}

fn run(seed: u64) {
    let mut rng = Rng(seed);
    let scrollback = usize::try_from(rng.below(50)).unwrap();
    let mut parser = crate::Parser::new(rng.size(8), rng.size(12), scrollback);
    for _ in 0..rng.below(40) {
        match rng.below(10) {
            0..=5 => parser.process(&stream(&mut rng)),
            6 => {
                let (rows, cols) = (rng.size(10), rng.size(14));
                parser.screen_mut().set_size(rows, cols);
            }
            7 => {
                parser
                    .screen_mut()
                    .set_scrollback(usize::try_from(rng.below(100)).unwrap());
                let _ = parser.screen().contents();
                parser.screen_mut().set_scrollback(0);
            }
            _ => persist(&parser),
        }
        let (rows, cols) = parser.screen().size();
        for row in 0..rows {
            for col in 0..cols {
                let _ = parser.screen().cell(row, col);
            }
        }
    }
    persist(&parser);
}

#[test]
fn nothing_panics_the_emulator() {
    for seed in 0..3000 {
        assert!(
            std::panic::catch_unwind(|| run(seed)).is_ok(),
            "seed {seed} panicked"
        );
    }
}

#[test]
fn a_wide_character_survives_a_squeeze_to_one_column() {
    let mut parser = crate::Parser::new(3, 4, 10);
    parser.process("ああ\r\nああ".as_bytes());
    let before = parser.screen().clone();
    parser.screen_mut().set_size(3, 1);
    // Its halves sit on consecutive rows, which drawing must cope with.
    let narrow = crate::Parser::new(3, 1, 0);
    let _ = parser.screen().state_diff(narrow.screen());
    let _ = narrow.screen().state_diff(parser.screen());
    parser.screen_mut().set_size(3, 4);
    assert_eq!(parser.screen().contents(), before.contents());
}
