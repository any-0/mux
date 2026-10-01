//! Random terminal output shaped like what real programs write: shells,
//! editors, pagers and curses applications, rather than uniform noise that
//! mostly exercises the parser's error paths.

use rand::{Rng, SeedableRng, rngs::SmallRng, seq::IndexedRandom};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Feature {
    Text,
    Unicode,
    Controls,
    Cursor,
    Erase,
    Edit,
    Scroll,
    Sgr,
    SaveRestore,
    AutoWrap,
    Origin,
    Insert,
    AltScreen,
    Charset,
    Repeat,
    Tabs,
    Reset,
}

impl Feature {
    pub const ALL: &[Feature] = &[
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
        Feature::AltScreen,
        Feature::Charset,
        Feature::Repeat,
        Feature::Tabs,
        Feature::Reset,
    ];

    fn weight(self) -> u32 {
        match self {
            Feature::Text => 30,
            Feature::Unicode => 8,
            Feature::Controls => 10,
            Feature::Cursor => 14,
            Feature::Erase => 6,
            Feature::Edit => 6,
            Feature::Scroll => 5,
            Feature::Sgr => 12,
            Feature::SaveRestore => 3,
            Feature::AutoWrap => 2,
            Feature::Origin => 2,
            Feature::Insert => 2,
            Feature::AltScreen => 1,
            Feature::Charset => 3,
            Feature::Repeat => 3,
            Feature::Tabs => 3,
            Feature::Reset => 1,
        }
    }
}

pub struct Generator {
    rng: SmallRng,
    rows: u16,
    cols: u16,
    features: Vec<Feature>,
}

const WORDS: &[&str] = &[
    "ls",
    "-la",
    "cargo",
    "build",
    "error:",
    "warning",
    "mux",
    "pane",
    "hello",
    "world",
    "$",
    "#",
    "~/dev",
    "|",
    "->",
    "=",
    "fn",
    "main()",
    "{",
    "}",
    "0123456789",
    "the",
    "quick",
    "brown",
    "fox",
    "jumps",
];

const UNICODE: &[&str] = &[
    "é", "ñ", "ü", "ß", "λ", "Ж", "中", "文", "字", "日本", "한", "ｱ", "😀", "🚀", "✓", "→", "•",
    "─", "│", "┌", "┐", "└", "┘", "█", "▌", "e\u{301}", "a\u{308}", "\u{200b}", "Ａ",
];

impl Generator {
    pub fn new(seed: u64, rows: u16, cols: u16, features: &[Feature]) -> Self {
        Self {
            rng: SmallRng::seed_from_u64(seed),
            rows,
            cols,
            features: features.to_vec(),
        }
    }

    /// `count` escape sequences and text runs, kept apart so a failing
    /// stream can be shrunk token by token.
    pub fn tokens(&mut self, count: usize) -> Vec<Vec<u8>> {
        (0..count)
            .map(|_| {
                let feature = *self
                    .features
                    .choose_weighted(&mut self.rng, |feature| feature.weight())
                    .unwrap();
                let mut out = Vec::new();
                self.token(feature, &mut out);
                out
            })
            .collect()
    }

    pub fn stream(&mut self, count: usize) -> Vec<u8> {
        self.tokens(count).concat()
    }

    fn n(&mut self, max: u16) -> u16 {
        self.rng.random_range(0..=max)
    }

    fn row(&mut self) -> u16 {
        // Mostly in range, sometimes past the edge to test clamping.
        if self.rng.random_bool(0.1) {
            self.rng.random_range(0..=self.rows + 5)
        } else {
            self.rng.random_range(1..=self.rows)
        }
    }

    fn col(&mut self) -> u16 {
        if self.rng.random_bool(0.1) {
            self.rng.random_range(0..=self.cols + 5)
        } else {
            self.rng.random_range(1..=self.cols)
        }
    }

    fn csi(out: &mut Vec<u8>, body: &str) {
        out.extend_from_slice(b"\x1b[");
        out.extend_from_slice(body.as_bytes());
    }

