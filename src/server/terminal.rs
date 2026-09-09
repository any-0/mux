//! The glue between a pane's PTY and the `vt100` parser: what the program
//! inside is told, and what mux makes of what it says back.

use base64::{Engine, engine::general_purpose::STANDARD};

use crate::{
    config::Theme,
    frame::{CursorShape, Rgb},
    protocol::{ALT, CTRL, Key, KeyCode, Mouse, MouseButton, MouseKind, SHIFT},
};

pub(super) const SCROLLBACK_LINES: usize = 20_000;

#[derive(Default)]
pub(super) struct TerminalCallbacks {
    pub(super) bell_count: u64,
    pub(super) prompt_checkpoint: Option<vt100::Screen>,
    pub(super) prompt_ready: Option<PromptReady>,
    pub(super) cursor_shape: CursorShape,
    /// The title the program in this pane last set, which is what a window
    /// with no name of its own is called.
    pub(super) title: Option<String>,
    pub(super) clipboard_writes: Vec<ClipboardWrite>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct ClipboardWrite {
    pub(super) selection: Vec<u8>,
    pub(super) data: Vec<u8>,
}

pub(super) struct PromptReady {
    pub(super) cursor: (u16, u16),
    pub(super) row: String,
}

impl vt100::Callbacks for TerminalCallbacks {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell_count += 1;
    }

    fn visual_bell(&mut self, _: &mut vt100::Screen) {
        self.bell_count += 1;
    }

    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, selection: &[u8], data: &[u8]) {
        if let Ok(data) = STANDARD.decode(data) {
            self.clipboard_writes.push(ClipboardWrite {
                selection: selection.to_vec(),
                data,
            });
        }
    }

    /// OSC 0 and OSC 2 both arrive here; a window with no name of its own goes
    /// by whatever its active pane last called itself.
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        let title = String::from_utf8_lossy(title).trim().to_string();
        self.title = (!title.is_empty()).then_some(title);
    }

    fn unhandled_csi(
        &mut self,
        _: &mut vt100::Screen,
        first_intermediate: Option<u8>,
        second_intermediate: Option<u8>,
        params: &[&[u16]],
        final_character: char,
    ) {
        if first_intermediate != Some(b' ')
            || second_intermediate.is_some()
            || final_character != 'q'
        {
            return;
        }
        let style = params
            .first()
            .and_then(|param| param.first())
            .copied()
            .unwrap_or(0);
        self.cursor_shape = match style {
            0..=2 => CursorShape::Block,
            3 | 4 => CursorShape::Underline,
            5 | 6 => CursorShape::Bar,
            _ => return,
        };
    }

    fn unhandled_osc(&mut self, screen: &mut vt100::Screen, params: &[&[u8]]) {
        match params {
            [b"777", b"mux-prompt-start"] => {
                self.prompt_checkpoint = Some(screen.clone());
                self.prompt_ready = None;
            }
            [b"777", b"mux-prompt-ready"] => {
                self.prompt_ready = Some(PromptReady {
                    cursor: screen.cursor_position(),
                    row: cursor_row(screen),
                });
            }
            [b"50", value] if value.starts_with(b"CursorShape=") => {
                self.cursor_shape = match value.get(12) {
                    Some(b'0') => CursorShape::Block,
                    Some(b'1') => CursorShape::Bar,
                    Some(b'2') => CursorShape::Underline,
                    _ => return,
                };
            }
            _ => {}
        }
    }
}

/// The bytes a program expects for `mouse`, or `None` when it has not asked
/// for mouse reporting at all.
///
/// `mouse` is positioned within the pane, counting from zero.
pub(super) fn mouse_report(screen: &vt100::Screen, mouse: Mouse) -> Option<Vec<u8>> {
    use vt100::{MouseProtocolEncoding, MouseProtocolMode};

    let mode = screen.mouse_protocol_mode();
    let wanted = match mouse.kind {
        MouseKind::Down | MouseKind::ScrollUp | MouseKind::ScrollDown => {
            mode != MouseProtocolMode::None
        }
        MouseKind::Up => matches!(
            mode,
            MouseProtocolMode::PressRelease
                | MouseProtocolMode::ButtonMotion
                | MouseProtocolMode::AnyMotion
        ),
        MouseKind::Drag => matches!(
            mode,
            MouseProtocolMode::ButtonMotion | MouseProtocolMode::AnyMotion
        ),
    };
    if !wanted {
        return None;
    }
    let button = match mouse.kind {
        MouseKind::ScrollUp => 64,
        MouseKind::ScrollDown => 65,
        _ => match mouse.button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        },
    };
    let button = button
        + if matches!(mouse.kind, MouseKind::Drag) {
            32
        } else {
            0
        }
        + if mouse.modifiers & SHIFT != 0 { 4 } else { 0 }
        + if mouse.modifiers & ALT != 0 { 8 } else { 0 }
        + if mouse.modifiers & CTRL != 0 { 16 } else { 0 };
    let (col, row) = (mouse.col + 1, mouse.row + 1);
    match screen.mouse_protocol_encoding() {
        MouseProtocolEncoding::Sgr => {
            let final_byte = if matches!(mouse.kind, MouseKind::Up) {
                'm'
            } else {
                'M'
            };
            Some(format!("\x1b[<{button};{col};{row}{final_byte}").into_bytes())
        }
        // The original encoding has one byte per field and cannot describe a
        // release, so it reports the generic button-up code instead.
        MouseProtocolEncoding::Default | MouseProtocolEncoding::Utf8 => {
            if col > 223 || row > 223 {
                return None;
            }
            let button = if matches!(mouse.kind, MouseKind::Up) {
                3
            } else {
                button
            };
            let mut report = b"\x1b[M".to_vec();
            report.push(32u8.saturating_add(button as u8));
            report.push(32 + col as u8);
            report.push(32 + row as u8);
            Some(report)
        }
    }
}

