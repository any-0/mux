//! The glue between a pane's PTY and the `vt100` parser: what the program
//! inside is told, and what mux makes of what it says back.

use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD};

use crate::{
    config::Theme,
    frame::{CursorShape, Rgb},
    protocol::{ALT, CTRL, Key, KeyCode, Mouse, MouseButton, MouseKind, SHIFT},
};

pub(super) const SCROLLBACK_LINES: usize = 20_000;
const SYNCHRONIZED_OUTPUT_TIMEOUT: Duration = Duration::from_secs(1);

pub(super) struct TerminalCallbacks {
    pub(super) bell_count: u64,
    pub(super) prompt_checkpoint: Option<vt100::Screen>,
    pub(super) prompt_ready: Option<PromptReady>,
    pub(super) cursor_shape: Option<CursorShape>,
    pub(super) synchronized_output: Option<SynchronizedOutput>,
    pub(super) responses: Vec<u8>,
    colors: TerminalColors,
    /// The title the program in this pane last set, which is what a window
    /// with no name of its own is called.
    pub(super) title: Option<String>,
    pub(super) clipboard_writes: Vec<ClipboardWrite>,
}

impl Default for TerminalCallbacks {
    fn default() -> Self {
        Self {
            bell_count: 0,
            prompt_checkpoint: None,
            prompt_ready: None,
            cursor_shape: None,
            synchronized_output: None,
            responses: Vec::new(),
            colors: TerminalColors::from(&Theme::default()),
            title: None,
            clipboard_writes: Vec::new(),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct ClipboardWrite {
    pub(super) selection: Vec<u8>,
    pub(super) data: Vec<u8>,
}

pub(super) struct SynchronizedOutput {
    screen: vt100::Screen,
    cursor_shape: Option<CursorShape>,
    pub(super) expires: Instant,
}

impl TerminalCallbacks {
    pub(super) fn set_colors(&mut self, colors: TerminalColors) {
        self.colors = colors;
    }

    pub(super) fn expire_synchronized_output(&mut self, now: Instant) -> bool {
        if self
            .synchronized_output
            .as_ref()
            .is_some_and(|update| now >= update.expires)
        {
            self.synchronized_output = None;
            return true;
        }
        false
    }
}

/// Applications may move a visible cursor while painting a synchronized
/// update. Keep both the cells and cursor at the completed screen until it ends.
pub(super) fn rendered_terminal(
    parser: &vt100::Parser<TerminalCallbacks>,
    default_cursor_shape: CursorShape,
) -> (&vt100::Screen, CursorShape) {
    let callbacks = parser.callbacks();
    match &callbacks.synchronized_output {
        Some(update) => (
            &update.screen,
            update.cursor_shape.unwrap_or(default_cursor_shape),
        ),
        None => (
            parser.screen(),
            callbacks.cursor_shape.unwrap_or(default_cursor_shape),
        ),
    }
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
        if 2 + selection.len() + data.len() >= vt100::MAX_OSC_BYTES {
            return;
        }
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
        screen: &mut vt100::Screen,
        first_intermediate: Option<u8>,
        second_intermediate: Option<u8>,
        params: &[&[u16]],
        final_character: char,
    ) {
        if second_intermediate.is_none() {
            if first_intermediate.is_none() && params == [&[5][..]] && final_character == 'n' {
                self.responses.extend_from_slice(b"\x1b[0n");
            } else if matches!(first_intermediate, None | Some(b'?'))
                && params == [&[6][..]]
                && final_character == 'n'
            {
                let (row, col) = screen.cursor_position();
                let private = if first_intermediate == Some(b'?') {
                    "?"
                } else {
                    ""
                };
                self.responses.extend_from_slice(
                    format!("\x1b[{private}{};{}R", row + 1, col + 1).as_bytes(),
                );
            } else if params == [&[0][..]] && final_character == 'c' {
                match first_intermediate {
                    None => self.responses.extend_from_slice(b"\x1b[?1;2c"),
                    Some(b'>') => self.responses.extend_from_slice(b"\x1b[>0;100;0c"),
                    _ => {}
                }
            }
        }
        if first_intermediate == Some(b'?') {
            if second_intermediate.is_none()
                && params.contains(&&[2026][..])
                && matches!(final_character, 'h' | 'l')
            {
                if final_character == 'h' {
                    let expires = Instant::now() + SYNCHRONIZED_OUTPUT_TIMEOUT;
                    if let Some(update) = &mut self.synchronized_output {
                        update.expires = expires;
                    } else {
                        self.synchronized_output = Some(SynchronizedOutput {
                            screen: screen.clone(),
                            cursor_shape: self.cursor_shape,
                            expires,
                        });
                    }
                } else {
                    self.synchronized_output = None;
                }
            } else if second_intermediate == Some(b'$')
                && params == [&[2026][..]]
                && final_character == 'p'
            {
                self.responses
                    .extend_from_slice(if self.synchronized_output.is_some() {
                        b"\x1b[?2026;1$y"
                    } else {
                        b"\x1b[?2026;2$y"
                    });
            }
            return;
        }
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
            0 => None,
            1 | 2 => Some(CursorShape::Block),
            3 | 4 => Some(CursorShape::Underline),
            5 | 6 => Some(CursorShape::Bar),
            _ => return,
        };
    }

    fn unhandled_osc(&mut self, screen: &mut vt100::Screen, params: &[&[u8]]) {
        match params {
            [b"10", b"?"] => self
                .responses
                .extend(color_response(b"10", self.colors.foreground)),
            [b"11", b"?"] => self
                .responses
                .extend(color_response(b"11", self.colors.background)),
            [b"12", b"?"] => self
                .responses
                .extend(color_response(b"12", self.colors.cursor)),
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
                    Some(b'0') => Some(CursorShape::Block),
                    Some(b'1') => Some(CursorShape::Bar),
                    Some(b'2') => Some(CursorShape::Underline),
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

pub(super) fn terminal_key_bytes(key: &Key, application_cursor: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if key.modifiers & ALT != 0
        && matches!(
            key.code,
            KeyCode::Char(_)
                | KeyCode::Enter
                | KeyCode::Escape
                | KeyCode::Backspace
                | KeyCode::Tab
                | KeyCode::BackTab
        )
    {
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
        KeyCode::Home => bytes.extend_from_slice(modified_csi(b'H', key.modifiers).as_bytes()),
        KeyCode::End => bytes.extend_from_slice(modified_csi(b'F', key.modifiers).as_bytes()),
        KeyCode::Delete => bytes.extend_from_slice(tilde_sequence(3, key.modifiers).as_bytes()),
        KeyCode::Insert => bytes.extend_from_slice(tilde_sequence(2, key.modifiers).as_bytes()),
        KeyCode::PageUp => bytes.extend_from_slice(tilde_sequence(5, key.modifiers).as_bytes()),
        KeyCode::PageDown => bytes.extend_from_slice(tilde_sequence(6, key.modifiers).as_bytes()),
        KeyCode::F(number) => {
            let sequence = match number {
                1..=4 => modified_csi(b'P' + number - 1, key.modifiers),
                5 => tilde_sequence(15, key.modifiers),
                6..=10 => tilde_sequence(11 + u16::from(number), key.modifiers),
                11..=12 => tilde_sequence(12 + u16::from(number), key.modifiers),
                _ => String::new(),
            };
            bytes.extend_from_slice(sequence.as_bytes());
        }
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

fn modified_csi(final_byte: u8, modifiers: u8) -> String {
    match modifier_parameter(modifiers) {
        None => {
            if matches!(final_byte, b'P'..=b'S') {
                format!("\x1bO{}", final_byte as char)
            } else {
                format!("\x1b[{}", final_byte as char)
            }
        }
        Some(parameter) => format!("\x1b[1;{parameter}{}", final_byte as char),
    }
}

fn tilde_sequence(code: u16, modifiers: u8) -> String {
    match modifier_parameter(modifiers) {
        None => format!("\x1b[{code}~"),
        Some(parameter) => format!("\x1b[{code};{parameter}~"),
    }
}

fn modifier_parameter(modifiers: u8) -> Option<usize> {
    let modifiers = modifiers & (SHIFT | ALT | CTRL);
    (modifiers != 0).then(|| {
        1 + usize::from(modifiers & SHIFT != 0)
            + 2 * usize::from(modifiers & ALT != 0)
            + 4 * usize::from(modifiers & CTRL != 0)
    })
}

#[cfg(test)]
mod key_sequence_tests {
    use super::terminal_key_bytes;
    use crate::protocol::{ALT, CTRL, Key, KeyCode, SHIFT};

    fn key(code: KeyCode, modifiers: u8) -> Key {
        Key { code, modifiers }
    }

    #[test]
    fn xterm_function_keys_include_modifiers() {
        assert_eq!(terminal_key_bytes(&key(KeyCode::F(1), 0), false), b"\x1bOP");
        assert_eq!(
            terminal_key_bytes(&key(KeyCode::F(4), SHIFT | ALT), false),
            b"\x1b[1;4S"
        );
        assert_eq!(
            terminal_key_bytes(&key(KeyCode::F(5), 0), false),
            b"\x1b[15~"
        );
        assert_eq!(
            terminal_key_bytes(&key(KeyCode::F(12), CTRL), false),
            b"\x1b[24;5~"
        );
    }

    #[test]
    fn navigation_keys_include_modifiers() {
        assert_eq!(
            terminal_key_bytes(&key(KeyCode::Home, CTRL), false),
            b"\x1b[1;5H"
        );
        assert_eq!(
            terminal_key_bytes(&key(KeyCode::Delete, SHIFT | ALT), false),
            b"\x1b[3;4~"
        );
        assert_eq!(
            terminal_key_bytes(&key(KeyCode::PageDown, ALT), false),
            b"\x1b[6;3~"
        );
    }
}
