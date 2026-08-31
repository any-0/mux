use std::{
    env,
    ffi::OsStr,
    io::{Write, stdout},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use crossterm::{
    cursor::{Hide, SetCursorStyle, Show},
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode as CrosstermKeyCode, KeyEventKind, KeyModifiers,
        MouseButton as CrosstermMouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, size,
    },
};

use crate::{
    config::Settings,
    protocol::{
        ALT, CTRL, ClientMessage, Hello, Key, KeyCode, Mouse, MouseButton, MouseKind, MuxCommand,
        MuxQuery, SHIFT, ServerMessage, read_message, write_message,
    },
};

enum ClientEvent {
    Server(ServerMessage),
    ServerDisconnected,
    Terminal(Event),
    TerminalError(String),
}

/// Where the daemon listens.
///
/// The fallback lives in a per-user directory rather than directly in `/tmp`,
/// so the daemon can keep the socket and its startup files out of reach of
/// other local accounts; see `server::persist::prepare_socket_directory`.
pub fn socket_path() -> PathBuf {
    if let Some(socket) = env::var_os("MUX") {
        PathBuf::from(socket)
    } else if let Some(runtime) = env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime).join("mux.sock")
    } else {
        PathBuf::from(format!("/tmp/mux-{}", nix::unistd::getuid().as_raw())).join("mux.sock")
    }
}

/// Whether attaching would put a client inside the daemon it belongs to.
///
/// `inside` is `MUX`, which the daemon sets in every pane it spawns.
fn nested(inside: Option<&OsStr>, socket: &Path) -> bool {
    inside.is_some_and(|inside| Path::new(inside) == socket)
}

pub fn attach(config: Option<&Path>, session: Option<String>) -> Result<()> {
    let settings = Settings::load(config)?;
    let socket_path = socket_path();
    // A pane is already showing one of this daemon's sessions. Attaching from
    // inside it gives both clients the same session, and since the inner
    // client's terminal is the pane it just resized, each resize feeds the next
    // until the pane is one column wide.
    if nested(env::var_os("MUX").as_deref(), &socket_path) {
        bail!("already inside this mux; run `env -u MUX mux` to attach a second client anyway");
    }
    let mut stream = connect_or_start(&socket_path)?;
    let (cols, rows) = terminal_size()?;
    let cwd = env::current_dir().context("read current directory")?;
    write_message(
        &mut stream,
        &ClientMessage::Hello(Box::new(Hello {
            cols,
            rows,
            cwd,
            session,
            bindings: settings.bindings,
            clipboard_command: settings.clipboard_command,
            theme: settings.theme,
            theme_command: settings.theme_command,
            theme_directory: settings.theme_directory,
            mouse: settings.mouse,
            bell_style: settings.bell_style,
            truecolor: terminal_has_truecolor(),
        })),
    )?;

    let mut reader = stream.try_clone()?;
    let (sender, receiver) = mpsc::channel();
    let server_sender = sender.clone();
    thread::spawn(move || {
        loop {
            match read_message(&mut reader) {
                Ok(Some(message)) => {
                    if server_sender.send(ClientEvent::Server(message)).is_err() {
                        return;
                    }
                }
                _ => {
                    let _ = server_sender.send(ClientEvent::ServerDisconnected);
                    return;
                }
            }
        }
    });

    let _terminal = TerminalGuard::enter(settings.mouse)?;
    let input_sender = sender.clone();
    thread::spawn(move || {
        loop {
            match event::read() {
                Ok(event) => {
                    if input_sender.send(ClientEvent::Terminal(event)).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = input_sender.send(ClientEvent::TerminalError(error.to_string()));
                    return;
                }
            }
        }
    });
    let mut output = stdout();
    loop {
        match receiver.recv() {
            Ok(ClientEvent::Server(ServerMessage::Render(bytes))) => {
                output.write_all(&bytes)?;
                output.flush()?;
            }
            Ok(ClientEvent::Server(ServerMessage::Detached)) => return Ok(()),
            // Only a query asks for a listing, and an attached client never does.
            Ok(ClientEvent::Server(ServerMessage::Done | ServerMessage::Listing(_))) => {}
            Ok(ClientEvent::Server(ServerMessage::Error(error))) => bail!("server: {error}"),
            Ok(ClientEvent::ServerDisconnected) | Err(_) => {
                bail!("multiplexer server disconnected")
            }
            Ok(ClientEvent::Terminal(Event::Key(key)))
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
            {
                if let Some(key) = convert_key(key.code, key.modifiers) {
                    write_message(&mut stream, &ClientMessage::Key(key))?;
                }
            }
            Ok(ClientEvent::Terminal(Event::Mouse(mouse))) => {
                if let Some(mouse) = convert_mouse(mouse) {
                    write_message(&mut stream, &ClientMessage::Mouse(mouse))?;
                }
            }
            Ok(ClientEvent::Terminal(Event::Paste(text))) => {
                write_message(&mut stream, &ClientMessage::Paste(text))?
            }
            Ok(ClientEvent::Terminal(Event::Resize(cols, rows))) => {
                let (cols, rows) = usable_terminal_size(cols, rows);
                write_message(&mut stream, &ClientMessage::Resize { cols, rows })?
            }
            Ok(ClientEvent::Terminal(_)) => {}
            Ok(ClientEvent::TerminalError(error)) => bail!("terminal input: {error}"),
        }
    }
}

