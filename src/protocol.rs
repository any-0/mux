use std::{
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::config::{BellStyle, Bindings, Theme};
use crate::frame::CursorShape;

const WIRE_MAGIC: [u8; 4] = *b"MUXP";
pub const WIRE_VERSION: u16 = 1;
const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

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
    F(u8),
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
    pub default_cursor_shape: CursorShape,
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
        json: bool,
    },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ServerMessage {
    Render(Vec<u8>),
    Clipboard { selection: Vec<u8>, data: Vec<u8> },
    Listing(Vec<String>),
    Detached,
    Done,
    Error(String),
}

pub fn write_message<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<()> {
    let bytes = bincode::serde::encode_to_vec(
        value,
        bincode::config::standard().with_limit::<MAX_MESSAGE_SIZE>(),
    )
    .context("encode protocol message")?;
    if bytes.len() > MAX_MESSAGE_SIZE {
        bail!("protocol message exceeds 16 MiB");
    }
    let length = u32::try_from(bytes.len()).context("protocol message is too large")?;
    writer.write_all(&WIRE_MAGIC)?;
    writer.write_all(&WIRE_VERSION.to_be_bytes())?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

pub fn read_message<T: DeserializeOwned>(reader: &mut impl Read) -> Result<Option<T>> {
    let mut magic = [0; 4];
    match reader.read(&mut magic[..1]) {
        Ok(0) => return Ok(None),
        Ok(_) => reader.read_exact(&mut magic[1..])?,
        Err(error) => return Err(error.into()),
    }
    if magic != WIRE_MAGIC {
        bail!("incompatible mux protocol; client and daemon must use the same mux version");
    }
    let mut version = [0; 2];
    reader.read_exact(&mut version)?;
    let version = u16::from_be_bytes(version);
    if version != WIRE_VERSION {
        bail!(
            "incompatible mux protocol version {version} (expected {WIRE_VERSION}); client and daemon must use the same mux version"
        );
    }
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_MESSAGE_SIZE {
        bail!("protocol message exceeds 16 MiB");
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    let (value, used) = bincode::serde::decode_from_slice(
        &bytes,
        bincode::config::standard().with_limit::<MAX_MESSAGE_SIZE>(),
    )
    .context("decode protocol message")?;
    if used != bytes.len() {
        bail!("protocol message contains trailing bytes");
    }
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framed_message_round_trips() {
        let mut bytes = Vec::new();
        write_message(&mut bytes, &ServerMessage::Done).unwrap();
        assert_eq!(&bytes[..4], &WIRE_MAGIC);
        assert!(matches!(
            read_message(&mut bytes.as_slice()).unwrap(),
            Some(ServerMessage::Done)
        ));
    }

    #[test]
    fn wrong_wire_version_has_an_actionable_error() {
        let mut bytes = Vec::from(WIRE_MAGIC);
        bytes.extend_from_slice(&(WIRE_VERSION + 1).to_be_bytes());
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        let error = read_message::<ServerMessage>(&mut bytes.as_slice()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("client and daemon must use the same mux version")
        );
    }

    #[test]
    fn partial_header_is_not_treated_as_a_clean_disconnect() {
        let error = read_message::<ServerMessage>(&mut &WIRE_MAGIC[..2]).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn oversized_inbound_message_is_rejected_before_allocation() {
        let mut bytes = Vec::from(WIRE_MAGIC);
        bytes.extend_from_slice(&WIRE_VERSION.to_be_bytes());
        bytes.extend_from_slice(&((MAX_MESSAGE_SIZE as u32) + 1).to_be_bytes());
        let error = read_message::<ServerMessage>(&mut bytes.as_slice()).unwrap_err();
        assert_eq!(error.to_string(), "protocol message exceeds 16 MiB");
    }

    #[test]
    fn oversized_outbound_message_is_rejected() {
        let value = ServerMessage::Render(vec![0; MAX_MESSAGE_SIZE + 1]);
        let error = write_message(&mut Vec::new(), &value).unwrap_err();
        assert!(
            error.to_string().contains("encode protocol message")
                || error.to_string().contains("exceeds 16 MiB")
        );
    }
}
