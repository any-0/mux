use std::{
    env,
    ffi::OsStr,
    io::{Read, Write, stdout},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use crossterm::{
    cursor::{Hide, SetCursorStyle, Show},
    event::{
        self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, Event, KeyCode as CrosstermKeyCode, KeyEventKind,
        KeyModifiers, MouseButton as CrosstermMouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, size,
    },
};
use nix::sys::signal::{SigSet, Signal};

use crate::{
    config::Settings,
    frame::TerminalFeatures,
    protocol::{
        ALT, CTRL, ClientMessage, Hello, Key, KeyCode, Mouse, MouseButton, MouseKind, MuxCommand,
        MuxQuery, SHIFT, ServerMessage, read_message, write_message,
    },
};

/// Events the client holds while its terminal is busy writing a frame.
const CLIENT_EVENT_QUEUE: usize = 16;

enum ClientEvent {
    /// A signal that ends the client: the terminal is put back first.
    Terminate,
    /// Job control asked the client to stop (SIGTSTP), or it was continued.
    Suspend,
    Resume,
    Server(ServerMessage),
    ServerDisconnected,
    Terminal(Event),
    TerminalError(String),
    ServerError(String),
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
    // Signals are taken on a thread of their own, so that whatever ends or
    // stops the client puts the terminal back the way it found it first. The
    // mask must be in place before any thread starts, since they inherit it,
    // and only after the daemon has been started: a blocked mask survives
    // exec, and every shell the daemon ever spawns would inherit it.
    let signals = client_signals();
    signals.thread_block().context("block client signals")?;
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
            terminal_clipboard: env::var_os("SSH_TTY").is_some_and(|value| !value.is_empty()),
            theme: settings.theme,
            theme_command: settings.theme_command,
            theme_directory: settings.theme_directory,
            mouse: settings.mouse,
            bell_style: settings.bell_style,
            terminal: TerminalFeatures {
                truecolor: settings.truecolor.unwrap_or_else(terminal_has_truecolor),
                styled_underlines: settings
                    .styled_underlines
                    .unwrap_or_else(|| styled_underlines_from(|name| env::var(name).ok())),
            },
            glyphs: settings.glyphs,
            default_cursor_shape: settings.default_cursor_shape,
        })),
    )?;

    let mut reader = stream.try_clone()?;
    // Bounded, so a terminal that stops reading stops this client reading
    // the socket too. The daemon then skips the frames in between and paints
    // the present in full once the terminal drains, instead of the client
    // buffering and replaying everything it missed.
    let (sender, receiver) = mpsc::sync_channel(CLIENT_EVENT_QUEUE);
    let server_sender = sender.clone();
    thread::spawn(move || {
        loop {
            match read_message(&mut reader) {
                Ok(Some(message)) => {
                    if server_sender.send(ClientEvent::Server(message)).is_err() {
                        return;
                    }
                }
                Ok(None) => {
                    let _ = server_sender.send(ClientEvent::ServerDisconnected);
                    return;
                }
                Err(error) => {
                    let _ = server_sender.send(ClientEvent::ServerError(error.to_string()));
                    return;
                }
            }
        }
    });

    let mut terminal = TerminalGuard::enter(settings.mouse)?;
    let signal_sender = sender.clone();
    thread::spawn(move || {
        while let Ok(signal) = signals.wait() {
            let event = match signal {
                Signal::SIGTSTP => ClientEvent::Suspend,
                Signal::SIGCONT => ClientEvent::Resume,
                _ => ClientEvent::Terminate,
            };
            if signal_sender.send(event).is_err() {
                return;
            }
        }
    });
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
            Ok(ClientEvent::Server(ServerMessage::Clipboard { selection, data })) => {
                write_terminal_clipboard(&mut output, &selection, &data)?;
            }
            Ok(ClientEvent::Server(ServerMessage::Detached)) => return Ok(()),
            // Only a query asks for a listing, and an attached client never does.
            Ok(ClientEvent::Server(ServerMessage::Done | ServerMessage::Listing(_))) => {}
            Ok(ClientEvent::Server(ServerMessage::Error(error))) => bail!("server: {error}"),
            Ok(ClientEvent::ServerDisconnected) | Err(_) => {
                bail!(
                    "multiplexer server disconnected; if mux was just updated, restart the daemon so client and daemon use the same version"
                )
            }
            Ok(ClientEvent::ServerError(error)) => bail!("multiplexer server protocol: {error}"),
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
            Ok(ClientEvent::Terminal(Event::FocusGained)) => {
                write_message(&mut stream, &ClientMessage::Focus(true))?
            }
            Ok(ClientEvent::Terminal(Event::FocusLost)) => {
                write_message(&mut stream, &ClientMessage::Focus(false))?
            }
            Ok(ClientEvent::Terminate) => return Ok(()),
            Ok(ClientEvent::Suspend) => {
                terminal.leave();
                // SIGSTOP cannot be caught or blocked, so this really stops;
                // SIGCONT then arrives as Resume.
                let _ = nix::sys::signal::raise(Signal::SIGSTOP);
            }
            Ok(ClientEvent::Resume) => {
                terminal.reenter()?;
                // Whatever ran in the meantime drew over the screen, and the
                // terminal may have been resized: a resize, even to the same
                // size, makes the daemon repaint everything.
                let (cols, rows) = terminal_size()?;
                write_message(&mut stream, &ClientMessage::Resize { cols, rows })?
            }
            Ok(ClientEvent::Terminal(_)) => {}
            Ok(ClientEvent::TerminalError(error)) => bail!("terminal input: {error}"),
        }
    }
}