fn terminal_size() -> Result<(u16, u16)> {
    let (cols, rows) = size().context("read terminal size")?;
    Ok(usable_terminal_size(cols, rows))
}

fn usable_terminal_size(cols: u16, rows: u16) -> (u16, u16) {
    (cols.max(2), rows.max(2))
}

/// Whether this terminal renders 24-bit colour, which is what the theme is
/// written in. The daemon paints for whatever the client reports, so an
/// unannounced terminal gets the nearest 256-colour approximation instead of
/// escape sequences it will not understand.
fn terminal_has_truecolor() -> bool {
    truecolor_from(
        env::var_os("COLORTERM").as_deref(),
        env::var_os("TERM").as_deref(),
    )
}

fn truecolor_from(colorterm: Option<&OsStr>, term: Option<&OsStr>) -> bool {
    if colorterm.is_some_and(|value| value == "truecolor" || value == "24bit") {
        return true;
    }
    // Some terminals say so in TERM instead of setting COLORTERM at all.
    term.and_then(OsStr::to_str).is_some_and(|term| {
        term == "xterm-kitty" || term.contains("direct") || term.contains("truecolor")
    })
}

/// Connects to a daemon that is already running. Unlike attaching, a one-shot
/// message is never worth starting one for: there would be no sessions in it.
fn connect() -> Result<UnixStream> {
    let path = socket_path();
    UnixStream::connect(&path).with_context(|| format!("connect to {}", path.display()))
}

/// Sends one message and waits for the daemon's single reply, which is `None`
/// when the daemon hung up before sending one.
fn request(message: ClientMessage) -> Result<Option<ServerMessage>> {
    let mut stream = connect()?;
    write_message(&mut stream, &message)?;
    read_message(&mut stream)
}

/// The pane a script is running in, which tells the daemon which session it
/// means without the script having to be attached to one.
fn origin_pane() -> Option<usize> {
    env::var("MUX_PANE").ok()?.parse().ok()
}

pub fn stop() -> Result<()> {
    match request(ClientMessage::Shutdown)? {
        Some(ServerMessage::Detached) => Ok(()),
        Some(ServerMessage::Error(error)) => bail!("server: {error}"),
        _ => bail!("multiplexer server disconnected before confirming shutdown"),
    }
}

pub fn command(command: MuxCommand) -> Result<()> {
    let message = ClientMessage::Command {
        pane_id: origin_pane(),
        command,
    };
    match request(message)? {
        Some(ServerMessage::Done) => Ok(()),
        Some(ServerMessage::Error(error)) => bail!("server: {error}"),
        _ => bail!("multiplexer server disconnected before completing command"),
    }
}

/// Asks the daemon a question and prints the answer, one item per line.
pub fn query(query: MuxQuery) -> Result<()> {
    let message = ClientMessage::Query {
        pane_id: origin_pane(),
        query,
    };
    match request(message)? {
        Some(ServerMessage::Listing(lines)) => {
            let mut output = stdout().lock();
            for line in lines {
                writeln!(output, "{line}")?;
            }
            Ok(())
        }
        Some(ServerMessage::Error(error)) => bail!("server: {error}"),
        _ => bail!("multiplexer server disconnected before answering"),
    }
}

fn connect_or_start(path: &Path) -> Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(path) {
        return Ok(stream);
    }
    let executable = env::current_exe().context("locate mux executable")?;
    Command::new(executable)
        .arg("__server")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("start multiplexer daemon")?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("daemon did not open {}", path.display()));
            }
        }
    }
}

