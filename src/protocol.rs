use std::{
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::config::{BellStyle, Bindings, Theme};

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MuxCommand {
    ChooseTree,
    Detach,
    NewWindow,
    NewSession(Option<String>),
    SetSessionRoot,
    RenameSession(String),
    RenameWindow(String),
    SplitHorizontal,
    SplitVertical,
    FocusLeft,
    FocusDown,
    FocusUp,
    FocusRight,
    ResizeLeft(u16),
    ResizeDown(u16),
    ResizeUp(u16),
    ResizeRight(u16),
    ZoomPane,
    BreakPane,
    JoinPane { window: u8, axis_is_vertical: bool },
    SwapWindow(u8),
    JumpToBell,
    KillPane,
    KillSession,
    SelectWindow(u8),
    EnterVim,
    SetTheme(Theme),
}

/// A read-only question for the daemon. Unlike a command, it needs no attached
/// client, so scripts can ask about sessions from anywhere.
#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MuxQuery {
    Sessions,
    Windows,
    Panes,
}

/// A mouse event, in the client's screen coordinates counting from zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Mouse {
    pub kind: MouseKind,
    pub button: MouseButton,
    pub col: u16,
    pub row: u16,
    pub modifiers: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MouseKind {
    Down,
    Up,
    Drag,
    ScrollUp,
    ScrollDown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct Key {
    pub code: KeyCode,
    pub modifiers: u8,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum KeyCode {
    Char(char),
    Enter,
    Escape,
    Backspace,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Delete,
    Insert,
    PageUp,
    PageDown,
}

pub const SHIFT: u8 = 1;
pub const ALT: u8 = 2;
pub const CTRL: u8 = 4;

#[cfg(test)]
pub fn parse_for_test(value: &str) -> Key {
    crate::config::parse_key(value).unwrap()
}

/// Everything a client tells the daemon about itself when it attaches.
#[derive(Debug, Serialize, Deserialize)]
pub struct Hello {
    pub cols: u16,
    pub rows: u16,
    pub cwd: PathBuf,
    pub session: Option<String>,
    pub bindings: Bindings,
    pub clipboard_command: Vec<String>,
    /// Whether clipboard writes must travel through the attached terminal.
    pub terminal_clipboard: bool,
    pub theme: Theme,
    pub theme_command: Vec<String>,
    pub theme_directory: Option<PathBuf>,
    pub mouse: bool,
    pub bell_style: BellStyle,
    /// Whether this client's terminal renders 24-bit colour.
    pub truecolor: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Boxed: every other message is a few bytes, and the channel the daemon
    /// reads them from is sized by its largest variant.
    Hello(Box<Hello>),
    Key(Key),
    Mouse(Mouse),
    Paste(String),
    Resize {
        cols: u16,
        rows: u16,
    },
    Detach,
    Command {
        pane_id: Option<usize>,
        command: MuxCommand,
    },
    Query {
        pane_id: Option<usize>,
        query: MuxQuery,
    },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ServerMessage {
    Render(Vec<u8>),
    Clipboard(String),
    Listing(Vec<String>),
    Detached,
    Done,
    Error(String),
}

pub fn write_message<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<()> {
    let bytes = bincode::serde::encode_to_vec(value, bincode::config::standard())
        .context("encode protocol message")?;
    let length = u32::try_from(bytes.len()).context("protocol message is too large")?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

pub fn read_message<T: DeserializeOwned>(reader: &mut impl Read) -> Result<Option<T>> {
    let mut length = [0; 4];
    match reader.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let length = u32::from_be_bytes(length) as usize;
    if length > 16 * 1024 * 1024 {
        bail!("protocol message exceeds 16 MiB");
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    let (value, used) = bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
        .context("decode protocol message")?;
    if used != bytes.len() {
        bail!("protocol message contains trailing bytes");
    }
    Ok(Some(value))
}
