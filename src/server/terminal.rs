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

#[derive(Default)]
pub(super) struct TerminalCallbacks {
    pub(super) bell_count: u64,
    pub(super) prompt_checkpoint: Option<vt100::Screen>,
    pub(super) prompt_ready: Option<PromptReady>,
    pub(super) cursor_shape: Option<CursorShape>,
    /// The cursor colour the program set (OSC 12), shown instead of the
    /// theme's while this pane's cursor is the one on screen.
    pub(super) cursor_color: Option<Rgb>,
    pub(super) synchronized_output: Option<SynchronizedOutput>,
    pub(super) responses: Vec<u8>,
    colors: TerminalColors,
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
        self.synchronized_output
            .take_if(|update| now >= update.expires)
            .is_some()
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
        let only = |value: u16| params == [&[value][..]];
        let synchronized = params.contains(&&[2026][..]);
        match (first_intermediate, second_intermediate, final_character) {
            (None, None, 'n') if only(5) => self.responses.extend_from_slice(b"\x1b[0n"),
            (None | Some(b'?'), None, 'n') if only(6) => {
                let (row, col) = screen.cursor_position();
                let private = if first_intermediate.is_some() {
                    "?"
                } else {
                    ""
                };
                self.responses.extend_from_slice(
                    format!("\x1b[{private}{};{}R", row + 1, col + 1).as_bytes(),
                );
            }
            (None, None, 'c') if only(0) => self.responses.extend_from_slice(b"\x1b[?1;2c"),
            (Some(b'>'), None, 'c') if only(0) => {
                self.responses.extend_from_slice(b"\x1b[>0;100;0c");
            }
            (Some(b'?'), None, 'h') if synchronized => {
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
            }
            (Some(b'?'), None, 'l') if synchronized => self.synchronized_output = None,
            (Some(b'?'), Some(b'$'), 'p') if only(2026) => {
                self.responses
                    .extend_from_slice(if self.synchronized_output.is_some() {
                        b"\x1b[?2026;1$y"
                    } else {
                        b"\x1b[?2026;2$y"
                    });
            }
            (Some(b' '), None, 'q') => {
                let style = params.first().and_then(|param| param.first());
                self.cursor_shape = match style.copied().unwrap_or(0) {
                    0 => None,
                    1 | 2 => Some(CursorShape::Block),
                    3 | 4 => Some(CursorShape::Underline),
                    5 | 6 => Some(CursorShape::Bar),
                    _ => return,
                };
            }
            _ => {}
        }
    }

    /// DECRQSS (`DCS $ q Pt ST`). Only SGR is reported: Neovim sets an
    /// undercurl and reads it back this way, and only then sends undercurls and
    /// underline colours at all.
    fn unhandled_dcs(
        &mut self,
        screen: &mut vt100::Screen,
        intermediates: &[u8],
        final_character: char,
        data: &[u8],
    ) {
        if intermediates != b"$" || final_character != 'q' {
            return;
        }
        if data != b"m" {
            self.responses.extend_from_slice(b"\x1bP0$r\x1b\\");
            return;
        }
        // `attributes_formatted` is a reset followed by SGR sequences; the
        // reply is their parameters as one list.
        let formatted = screen.attributes_formatted();
        let mut reply = b"\x1bP1$r0".to_vec();
        for parameters in formatted
            .split(|&byte| byte == 0x1b)
            .filter_map(|sequence| sequence.strip_prefix(b"["))
            .filter_map(|sequence| sequence.strip_suffix(b"m"))
            .filter(|parameters| !parameters.is_empty())
        {
            reply.push(b';');
            reply.extend_from_slice(parameters);
        }
        reply.extend_from_slice(b"m\x1b\\");
        self.responses.extend_from_slice(&reply);
    }

    fn unhandled_osc(&mut self, screen: &mut vt100::Screen, params: &[&[u8]]) {
        match params {
            [b"10", b"?"] => self
                .responses
                .extend(color_response(b"10", self.colors.foreground)),
            [b"11", b"?"] => self
                .responses
                .extend(color_response(b"11", self.colors.background)),
            [b"12", b"?"] => self.responses.extend(color_response(
                b"12",
                self.cursor_color.unwrap_or(self.colors.cursor),
            )),
            [b"12", spec] => {
                if let Some(color) = parse_color_spec(spec) {
                    self.cursor_color = Some(color);
                }
            }
            [b"112"] | [b"112", b""] => self.cursor_color = None,
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
        + 32 * u8::from(matches!(mouse.kind, MouseKind::Drag))
        + 4 * u8::from(mouse.modifiers & SHIFT != 0)
        + 8 * u8::from(mouse.modifiers & ALT != 0)
        + 16 * u8::from(mouse.modifiers & CTRL != 0);
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
            Some(vec![
                0x1b,
                b'[',
                b'M',
                32u8.saturating_add(button),
                32 + col as u8,
                32 + row as u8,
            ])
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

/// Feeds a pane's output to its terminal emulator. Sequences split across
/// reads are completed by the parser itself.
pub(super) fn process_terminal_bytes<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    bytes: &[u8],
) {
    parser.process(bytes);
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

impl Default for TerminalColors {
    fn default() -> Self {
        Self::from(&Theme::default())
    }
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

/// An X11 colour specification as programs send it: `#rgb`, `#rrggbb`,
/// `#rrrgggbbb`, `#rrrrggggbbbb`, or `rgb:r/g/b` with one to four hex digits
/// per channel. Colour names are not understood.
fn parse_color_spec(spec: &[u8]) -> Option<Rgb> {
    let spec = std::str::from_utf8(spec).ok()?;
    if !spec.is_ascii() {
        return None;
    }
    let channels: Vec<&str> = if let Some(hex) = spec.strip_prefix('#') {
        let width = match hex.len() {
            3 | 6 | 9 | 12 => hex.len() / 3,
            _ => return None,
        };
        (0..3).map(|index| &hex[index * width..][..width]).collect()
    } else {
        spec.strip_prefix("rgb:")?.split('/').collect()
    };
    let [red, green, blue] = channels[..] else {
        return None;
    };
    // Each channel is a fraction of its own width: `f` and `ffff` are both
    // full intensity.
    let channel = |digits: &str| {
        if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(digits, 16).ok()?;
        let max = (1 << (4 * digits.len())) - 1;
        u8::try_from((value * 255 + max / 2) / max).ok()
    };
    Some((channel(red)?, channel(green)?, channel(blue)?))
}

fn color_response(kind: &[u8], (red, green, blue): Rgb) -> Vec<u8> {
    let kind = String::from_utf8_lossy(kind);
    format!("\x1b]{kind};rgb:{red:02x}{red:02x}/{green:02x}{green:02x}/{blue:02x}{blue:02x}\x1b\\")
        .into_bytes()
}

pub(super) fn terminal_key_bytes(key: &Key, application_cursor: bool) -> Vec<u8> {
    let cursor =
        |final_byte| cursor_sequence(final_byte, key.modifiers, application_cursor).into_bytes();
    let tilde = |code| tilde_sequence(code, key.modifiers).into_bytes();
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
        KeyCode::Char(character)
            if key.modifiers & CTRL != 0 && control_byte(character).is_some() =>
        {
            bytes.extend(control_byte(character));
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
        // Home and End follow the cursor keys: SS3 in application cursor mode,
        // which is what terminfo's khome and kend expect once curses sends smkx.
        KeyCode::Up => bytes.extend(cursor(b'A')),
        KeyCode::Down => bytes.extend(cursor(b'B')),
        KeyCode::Right => bytes.extend(cursor(b'C')),
        KeyCode::Left => bytes.extend(cursor(b'D')),
        KeyCode::Home => bytes.extend(cursor(b'H')),
        KeyCode::End => bytes.extend(cursor(b'F')),
        KeyCode::Delete => bytes.extend(tilde(3)),
        KeyCode::Insert => bytes.extend(tilde(2)),
        KeyCode::PageUp => bytes.extend(tilde(5)),
        KeyCode::PageDown => bytes.extend(tilde(6)),
        // F1-F4 are SS3 P-S unmodified, whatever the cursor mode.
        KeyCode::F(number @ 1..=4) => {
            bytes.extend(cursor_sequence(b'P' + number - 1, key.modifiers, true).into_bytes());
        }
        KeyCode::F(5) => bytes.extend(tilde(15)),
        KeyCode::F(number @ 6..=10) => bytes.extend(tilde(11 + u16::from(number))),
        KeyCode::F(number @ 11..=12) => bytes.extend(tilde(12 + u16::from(number))),
        KeyCode::F(_) => {}
    }
    bytes
}

/// The byte xterm sends for Ctrl and `character`, or `None` when Ctrl does
/// nothing to it and the character is sent as is.
fn control_byte(character: char) -> Option<u8> {
    Some(match character {
        'a'..='z' | 'A'..='Z' => (character as u8) & 0x1f,
        '@' | ' ' | '2' | '`' => 0x00,
        '[' | '3' | '{' => 0x1b,
        '\\' | '4' | '|' => 0x1c,
        ']' | '5' | '}' => 0x1d,
        '^' | '6' | '~' => 0x1e,
        '_' | '7' | '/' | '-' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    })
}

fn cursor_sequence(final_byte: u8, modifiers: u8, application_cursor: bool) -> String {
    let final_byte = final_byte as char;
    match modifier_parameter(modifiers) {
        Some(parameter) => format!("\x1b[1;{parameter}{final_byte}"),
        None if application_cursor => format!("\x1bO{final_byte}"),
        None => format!("\x1b[{final_byte}"),
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

    #[test]
    fn control_keys_use_the_bytes_xterm_sends() {
        let ctrl = |character| terminal_key_bytes(&key(KeyCode::Char(character), CTRL), false);
        assert_eq!(ctrl('a'), b"\x01");
        assert_eq!(ctrl(' '), b"\x00");
        assert_eq!(ctrl('\\'), b"\x1c");
        assert_eq!(ctrl(']'), b"\x1d");
        assert_eq!(ctrl('^'), b"\x1e");
        assert_eq!(ctrl('_'), b"\x1f");
        assert_eq!(ctrl('/'), b"\x1f");
        assert_eq!(ctrl('2'), b"\x00");
        assert_eq!(ctrl('8'), b"\x7f");
        // Ctrl does nothing to characters it has no code for.
        assert_eq!(ctrl('1'), b"1");
        assert_eq!(
            terminal_key_bytes(&key(KeyCode::Char(']'), CTRL | ALT), false),
            b"\x1b\x1d"
        );
    }

    #[test]
    fn home_and_end_follow_application_cursor_mode() {
        assert_eq!(terminal_key_bytes(&key(KeyCode::Home, 0), false), b"\x1b[H");
        assert_eq!(terminal_key_bytes(&key(KeyCode::Home, 0), true), b"\x1bOH");
        assert_eq!(terminal_key_bytes(&key(KeyCode::End, 0), true), b"\x1bOF");
        assert_eq!(
            terminal_key_bytes(&key(KeyCode::End, SHIFT), true),
            b"\x1b[1;2F"
        );
    }
}
