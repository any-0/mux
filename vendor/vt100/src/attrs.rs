/// Represents a foreground or background color for cells.
#[derive(Eq, PartialEq, Debug, Copy, Clone, Default)]
pub enum Color {
    /// The default terminal color.
    #[default]
    Default,

    /// An indexed terminal color.
    Idx(u8),

    /// An RGB terminal color. The parameters are (red, green, blue).
    Rgb(u8, u8, u8),
}

// The bits of `mode` are part of the packed scrollback format.
const TEXT_MODE_INTENSITY: u8 = 0b0000_0011;
const TEXT_MODE_BOLD: u8 = 0b0000_0001;
const TEXT_MODE_DIM: u8 = 0b0000_0010;
const TEXT_MODE_ITALIC: u8 = 0b0000_0100;
const TEXT_MODE_UNDERLINE: u8 = 0b0000_1000;
const TEXT_MODE_INVERSE: u8 = 0b0001_0000;
const TEXT_MODE_UNDERLINE_STYLE: u8 = 0b1110_0000;

// Rendition flags that do not fit in `mode`. Kept in a separate byte so the
// packed scrollback format only grows for rows that actually use them.
const EXTRA_STRIKETHROUGH: u8 = 0b0000_0001;
const EXTRA_BLINK: u8 = 0b0000_0010;
const EXTRA_HIDDEN: u8 = 0b0000_0100;
const EXTRA_OVERLINE: u8 = 0b0000_1000;
pub const EXTRA_ALL: u8 = 0b0000_1111;

/// The visual form of an underline.
#[derive(Eq, PartialEq, Debug, Copy, Clone, Default)]
#[repr(u8)]
pub enum UnderlineStyle {
    /// No underline.
    #[default]
    None = 0,
    /// A single straight line.
    Straight = 1,
    /// Two straight lines.
    Double = 2,
    /// A wavy line.
    Curly = 3,
    /// A dotted line.
    Dotted = 4,
    /// A dashed line.
    Dashed = 5,
}

impl From<UnderlineStyle> for u8 {
    #[allow(clippy::as_conversions)] // a fieldless `repr(u8)` enum
    fn from(style: UnderlineStyle) -> Self {
        style as Self
    }
}

impl UnderlineStyle {
    /// The style `SGR 4:<value>` selects.
    pub(crate) fn from_sgr(value: u16) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::Straight),
            2 => Some(Self::Double),
            3 => Some(Self::Curly),
            4 => Some(Self::Dotted),
            5 => Some(Self::Dashed),
            _ => None,
        }
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attrs {
    pub fgcolor: Color,
    pub bgcolor: Color,
    pub underline_color: Color,
    pub mode: u8,
    pub extra: u8,
}

fn set_bit(bits: &mut u8, flag: u8, value: bool) {
    if value {
        *bits |= flag;
    } else {
        *bits &= !flag;
    }
}

impl Attrs {
    pub fn strikethrough(&self) -> bool {
        self.extra & EXTRA_STRIKETHROUGH != 0
    }

    pub fn set_strikethrough(&mut self, value: bool) {
        set_bit(&mut self.extra, EXTRA_STRIKETHROUGH, value);
    }

    pub fn blink(&self) -> bool {
        self.extra & EXTRA_BLINK != 0
    }

    pub fn set_blink(&mut self, value: bool) {
        set_bit(&mut self.extra, EXTRA_BLINK, value);
    }

    pub fn hidden(&self) -> bool {
        self.extra & EXTRA_HIDDEN != 0
    }

    pub fn set_hidden(&mut self, value: bool) {
        set_bit(&mut self.extra, EXTRA_HIDDEN, value);
    }

    pub fn overline(&self) -> bool {
        self.extra & EXTRA_OVERLINE != 0
    }

    pub fn set_overline(&mut self, value: bool) {
        set_bit(&mut self.extra, EXTRA_OVERLINE, value);
    }

    pub fn bold(&self) -> bool {
        self.mode & TEXT_MODE_BOLD != 0
    }

    pub fn dim(&self) -> bool {
        self.mode & TEXT_MODE_DIM != 0
    }

    // Bold and faint are independent, as in xterm: SGR 1 and SGR 2 each add
    // one, and only SGR 22 takes them away.
    pub fn set_bold(&mut self) {
        self.mode |= TEXT_MODE_BOLD;
    }

