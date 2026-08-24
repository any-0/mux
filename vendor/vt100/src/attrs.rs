use crate::term::BufWrite as _;

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

const TEXT_MODE_INTENSITY: u8 = 0b0000_0011;
const TEXT_MODE_BOLD: u8 = 0b0000_0001;
const TEXT_MODE_DIM: u8 = 0b0000_0010;
const TEXT_MODE_ITALIC: u8 = 0b0000_0100;
const TEXT_MODE_UNDERLINE: u8 = 0b0000_1000;
const TEXT_MODE_INVERSE: u8 = 0b0001_0000;
const TEXT_MODE_UNDERLINE_STYLE: u8 = 0b1110_0000;

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

impl UnderlineStyle {
    fn from_sgr(value: u16) -> Option<Self> {
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
    pub mode: u8,
}

impl Attrs {
    pub fn bold(&self) -> bool {
        self.mode & TEXT_MODE_BOLD != 0
    }

    pub fn dim(&self) -> bool {
        self.mode & TEXT_MODE_DIM != 0
    }

    fn intensity(&self) -> u8 {
        self.mode & TEXT_MODE_INTENSITY
    }

    pub fn set_bold(&mut self) {
        self.mode &= !TEXT_MODE_INTENSITY;
        self.mode |= TEXT_MODE_BOLD;
    }

    pub fn set_dim(&mut self) {
        self.mode &= !TEXT_MODE_INTENSITY;
        self.mode |= TEXT_MODE_DIM;
    }

    pub fn set_normal_intensity(&mut self) {
        self.mode &= !TEXT_MODE_INTENSITY;
    }

    pub fn italic(&self) -> bool {
        self.mode & TEXT_MODE_ITALIC != 0
    }

    pub fn set_italic(&mut self, italic: bool) {
        if italic {
            self.mode |= TEXT_MODE_ITALIC;
        } else {
            self.mode &= !TEXT_MODE_ITALIC;
        }
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
        match (self.mode & TEXT_MODE_UNDERLINE_STYLE) >> 5 {
            1 => UnderlineStyle::Straight,
            2 => UnderlineStyle::Double,
            3 => UnderlineStyle::Curly,
            4 => UnderlineStyle::Dotted,
            5 => UnderlineStyle::Dashed,
            _ => UnderlineStyle::None,
        }
    }

    pub fn set_underline_style(&mut self, style: UnderlineStyle) {
        self.mode &= !(TEXT_MODE_UNDERLINE | TEXT_MODE_UNDERLINE_STYLE);
        self.mode |= (style as u8) << 5;
        if style != UnderlineStyle::None {
            self.mode |= TEXT_MODE_UNDERLINE;
        }
    }

    pub fn set_underline_style_sgr(&mut self, value: u16) -> bool {
        let Some(style) = UnderlineStyle::from_sgr(value) else {
            return false;
        };
        self.set_underline_style(style);
        true
    }

    pub fn inverse(&self) -> bool {
        self.mode & TEXT_MODE_INVERSE != 0
    }

    pub fn set_inverse(&mut self, inverse: bool) {
        if inverse {
            self.mode |= TEXT_MODE_INVERSE;
        } else {
            self.mode &= !TEXT_MODE_INVERSE;
        }
    }

    pub fn write_escape_code_diff(
        &self,
        contents: &mut Vec<u8>,
        other: &Self,
    ) {
        if self != other && self == &Self::default() {
            crate::term::ClearAttrs.write_buf(contents);
            return;
        }

        let attrs = crate::term::Attrs::default();

        let attrs = if self.fgcolor == other.fgcolor {
            attrs
        } else {
            attrs.fgcolor(self.fgcolor)
        };
        let attrs = if self.bgcolor == other.bgcolor {
            attrs
        } else {
            attrs.bgcolor(self.bgcolor)
        };
        let attrs = if self.intensity() == other.intensity() {
            attrs
        } else {
            attrs.intensity(match self.intensity() {
                0 => crate::term::Intensity::Normal,
                TEXT_MODE_BOLD => crate::term::Intensity::Bold,
                TEXT_MODE_DIM => crate::term::Intensity::Dim,
                _ => unreachable!(),
            })
        };
        let attrs = if self.italic() == other.italic() {
            attrs
        } else {
            attrs.italic(self.italic())
        };
        let attrs = if self.underline_style() == other.underline_style() {
            attrs
        } else {
            attrs.underline(self.underline_style())
        };
        let attrs = if self.inverse() == other.inverse() {
            attrs
        } else {
            attrs.inverse(self.inverse())
        };

        attrs.write_buf(contents);
    }
}
