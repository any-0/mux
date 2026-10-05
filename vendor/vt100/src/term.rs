//! The escape sequences the formatted and diff output is made of.

use crate::grid::Pos;

pub fn clear_screen(buf: &mut Vec<u8>) {
    buf.extend_from_slice(b"\x1b[H\x1b[J");
}

pub fn clear_row_forward(buf: &mut Vec<u8>) {
    buf.extend_from_slice(b"\x1b[K");
}

pub fn backspace(buf: &mut Vec<u8>) {
    buf.push(b'\x08');
}

pub fn save_cursor(buf: &mut Vec<u8>) {
    buf.extend_from_slice(b"\x1b7");
}

pub fn restore_cursor(buf: &mut Vec<u8>) {
    buf.extend_from_slice(b"\x1b8");
}

pub fn clear_attrs(buf: &mut Vec<u8>) {
    buf.extend_from_slice(b"\x1b[m");
}

pub fn hide_cursor(buf: &mut Vec<u8>, hide: bool) {
    buf.extend_from_slice(if hide { b"\x1b[?25l" } else { b"\x1b[?25h" });
}

/// `CSI <count> <c>`, with the count left out when it is 1 and nothing at
/// all written when it is 0.
fn counted(buf: &mut Vec<u8>, count: u16, c: u8) {
    match count {
        0 => {}
        1 => buf.extend_from_slice(&[b'\x1b', b'[', c]),
        n => {
            buf.extend_from_slice(b"\x1b[");
            extend_itoa(buf, n);
            buf.push(c);
        }
    }
}

pub fn erase_char(buf: &mut Vec<u8>, count: u16) {
    counted(buf, count, b'X');
}

fn move_to(buf: &mut Vec<u8>, to: Pos) {
    if to.row == 0 && to.col == 0 {
        buf.extend_from_slice(b"\x1b[H");
    } else {
        buf.extend_from_slice(b"\x1b[");
        extend_itoa(buf, to.row + 1);
        buf.push(b';');
        extend_itoa(buf, to.col + 1);
        buf.push(b'H');
    }
}

/// The shortest way from `from` to `to` among CRLF, CUF and CUP.
pub fn move_from_to(buf: &mut Vec<u8>, from: Pos, to: Pos) {
    if to.row == from.row + 1 && to.col == 0 {
        buf.extend_from_slice(b"\r\n");
    } else if from.row == to.row && from.col < to.col {
        counted(buf, to.col - from.col, b'C');
    } else if to != from {
        move_to(buf, to);
    }
}

pub fn mouse_protocol_mode(
    buf: &mut Vec<u8>,
    mode: crate::MouseProtocolMode,
    prev: crate::MouseProtocolMode,
) {
    use crate::MouseProtocolMode as Mode;
    let (param, set) = if mode == prev {
        return;
    } else if mode == Mode::None {
        (prev, false)
    } else {
        (mode, true)
    };
    buf.extend_from_slice(match param {
        Mode::None => unreachable!(),
        Mode::Press => b"\x1b[?9",
        Mode::PressRelease => b"\x1b[?1000",
        Mode::ButtonMotion => b"\x1b[?1002",
        Mode::AnyMotion => b"\x1b[?1003",
    });
    buf.push(if set { b'h' } else { b'l' });
}

pub fn mouse_protocol_encoding(
    buf: &mut Vec<u8>,
    encoding: crate::MouseProtocolEncoding,
    prev: crate::MouseProtocolEncoding,
) {
    use crate::MouseProtocolEncoding as Encoding;
    let (param, set) = if encoding == prev {
        return;
    } else if encoding == Encoding::Default {
        (prev, false)
    } else {
        (encoding, true)
    };
    buf.extend_from_slice(match param {
        Encoding::Default => unreachable!(),
        Encoding::Utf8 => b"\x1b[?1005",
        Encoding::Sgr => b"\x1b[?1006",
    });
    buf.push(if set { b'h' } else { b'l' });
}

/// An SGR sequence, opened by its first parameter and closed by
/// [`Sgr::finish`] only if it has one.
pub struct Sgr<'a> {
    buf: &'a mut Vec<u8>,
    started: bool,
}

impl<'a> Sgr<'a> {
    pub fn new(buf: &'a mut Vec<u8>) -> Self {
        Self {
            buf,
            started: false,
        }
    }

    /// Starts the next parameter and returns the buffer to write it to.
    fn next(&mut self) -> &mut Vec<u8> {
        self.buf
            .extend_from_slice(if self.started { b";" } else { b"\x1b[" });
        self.started = true;
        self.buf
    }

    pub fn param(&mut self, i: u8) {
        extend_itoa(self.next(), i);
    }

    pub fn params(&mut self, params: &[u8]) {
        for param in params {
            self.param(*param);
        }
    }

    pub fn flag(&mut self, value: bool, on: u8, off: u8) {
        self.param(if value { on } else { off });
    }

    /// Writes `color` for SGR `base` (30 foreground, 40 background, 50
    /// underline). The underline colour has no short indexed forms.
    pub fn color(&mut self, color: crate::Color, base: u8) {
        match color {
            crate::Color::Default => self.param(base + 9),
            crate::Color::Idx(i) if i < 8 && base != 50 => self.param(base + i),
            crate::Color::Idx(i) if i < 16 && base != 50 => {
                self.param(base + 52 + i);
            }
            crate::Color::Idx(i) => self.params(&[base + 8, 5, i]),
            crate::Color::Rgb(r, g, b) => self.params(&[base + 8, 2, r, g, b]),
        }
    }

    pub fn underline(&mut self, style: crate::attrs::UnderlineStyle) {
        match style {
            crate::attrs::UnderlineStyle::None => self.param(24),
            crate::attrs::UnderlineStyle::Straight => self.param(4),
            style => {
                let buf = self.next();
                buf.extend_from_slice(b"4:");
                extend_itoa(buf, u8::from(style));
            }
        }
    }

    pub fn finish(self) {
        if self.started {
            self.buf.push(b'm');
        }
    }
}

fn extend_itoa<I: itoa::Integer>(buf: &mut Vec<u8>, i: I) {
    let mut itoa_buf = itoa::Buffer::new();
    buf.extend_from_slice(itoa_buf.format(i).as_bytes());
}