    fn token(&mut self, feature: Feature, out: &mut Vec<u8>) {
        let rng = &mut self.rng;
        match feature {
            Feature::Text => {
                let count = rng.random_range(1..6);
                for index in 0..count {
                    if index > 0 {
                        out.push(b' ');
                    }
                    out.extend_from_slice(WORDS.choose(rng).unwrap().as_bytes());
                }
                if rng.random_bool(0.05) {
                    // A long run that has to wrap.
                    let len = rng.random_range(self.cols as usize..self.cols as usize * 2);
                    out.extend((0..len).map(|i| b'a' + (i % 26) as u8));
                }
            }
            Feature::Unicode => {
                out.extend_from_slice(UNICODE.choose(rng).unwrap().as_bytes());
            }
            Feature::Controls => {
                let control: &[u8] = [
                    &b"\r"[..],
                    b"\n",
                    b"\r\n",
                    b"\x08",
                    b"\t",
                    b"\x07",
                    b"\x0b",
                    b"\x0c",
                ]
                .choose(rng)
                .unwrap();
                out.extend_from_slice(control);
            }
            Feature::Cursor => {
                let choice = self.n(10);
                let body = match choice {
                    0 | 1 => {
                        let (row, col) = (self.row(), self.col());
                        format!("{row};{col}H")
                    }
                    2 => format!("{}A", self.n(5)),
                    3 => format!("{}B", self.n(5)),
                    4 => format!("{}C", self.n(8)),
                    5 => format!("{}D", self.n(8)),
                    6 => format!("{}G", self.col()),
                    7 => format!("{}d", self.row()),
                    8 => format!("{}E", self.n(3)),
                    9 => format!("{}F", self.n(3)),
                    _ => "H".into(),
                };
                Self::csi(out, &body);
            }
            Feature::Erase => {
                let body = match self.n(6) {
                    0 => format!("{}J", self.n(2)),
                    1 | 2 => format!("{}K", self.n(2)),
                    3 | 4 => format!("{}X", self.n(10)),
                    5 => "K".into(),
                    _ => "J".into(),
                };
                Self::csi(out, &body);
            }
            Feature::Edit => {
                // DEC resets the column after IL and DL; alacritty does not.
                // Every real program positions the cursor next anyway, so
                // the generated stream does too, and the difference is moot.
                let body = match self.n(3) {
                    0 => format!("{}@", self.n(6)),
                    1 => format!("{}P", self.n(6)),
                    2 => format!("{}L\r", self.n(3)),
                    _ => format!("{}M\r", self.n(3)),
                };
                Self::csi(out, &body);
            }
            Feature::Scroll => match self.n(6) {
                0 | 1 => {
                    let top = self.rng.random_range(1..=self.rows);
                    let bottom = self.rng.random_range(top..=self.rows);
                    if self.rng.random_bool(0.2) {
                        Self::csi(out, "r");
                    } else {
                        Self::csi(out, &format!("{top};{bottom}r"));
                    }
                }
                2 => Self::csi(out, &format!("{}S", self.n(3))),
                3 => Self::csi(out, &format!("{}T", self.n(3))),
                4 => out.extend_from_slice(b"\x1bM"),
                5 => out.extend_from_slice(b"\x1bD"),
                _ => out.extend_from_slice(b"\x1bE"),
            },
            Feature::Sgr => {
                let mut params: Vec<String> = Vec::new();
                for _ in 0..self.rng.random_range(1..4) {
                    let parameter = match self.n(16) {
                        0 => "0".to_string(),
                        1 => "1".into(),
                        2 => "2".into(),
                        3 => "3".into(),
                        4 => "4".into(),
                        5 => "7".into(),
                        6 => "9".into(),
                        7 => format!("{}", 30 + self.n(7)),
                        8 => format!("{}", 40 + self.n(7)),
                        9 => format!("{}", 90 + self.n(7)),
                        10 => format!("38;5;{}", self.n(255)),
                        11 => format!("48;5;{}", self.n(255)),
                        12 => format!("38;2;{};{};{}", self.n(255), self.n(255), self.n(255)),
                        13 => format!("48;2;{};{};{}", self.n(255), self.n(255), self.n(255)),
                        14 => ["22", "23", "24", "27", "29", "39", "49"]
                            .choose(&mut self.rng)
                            .unwrap()
                            .to_string(),
                        15 => format!("4:{}", self.n(3)),
                        _ => "".into(),
                    };
                    params.push(parameter);
                }
                Self::csi(out, &format!("{}m", params.join(";")));
            }
            Feature::SaveRestore => {
                let sequence: &[u8] = [&b"\x1b7"[..], b"\x1b8", b"\x1b[s", b"\x1b[u"]
                    .choose(rng)
                    .unwrap();
                out.extend_from_slice(sequence);
            }
            Feature::AutoWrap => {
                Self::csi(out, if rng.random_bool(0.7) { "?7h" } else { "?7l" });
            }
            Feature::Origin => {
                // DECOM homes the cursor in DEC terminals but not alacritty;
                // home it explicitly so only origin-relative addressing is
                // under test.
                Self::csi(
                    out,
                    if rng.random_bool(0.5) {
                        "?6h\x1b[H"
                    } else {
                        "?6l\x1b[H"
                    },
                );
            }
            Feature::Insert => {
                Self::csi(out, if rng.random_bool(0.5) { "4h" } else { "4l" });
            }
            Feature::AltScreen => {
                // alacritty implements only 1049 of the alternate screen modes;
                // 47 and 1047 are covered by mux's own unit tests.
                let body = ["?1049h", "?1049l"].choose(rng).unwrap();
                Self::csi(out, body);
            }
            Feature::Charset => {
                let sequence: &[u8] = [
                    &b"\x1b(0"[..],
                    b"\x1b(B",
                    b"\x1b)0",
                    b"\x0e",
                    b"\x0f",
                    b"lqqk",
                    b"x  x",
                    b"mqqj",
                    b"tqnqu",
                ]
                .choose(rng)
                .unwrap();
                out.extend_from_slice(sequence);
            }
            Feature::Repeat => {
                let character = *[b'-', b'=', b'x', b' '].choose(rng).unwrap();
                out.push(character);
                Self::csi(out, &format!("{}b", self.n(20)));
            }
            Feature::Tabs => {
                let sequence = match self.n(5) {
                    0 => "\x1bH".to_string(),
                    1 => "\x1b[g".into(),
                    2 => "\x1b[3g".into(),
                    3 => format!("\x1b[{}I", self.n(3)),
                    4 => format!("\x1b[{}Z", self.n(3)),
                    _ => "\t".into(),
                };
                out.extend_from_slice(sequence.as_bytes());
            }
            Feature::Reset => {
                out.extend_from_slice(if rng.random_bool(0.7) {
                    b"\x1b[!p"
                } else {
                    b"\x1bc"
                });
            }
        }
    }
}
