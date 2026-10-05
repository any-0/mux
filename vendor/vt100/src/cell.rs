use unicode_width::UnicodeWidthChar as _;

// chosen to keep the size of the cell struct bounded
const CONTENT_BYTES: usize = 22;

const IS_WIDE: u8 = 0b1000_0000;
const IS_WIDE_CONTINUATION: u8 = 0b0100_0000;
pub const LEN_BITS: u8 = 0b0001_1111;

/// Represents a single terminal cell.
#[derive(Clone, Debug, Eq)]
pub struct Cell {
    contents: [u8; CONTENT_BYTES],
    len: u8,
    attrs: crate::attrs::Attrs,
}
const _: () = assert!(std::mem::size_of::<Cell>() <= 40);

// Bytes past the length are stale and do not count.
impl PartialEq<Self> for Cell {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len
            && self.attrs == other.attrs
            && self.contents[..self.len()] == other.contents[..self.len()]
    }
}

impl Cell {
    pub(crate) fn new() -> Self {
        Self {
            contents: Default::default(),
            len: 0,
            attrs: crate::attrs::Attrs::default(),
        }
    }

    /// The length byte, with its wide flags, as packed rows store it.
    pub(crate) fn compact_len(&self) -> u8 {
        self.len
    }

    pub(crate) fn from_compact(contents: &[u8], len: u8, attrs: crate::attrs::Attrs) -> Self {
        let content_len = usize::from(len & LEN_BITS);
        let mut compact = Self {
            contents: [0; CONTENT_BYTES],
            len,
            attrs,
        };
        compact.contents[..content_len].copy_from_slice(contents);
        compact
    }

    fn len(&self) -> usize {
        usize::from(self.len & LEN_BITS)
    }

    pub(crate) fn set(&mut self, c: char, a: crate::attrs::Attrs) {
        self.len = 0;
        self.append_char(0, c);
        // A cell holds one character and the zero-width ones combined with
        // it, so the first one decides the width.
        if c.width().unwrap_or(1) > 1 {
            self.len |= IS_WIDE;
        }
        self.attrs = a;
    }

    pub(crate) fn append(&mut self, c: char) {
        let len = self.len();
        if len >= CONTENT_BYTES - 4 {
            return;
        }
        if len == 0 {
            self.contents[0] = b' ';
            self.len += 1;
        }

        // we already checked that we have space for another codepoint
        self.append_char(self.len(), c);
    }

    // The caller makes sure there is room for four more bytes at `start`.
    fn append_char(&mut self, start: usize, c: char) {
        c.encode_utf8(&mut self.contents[start..]);
        self.len += u8::try_from(c.len_utf8()).unwrap();
    }

    pub(crate) fn clear(&mut self, attrs: crate::attrs::Attrs) {
        self.len = 0;
        self.attrs = attrs;
    }

    /// Returns the text contents of the cell.
    ///
    /// Can include multiple unicode characters if combining characters are
    /// used, but will contain at most one character with a non-zero character
    /// width.
    // Since contents has been constructed by appending chars encoded as UTF-8 it will be valid UTF-8
    #[allow(clippy::missing_panics_doc)]
    #[must_use]
    pub fn contents(&self) -> &str {
        std::str::from_utf8(&self.contents[..self.len()]).unwrap()
    }

    /// Returns whether the cell contains any text data.
    #[must_use]
    pub fn has_contents(&self) -> bool {
        self.len() > 0
    }

    /// Returns whether the text data in the cell represents a wide character.
    #[must_use]
    pub fn is_wide(&self) -> bool {
        self.len & IS_WIDE != 0
    }

    /// Returns whether the cell contains the second half of a wide character
    /// (in other words, whether the previous cell in the row contains a wide
    /// character)
    #[must_use]
    pub fn is_wide_continuation(&self) -> bool {
        self.len & IS_WIDE_CONTINUATION != 0
    }

    pub(crate) fn set_wide_continuation(&mut self) {
        self.len |= IS_WIDE_CONTINUATION;
    }

    pub(crate) fn attrs(&self) -> &crate::attrs::Attrs {
        &self.attrs
    }

    /// Returns the foreground color of the cell.
    #[must_use]
    pub fn fgcolor(&self) -> crate::Color {
        self.attrs.fgcolor
    }

    /// Returns the background color of the cell.
    #[must_use]
    pub fn bgcolor(&self) -> crate::Color {
        self.attrs.bgcolor
    }

    /// Returns the explicit color of the cell's underline, if one was set.
    #[must_use]
    pub fn underline_color(&self) -> crate::Color {
        self.attrs.underline_color
    }

    /// Returns whether the cell should be rendered with the bold text
    /// attribute.
    #[must_use]
    pub fn bold(&self) -> bool {
        self.attrs.bold()
    }

    /// Returns whether the cell should be rendered with the dim text
    /// attribute.
    #[must_use]
    pub fn dim(&self) -> bool {
        self.attrs.dim()
    }

    /// Returns whether the cell should be rendered with the italic text
    /// attribute.
    #[must_use]
    pub fn italic(&self) -> bool {
        self.attrs.italic()
    }

    /// Returns whether the cell should be rendered with the underlined text
    /// attribute.
    #[must_use]
    pub fn underline(&self) -> bool {
        self.attrs.underline()
    }

    /// Returns the visual style of the cell's underline.
    #[must_use]
    pub fn underline_style(&self) -> crate::attrs::UnderlineStyle {
        self.attrs.underline_style()
    }

    /// Returns whether the cell should be rendered with the inverse text
    /// attribute.
    #[must_use]
    pub fn inverse(&self) -> bool {
        self.attrs.inverse()
    }

    /// Returns whether the cell is struck through (SGR 9).
    #[must_use]
    pub fn strikethrough(&self) -> bool {
        self.attrs.strikethrough()
    }

    /// Returns whether the cell blinks (SGR 5 or 6).
    #[must_use]
    pub fn blink(&self) -> bool {
        self.attrs.blink()
    }

    /// Returns whether the cell is concealed (SGR 8).
    #[must_use]
    pub fn hidden(&self) -> bool {
        self.attrs.hidden()
    }

    /// Returns whether the cell is overlined (SGR 53).
    #[must_use]
    pub fn overline(&self) -> bool {
        self.attrs.overline()
    }
}