fn write_terminal_clipboard(
    output: &mut impl Write,
    selection: &[u8],
    data: &[u8],
) -> std::io::Result<()> {
    write!(
        output,
        "\x1b]52;{};{}\x07",
        String::from_utf8_lossy(selection),
        STANDARD.encode(data)
    )?;
    output.flush()
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
        env::var_os("WT_SESSION").as_deref(),
    )
}

fn truecolor_from(
    colorterm: Option<&OsStr>,
    term: Option<&OsStr>,
    wt_session: Option<&OsStr>,
) -> bool {
    // Windows Terminal exposes WT_SESSION in WSL but may omit COLORTERM.
    if wt_session.is_some_and(|value| !value.is_empty()) {
        return true;
    }
    if colorterm.is_some_and(|value| value == "truecolor" || value == "24bit") {
        return true;
    }
    // Some terminals say so in TERM instead of setting COLORTERM at all.
    term.and_then(OsStr::to_str).is_some_and(|term| {
        term == "xterm-kitty" || term.contains("direct") || term.contains("truecolor")
    })
}

/// Whether the terminal draws curly, dotted and dashed underlines and SGR 58
/// underline colours. Only terminals known to are sent them: to any other,
/// the colour's parameters read as unrelated attributes. `var` reads the
/// environment.
fn styled_underlines_from(var: impl Fn(&str) -> Option<String>) -> bool {
    // An explicit answer, for a terminal (or a test oracle) that is capable
    // but cannot be recognised. Unlike the configuration key, older mux
    // versions simply ignore it.
    match var("MUX_STYLED_UNDERLINES").as_deref() {
        Some("1") => return true,
        Some("0") => return false,
        _ => {}
    }
    // A client inside a mux pane draws into mux, which understands them and
    // passes them on as its own client's terminal allows.
    if var("MUX").is_some_and(|value| !value.is_empty()) {
        return true;
    }
    let term = var("TERM").unwrap_or_default();
    if [
        "kitty",
        "wezterm",
        "foot",
        "ghostty",
        "alacritty",
        "contour",
        "rio",
    ]
    .iter()
    .any(|name| term.contains(name))
    {
        return true;
    }
    if var("TERM_PROGRAM").is_some_and(|program| {
        matches!(
            program.as_str(),
            "WezTerm" | "ghostty" | "iTerm.app" | "vscode" | "rio"
        )
    }) {
        return true;
    }
    if [
        "KITTY_WINDOW_ID",
        "WEZTERM_EXECUTABLE",
        "GHOSTTY_RESOURCES_DIR",
        "ALACRITTY_WINDOW_ID",
    ]
    .iter()
    .any(|name| var(name).is_some())
    {
        return true;
    }
    // VTE (GNOME Terminal, Tilix, ...) has had both since 0.52.
    var("VTE_VERSION")
        .and_then(|version| version.parse::<u32>().ok())
        .is_some_and(|version| version >= 5200)
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
    read_message(&mut stream).context(
        "read daemon response; if mux was just updated, restart the daemon so client and daemon use the same version",
    )
}