pub(super) fn new_parser(rows: u16, cols: u16) -> vt100::Parser<TerminalCallbacks> {
    vt100::Parser::new_with_callbacks(
        rows.max(1),
        cols.max(1),
        SCROLLBACK_LINES,
        TerminalCallbacks::default(),
    )
}

pub(super) fn cursor_row(screen: &vt100::Screen) -> String {
    let (row, _) = screen.cursor_position();
    let (_, cols) = screen.size();
    screen
        .rows(0, cols)
        .nth(usize::from(row))
        .unwrap_or_default()
}

pub(super) fn restored_prompt_correction(
    parser: &vt100::Parser<TerminalCallbacks>,
) -> Option<Vec<u8>> {
    let screen = parser.screen();
    let callbacks = parser.callbacks();
    if callbacks.prompt_checkpoint.is_some() || callbacks.prompt_ready.is_some() {
        let ready = callbacks.prompt_ready.as_ref()?;
        if screen.alternate_screen()
            || !screen.bracketed_paste()
            || screen.cursor_position() != ready.cursor
            || cursor_row(screen) != ready.row
        {
            return None;
        }
        let correction = callbacks.prompt_checkpoint.as_ref()?.state_diff(screen);
        return (!correction.is_empty()).then_some(correction);
    }
    legacy_idle_prompt_correction(screen)
}

fn legacy_idle_prompt_correction(screen: &vt100::Screen) -> Option<Vec<u8>> {
    if screen.alternate_screen() || !screen.bracketed_paste() || screen.hide_cursor() {
        return None;
    }
    let (row, _) = screen.cursor_position();
    let (_, cols) = screen.size();
    let rows: Vec<_> = screen.rows(0, cols).collect();
    let mut correction = format!("\x1b[{};1H\x1b[2K", row + 1).into_bytes();
    if row >= 2
        && !rows[usize::from(row - 1)].trim().is_empty()
        && rows[usize::from(row - 2)].trim().is_empty()
    {
        correction.extend_from_slice(format!("\x1b[{};1H\x1b[2K", row).as_bytes());
    }
    Some(correction)
}

pub(super) fn process_terminal_bytes<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    prefix: &mut Vec<u8>,
    bytes: &[u8],
) {
    prefix.extend_from_slice(bytes);
    let mut output = Vec::with_capacity(prefix.len());
    let mut consumed = 0;
    while consumed < prefix.len() {
        if prefix[consumed] != 0x1b {
            output.push(prefix[consumed]);
            consumed += 1;
            continue;
        }
        let remaining = prefix.len() - consumed;
        if remaining == 1 || (remaining == 2 && prefix[consumed + 1] == b'[') {
            break;
        }
        if remaining >= 3 && prefix[consumed + 1] == b'[' {
            match prefix[consumed + 2] {
                b's' => {
                    output.extend_from_slice(b"\x1b7");
                    consumed += 3;
                    continue;
                }
                b'u' => {
                    output.extend_from_slice(b"\x1b8");
                    consumed += 3;
                    continue;
                }
                _ => {}
            }
        }
        output.push(prefix[consumed]);
        consumed += 1;
    }
    prefix.drain(..consumed);
    parser.process(&output);
}

/// What a program running in a pane is told when it asks the terminal about
/// its colours.
///
/// mux paints panes with the terminal's own default colours, so it has no
/// authoritative answer; it reports the theme's, which is what the surrounding
/// terminal is themed to as well.
#[derive(Clone, Copy)]
pub(super) struct TerminalColors {
    pub(super) foreground: Rgb,
    pub(super) background: Rgb,
    pub(super) cursor: Rgb,
}

impl From<&Theme> for TerminalColors {
    fn from(theme: &Theme) -> Self {
        Self {
            foreground: theme.popup_text,
            background: theme.bar_label_foreground,
            cursor: theme.cursor,
        }
    }
}

fn color_response(kind: &[u8], (red, green, blue): Rgb) -> Vec<u8> {
    let kind = String::from_utf8_lossy(kind);
    format!("\x1b]{kind};rgb:{red:02x}{red:02x}/{green:02x}{green:02x}/{blue:02x}{blue:02x}\x1b\\")
        .into_bytes()
}