fn convert_key(code: CrosstermKeyCode, modifiers: KeyModifiers) -> Option<Key> {
    let mut modifier_bits = 0;
    if modifiers.contains(KeyModifiers::SHIFT) {
        modifier_bits |= SHIFT;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        modifier_bits |= ALT;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        modifier_bits |= CTRL;
    }
    let code = match code {
        // Ctrl-[ is Escape's own byte, and the only way to type it on a
        // keyboard whose Escape key is broken; mux reads the two as one key.
        CrosstermKeyCode::Char('[') if modifier_bits & CTRL != 0 => {
            modifier_bits &= !(CTRL | SHIFT);
            KeyCode::Escape
        }
        CrosstermKeyCode::Char(mut character) => {
            if modifier_bits & (ALT | CTRL) != 0
                && modifier_bits & SHIFT != 0
                && character.is_uppercase()
            {
                character = character.to_lowercase().next().unwrap_or(character);
            } else if modifier_bits & (ALT | CTRL) == 0 {
                modifier_bits &= !SHIFT;
            }
            KeyCode::Char(character)
        }
        CrosstermKeyCode::Enter => KeyCode::Enter,
        CrosstermKeyCode::Esc => KeyCode::Escape,
        CrosstermKeyCode::Backspace => KeyCode::Backspace,
        CrosstermKeyCode::Tab => KeyCode::Tab,
        CrosstermKeyCode::BackTab => KeyCode::BackTab,
        CrosstermKeyCode::Up => KeyCode::Up,
        CrosstermKeyCode::Down => KeyCode::Down,
        CrosstermKeyCode::Left => KeyCode::Left,
        CrosstermKeyCode::Right => KeyCode::Right,
        CrosstermKeyCode::Home => KeyCode::Home,
        CrosstermKeyCode::End => KeyCode::End,
        CrosstermKeyCode::Delete => KeyCode::Delete,
        CrosstermKeyCode::Insert => KeyCode::Insert,
        CrosstermKeyCode::PageUp => KeyCode::PageUp,
        CrosstermKeyCode::PageDown => KeyCode::PageDown,
        _ => return None,
    };
    Some(Key {
        code,
        modifiers: modifier_bits,
    })
}

fn convert_mouse(event: MouseEvent) -> Option<Mouse> {
    let button = |button| match button {
        CrosstermMouseButton::Left => MouseButton::Left,
        CrosstermMouseButton::Middle => MouseButton::Middle,
        CrosstermMouseButton::Right => MouseButton::Right,
    };
    let (kind, pressed) = match event.kind {
        MouseEventKind::Down(pressed) => (MouseKind::Down, button(pressed)),
        MouseEventKind::Up(pressed) => (MouseKind::Up, button(pressed)),
        MouseEventKind::Drag(pressed) => (MouseKind::Drag, button(pressed)),
        MouseEventKind::ScrollUp => (MouseKind::ScrollUp, MouseButton::Left),
        MouseEventKind::ScrollDown => (MouseKind::ScrollDown, MouseButton::Left),
        // Plain movement is noise unless something asked for it.
        _ => return None,
    };
    let mut modifiers = 0;
    if event.modifiers.contains(KeyModifiers::SHIFT) {
        modifiers |= SHIFT;
    }
    if event.modifiers.contains(KeyModifiers::ALT) {
        modifiers |= ALT;
    }
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        modifiers |= CTRL;
    }
    Some(Mouse {
        kind,
        button: pressed,
        col: event.column,
        row: event.row,
        modifiers,
    })
}

struct TerminalGuard {
    mouse: bool,
}

impl TerminalGuard {
    fn enter(mouse: bool) -> Result<Self> {
        enable_raw_mode()?;
        execute!(
            stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            SetCursorStyle::SteadyBlock,
            Hide
        )?;
        if mouse {
            execute!(stdout(), EnableMouseCapture)?;
        }
        Ok(Self { mouse })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.mouse {
            let _ = execute!(stdout(), DisableMouseCapture);
        }
        let _ = execute!(
            stdout(),
            SetCursorStyle::DefaultUserShape,
            Show,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifted_alt_letters_are_canonicalized_for_bindings() {
        assert_eq!(
            convert_key(
                CrosstermKeyCode::Char('T'),
                KeyModifiers::ALT | KeyModifiers::SHIFT
            ),
            Some(crate::config::parse_key("Alt-Shift-t").unwrap())
        );
        assert_eq!(
            convert_key(CrosstermKeyCode::Char('W'), KeyModifiers::SHIFT),
            Some(crate::config::parse_key("W").unwrap())
        );
    }

    #[test]
    fn truecolor_is_taken_from_colorterm_or_a_direct_term() {
        let colorterm = |value| Some(OsStr::new(value));
        assert!(truecolor_from(colorterm("truecolor"), None));
        assert!(truecolor_from(colorterm("24bit"), None));
        assert!(truecolor_from(None, colorterm("xterm-direct")));
        assert!(truecolor_from(None, colorterm("xterm-kitty")));
        // Anything that has not said so is painted for 256 colours.
        assert!(!truecolor_from(None, None));
        assert!(!truecolor_from(None, colorterm("xterm-256color")));
        assert!(!truecolor_from(colorterm("8bit"), colorterm("screen")));
    }

    #[test]
    fn attaching_from_inside_the_same_daemon_is_refused() {
        let socket = Path::new("/run/user/501/mux.sock");
        assert!(nested(Some(OsStr::new("/run/user/501/mux.sock")), socket));
        // Outside a pane, and inside a pane of a different daemon.
        assert!(!nested(None, socket));
        assert!(!nested(Some(OsStr::new("/tmp/other/mux.sock")), socket));
        // Clearing MUX is the way to nest deliberately.
        assert!(!nested(Some(OsStr::new("")), socket));
    }

    #[test]
    fn zero_sized_pty_reports_a_usable_terminal_size() {
        assert_eq!(usable_terminal_size(0, 0), (2, 2));
    }
}