/// The pane a script is running in, which tells the daemon which session it
/// means without the script having to be attached to one.
fn origin_pane() -> Option<usize> {
    env::var("MUX_PANE").ok()?.parse().ok()
}

pub fn stop() -> Result<()> {
    stop_at(&socket_path())
}

fn stop_at(path: &Path) -> Result<()> {
    let mut stream =
        UnixStream::connect(path).with_context(|| format!("connect to {}", path.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    // Existing version 2 daemons predate the stable shutdown control message.
    // Try their ordinary request first, then reconnect using version 1's
    // permanent encoding if the daemon rejects our interactive protocol.
    let response = (|| -> Result<Option<ServerMessage>> {
        write_message(&mut stream, &ClientMessage::Shutdown)?;
        read_message(&mut stream)
    })();
    match response {
        Ok(Some(ServerMessage::Detached)) => return Ok(()),
        Ok(Some(ServerMessage::Error(error))) => bail!("server: {error}"),
        _ => {}
    }
    drop(stream);
    let mut stream =
        UnixStream::connect(path).with_context(|| format!("connect to {}", path.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    crate::protocol::write_shutdown(&mut stream)?;
    crate::protocol::read_shutdown_response(&mut stream)
}

pub fn command(command: MuxCommand, pane: Option<usize>) -> Result<()> {
    let message = ClientMessage::Command {
        pane_id: pane.or_else(origin_pane),
        command,
    };
    match request(message)? {
        Some(ServerMessage::Done) => Ok(()),
        Some(ServerMessage::Error(error)) => bail!("server: {error}"),
        _ => bail!("multiplexer server disconnected before completing command"),
    }
}

/// Asks the daemon a question and prints the answer, one item per line.
pub fn query(query: MuxQuery, pane: Option<usize>, json: bool) -> Result<()> {
    let message = ClientMessage::Query {
        pane_id: pane.or_else(origin_pane),
        query,
        json,
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
    let mut child = Command::new(executable)
        .arg("__server")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("start multiplexer daemon")?;
    let stderr = child
        .stderr
        .take()
        .context("capture daemon startup errors")?;
    let stderr = thread::spawn(move || capture_stderr(stderr));
    wait_for_daemon(path, child, stderr, Instant::now() + Duration::from_secs(2))
}

fn wait_for_daemon(
    path: &Path,
    mut child: std::process::Child,
    stderr: thread::JoinHandle<String>,
    deadline: Instant,
) -> Result<UnixStream> {
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(_) if Instant::now() < deadline => {
                // A concurrently started daemon may win the state lock and
                // open the socket after this child exits, so keep waiting.
                let _ = child.try_wait()?;
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let stderr = stderr.join().unwrap_or_default();
                return daemon_start_error(path, error, &stderr);
            }
        }
    }
}

fn daemon_start_error(
    path: &Path,
    connect_error: std::io::Error,
    stderr: &str,
) -> Result<UnixStream> {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        Err(connect_error).with_context(|| format!("daemon did not open {}", path.display()))
    } else {
        Err(connect_error)
            .with_context(|| format!("daemon did not open {}: {stderr}", path.display()))
    }
}

fn capture_stderr(mut stderr: impl Read) -> String {
    const LIMIT: usize = 64 * 1024;
    let mut captured = Vec::new();
    let mut chunk = [0; 4096];
    while let Ok(length) = stderr.read(&mut chunk) {
        if length == 0 {
            break;
        }
        captured.extend_from_slice(&chunk[..length]);
        if captured.len() > LIMIT {
            captured.drain(..captured.len() - LIMIT);
        }
    }
    String::from_utf8_lossy(&captured).into_owned()
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
        // A terminal sends 0x1c through 0x1f for Ctrl-\, Ctrl-], Ctrl-^ and
        // Ctrl-_ (or Ctrl-/), and crossterm names those bytes Ctrl-4 through
        // Ctrl-7 after the digits that also produce them on a US keyboard. The
        // daemon encodes the key it is given, so give it the one the byte means.
        CrosstermKeyCode::Char(digit @ '4'..='7') if modifier_bits & CTRL != 0 => {
            modifier_bits &= !SHIFT;
            KeyCode::Char(match digit {
                '4' => '\\',
                '5' => ']',
                '6' => '^',
                _ => '_',
            })
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
        CrosstermKeyCode::F(number @ 1..=12) => KeyCode::F(number),
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

/// The signals the client handles itself rather than dying of.
fn client_signals() -> SigSet {
    let mut signals = SigSet::empty();
    for signal in [
        Signal::SIGTERM,
        Signal::SIGHUP,
        Signal::SIGINT,
        Signal::SIGQUIT,
        Signal::SIGTSTP,
        Signal::SIGCONT,
    ] {
        signals.add(signal);
    }
    signals
}

/// The terminal set up for mux, and put back when the client ends or stops.
struct TerminalGuard {
    mouse: bool,
    entered: bool,
}

impl TerminalGuard {
    fn enter(mouse: bool) -> Result<Self> {
        let mut guard = Self {
            mouse,
            entered: false,
        };
        guard.reenter()?;
        Ok(guard)
    }

    fn reenter(&mut self) -> Result<()> {
        enable_raw_mode()?;
        self.entered = true;
        execute!(
            stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableFocusChange,
            Hide
        )?;
        // Every frame positions its own text; autowrap could only ever make a
        // glyph at the right edge wrap or scroll the screen.
        stdout().write_all(b"\x1b[?7l")?;
        stdout().flush()?;
        if self.mouse {
            execute!(stdout(), EnableMouseCapture)?;
        }
        Ok(())
    }

    fn leave(&mut self) {
        if !std::mem::take(&mut self.entered) {
            return;
        }
        if self.mouse {
            let _ = execute!(stdout(), DisableMouseCapture);
        }
        let _ = stdout().write_all(b"\x1b[?7h");
        let _ = execute!(
            stdout(),
            SetCursorStyle::DefaultUserShape,
            Show,
            DisableFocusChange,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.leave();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stop_retries_with_the_old_daemons_shutdown_encoding() {
        let directory = env::temp_dir().join(format!("mux-stop-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("daemon.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let daemon = thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let mut header = [0; 6];
            first.read_exact(&mut header).unwrap();
            assert_eq!(&header[..4], b"MUXP");
            assert_eq!(&header[4..], &crate::protocol::WIRE_VERSION.to_be_bytes());
            drop(first); // Version 1 rejects the current protocol header.
            let (mut retry, _) = listener.accept().unwrap();
            let mut shutdown = [0; 11];
            retry.read_exact(&mut shutdown).unwrap();
            assert_eq!(&shutdown, b"MUXP\x00\x01\x00\x00\x00\x01\x08");
            retry
                .write_all(b"MUXP\x00\x01\x00\x00\x00\x01\x03")
                .unwrap();
        });
        let result = stop_at(&socket);
        daemon.join().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        result.unwrap();
    }

    use super::*;

    #[test]
    fn control_digits_from_crossterm_are_the_punctuation_that_sent_them() {
        let control =
            |character| convert_key(CrosstermKeyCode::Char(character), KeyModifiers::CONTROL);
        for (digit, meant) in [('4', '\\'), ('5', ']'), ('6', '^'), ('7', '_')] {
            assert_eq!(
                control(digit),
                Some(Key {
                    code: KeyCode::Char(meant),
                    modifiers: CTRL,
                }),
                "Ctrl-{digit}"
            );
        }
    }

    #[test]
    fn styled_underlines_are_detected_only_for_terminals_known_to_draw_them() {
        let with = |pairs: &[(&str, &str)]| {
            let pairs: Vec<(String, String)> = pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect();
            styled_underlines_from(move |name| {
                pairs
                    .iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.clone())
            })
        };
        assert!(!with(&[("TERM", "xterm-256color")]));
        assert!(!with(&[("TERM", "screen-256color"), ("TMUX", "/tmp/x")]));
        assert!(!with(&[("TERM_PROGRAM", "Apple_Terminal")]));
        assert!(!with(&[("VTE_VERSION", "5000")]));
        assert!(with(&[("TERM", "xterm-kitty")]));
        assert!(with(&[
            ("TERM", "xterm-256color"),
            ("TERM_PROGRAM", "WezTerm")
        ]));
        assert!(with(&[("TERM", "xterm-256color"), ("VTE_VERSION", "7600")]));
        assert!(with(&[("MUX", "/run/user/1/mux.sock")]));
        assert!(with(&[
            ("TERM", "xterm-256color"),
            ("MUX_STYLED_UNDERLINES", "1")
        ]));
        assert!(!with(&[
            ("TERM", "xterm-kitty"),
            ("MUX_STYLED_UNDERLINES", "0")
        ]));
    }

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
    fn function_keys_are_forwarded_through_the_protocol() {
        assert_eq!(
            convert_key(CrosstermKeyCode::F(12), KeyModifiers::CONTROL),
            Some(Key {
                code: KeyCode::F(12),
                modifiers: CTRL,
            })
        );
        assert_eq!(
            convert_key(CrosstermKeyCode::F(13), KeyModifiers::NONE),
            None
        );
    }

    #[test]
    fn truecolor_is_taken_from_colorterm_or_a_direct_term() {
        let colorterm = |value| Some(OsStr::new(value));
        assert!(truecolor_from(colorterm("truecolor"), None, None));
        assert!(truecolor_from(colorterm("24bit"), None, None));
        assert!(truecolor_from(None, colorterm("xterm-direct"), None));
        assert!(truecolor_from(None, colorterm("xterm-kitty"), None));
        // Anything that has not said so is painted for 256 colours.
        assert!(!truecolor_from(None, None, None));
        assert!(!truecolor_from(None, colorterm("xterm-256color"), None));
        assert!(!truecolor_from(
            colorterm("8bit"),
            colorterm("screen"),
            None
        ));
    }

    #[test]
    fn windows_terminal_session_enables_truecolor_without_colorterm() {
        let term = Some(OsStr::new("xterm-256color"));
        assert!(truecolor_from(None, term, Some(OsStr::new("session-id"))));
        assert!(!truecolor_from(None, term, Some(OsStr::new(""))));
        assert!(!truecolor_from(None, term, None));
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

    #[test]
    fn terminal_clipboard_is_an_osc_52_write() {
        let mut output = Vec::new();
        write_terminal_clipboard(&mut output, b"c", b"copied text").unwrap();
        assert_eq!(output, b"\x1b]52;c;Y29waWVkIHRleHQ=\x07");
    }

    #[test]
    fn daemon_start_error_includes_captured_stderr() {
        let socket = env::temp_dir().join(format!("mux-test-{}.sock", std::process::id()));
        let mut child = Command::new("/bin/sh")
            .args(["-c", "echo daemon setup failed >&2; exit 1"])
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let stderr = thread::spawn(move || capture_stderr(stderr));
        let error = wait_for_daemon(
            &socket,
            child,
            stderr,
            Instant::now() + Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.to_string().contains("daemon setup failed"));
        assert!(error.to_string().contains(socket.to_str().unwrap()));
    }
}