pub(super) fn terminal_query_responses(
    prefix: &mut Vec<u8>,
    bytes: &[u8],
    cursor: (u16, u16),
    colors: TerminalColors,
) -> Vec<u8> {
    prefix.extend_from_slice(bytes);
    let mut responses = Vec::new();
    let mut consumed = 0;
    while consumed < prefix.len() {
        let Some(escape) = prefix[consumed..].iter().position(|byte| *byte == 0x1b) else {
            consumed = prefix.len();
            break;
        };
        consumed += escape;
        if prefix.len() - consumed == 1 {
            break;
        }
        match prefix[consumed + 1] {
            b']' => {
                let mut end = consumed + 2;
                let mut terminator_length = 0;
                while end < prefix.len() {
                    if prefix[end] == 0x07 {
                        terminator_length = 1;
                        break;
                    }
                    if prefix[end] == 0x1b && prefix.get(end + 1).is_some_and(|byte| *byte == b'\\')
                    {
                        terminator_length = 2;
                        break;
                    }
                    end += 1;
                }
                if terminator_length == 0 {
                    break;
                }
                match &prefix[consumed + 2..end] {
                    b"10;?" => responses.extend(color_response(b"10", colors.foreground)),
                    b"11;?" => responses.extend(color_response(b"11", colors.background)),
                    b"12;?" => responses.extend(color_response(b"12", colors.cursor)),
                    _ => {}
                }
                consumed = end + terminator_length;
            }
            b'[' => {
                let Some(final_offset) = prefix[consumed + 2..]
                    .iter()
                    .position(|byte| (0x40..=0x7e).contains(byte))
                else {
                    break;
                };
                let end = consumed + 2 + final_offset;
                match &prefix[consumed + 2..=end] {
                    b"5n" => responses.extend_from_slice(b"\x1b[0n"),
                    b"6n" => responses.extend_from_slice(
                        format!("\x1b[{};{}R", cursor.0 + 1, cursor.1 + 1).as_bytes(),
                    ),
                    b"?6n" => responses.extend_from_slice(
                        format!("\x1b[?{};{}R", cursor.0 + 1, cursor.1 + 1).as_bytes(),
                    ),
                    b"c" => responses.extend_from_slice(b"\x1b[?1;2c"),
                    b">c" => responses.extend_from_slice(b"\x1b[>0;100;0c"),
                    _ => {}
                }
                consumed = end + 1;
            }
            _ => consumed += 2,
        }
    }
    prefix.drain(..consumed);
    responses
}

pub(super) fn terminal_key_bytes(key: &Key, application_cursor: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if key.modifiers & ALT != 0 {
        bytes.push(0x1b);
    }
    match key.code {
        KeyCode::Char(character) if key.modifiers & CTRL != 0 && character.is_ascii() => {
            bytes.push((character.to_ascii_lowercase() as u8) & 0x1f);
        }
        KeyCode::Char(character) => {
            // Alt-Shift-a is looked up as a lowercase binding, so the shift
            // only survives in the modifiers; the pane still wants the capital.
            let character = if key.modifiers & SHIFT != 0 {
                character.to_uppercase().next().unwrap_or(character)
            } else {
                character
            };
            let mut encoded = [0; 4];
            bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        }
        KeyCode::Enter => bytes.push(b'\r'),
        KeyCode::Escape => bytes.push(0x1b),
        KeyCode::Backspace => bytes.push(0x7f),
        KeyCode::Tab => bytes.push(b'\t'),
        KeyCode::BackTab => bytes.extend_from_slice(b"\x1b[Z"),
        KeyCode::Up => bytes
            .extend_from_slice(cursor_sequence(b'A', key.modifiers, application_cursor).as_bytes()),
        KeyCode::Down => bytes
            .extend_from_slice(cursor_sequence(b'B', key.modifiers, application_cursor).as_bytes()),
        KeyCode::Right => bytes
            .extend_from_slice(cursor_sequence(b'C', key.modifiers, application_cursor).as_bytes()),
        KeyCode::Left => bytes
            .extend_from_slice(cursor_sequence(b'D', key.modifiers, application_cursor).as_bytes()),
        KeyCode::Home => bytes.extend_from_slice(b"\x1b[H"),
        KeyCode::End => bytes.extend_from_slice(b"\x1b[F"),
        KeyCode::Delete => bytes.extend_from_slice(b"\x1b[3~"),
        KeyCode::Insert => bytes.extend_from_slice(b"\x1b[2~"),
        KeyCode::PageUp => bytes.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => bytes.extend_from_slice(b"\x1b[6~"),
    }
    bytes
}

fn cursor_sequence(final_byte: u8, modifiers: u8, application_cursor: bool) -> String {
    let modifiers = modifiers & (SHIFT | ALT | CTRL);
    if modifiers == 0 {
        if application_cursor {
            format!("\x1bO{}", final_byte as char)
        } else {
            format!("\x1b[{}", final_byte as char)
        }
    } else {
        let parameter = 1
            + usize::from(modifiers & SHIFT != 0)
            + 2 * usize::from(modifiers & ALT != 0)
            + 4 * usize::from(modifiers & CTRL != 0);
        format!("\x1b[1;{parameter}{}", final_byte as char)
    }
}