    pub fn set_dim(&mut self) {
        self.mode |= TEXT_MODE_DIM;
    }

    pub fn set_normal_intensity(&mut self) {
        self.mode &= !TEXT_MODE_INTENSITY;
    }

    pub fn italic(&self) -> bool {
        self.mode & TEXT_MODE_ITALIC != 0
    }

    pub fn set_italic(&mut self, italic: bool) {
        set_bit(&mut self.mode, TEXT_MODE_ITALIC, italic);
    }

    pub fn underline(&self) -> bool {
        self.underline_style() != UnderlineStyle::None
    }

    pub fn set_underline(&mut self, underline: bool) {
        self.set_underline_style(if underline {
            UnderlineStyle::Straight
        } else {
            UnderlineStyle::None
        });
    }

    pub fn underline_style(&self) -> UnderlineStyle {
        UnderlineStyle::from_sgr(u16::from(self.mode >> 5)).unwrap_or_default()
    }

    pub fn set_underline_style(&mut self, style: UnderlineStyle) {
        self.mode &= !(TEXT_MODE_UNDERLINE | TEXT_MODE_UNDERLINE_STYLE);
        self.mode |= u8::from(style) << 5;
        if style != UnderlineStyle::None {
            self.mode |= TEXT_MODE_UNDERLINE;
        }
    }

    pub fn inverse(&self) -> bool {
        self.mode & TEXT_MODE_INVERSE != 0
    }

    pub fn set_inverse(&mut self, inverse: bool) {
        set_bit(&mut self.mode, TEXT_MODE_INVERSE, inverse);
    }

    /// Writes the SGR sequence that changes `other` into `self`.
    pub fn write_escape_code_diff(&self, contents: &mut Vec<u8>, other: &Self) {
        if self != other && self == &Self::default() {
            crate::term::clear_attrs(contents);
            return;
        }

        let mut sgr = crate::term::Sgr::new(contents);
        if self.fgcolor != other.fgcolor {
            sgr.color(self.fgcolor, 30);
        }
        if self.bgcolor != other.bgcolor {
            sgr.color(self.bgcolor, 40);
        }
        if self.underline_color != other.underline_color {
            sgr.color(self.underline_color, 50);
        }
        if self.mode & TEXT_MODE_INTENSITY != other.mode & TEXT_MODE_INTENSITY {
            // Bold and faint are independent, so each state is written from
            // a clean slate.
            sgr.param(22);
            if self.bold() {
                sgr.param(1);
            }
            if self.dim() {
                sgr.param(2);
            }
        }
        if self.italic() != other.italic() {
            sgr.flag(self.italic(), 3, 23);
        }
        if self.underline_style() != other.underline_style() {
            sgr.underline(self.underline_style());
        }
        for (value, prev, on, off) in [
            (self.inverse(), other.inverse(), 7, 27),
            (self.blink(), other.blink(), 5, 25),
            (self.hidden(), other.hidden(), 8, 28),
            (self.strikethrough(), other.strikethrough(), 9, 29),
            (self.overline(), other.overline(), 53, 55),
        ] {
            if value != prev {
                sgr.flag(value, on, off);
            }
        }
        sgr.finish();
    }
}

#[cfg(test)]
mod intensity_tests {
    #[test]
    fn formatted_intensity_transitions_clear_old_bits_before_setting_new_ones() {
        // Every pair is needed: emitting only SGR 2 after SGR 1 accumulates
        // bold+faint on independent terminal emulators.
        for old in [b"0".as_slice(), b"1", b"2", b"1;2"] {
            for new in [b"0".as_slice(), b"1", b"2", b"1;2"] {
                let mut parser = crate::Parser::new(3, 20, 0);
                parser.process(b"\x1b[3m"); // avoid the all-default fast path
                parser.process(b"\x1b[");
                parser.process(old);
                parser.process(b"mA\x1b[22;3m\x1b[");
                parser.process(new);
                parser.process(b"mB");
                let mut decoded = crate::Parser::new(3, 20, 0);
                decoded.process(&parser.screen().contents_formatted());
                for col in 0..2 {
                    assert_eq!(parser.screen().cell(0, col), decoded.screen().cell(0, col));
                }
            }
        }
    }
}
