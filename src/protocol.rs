use std::{
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::config::{BellStyle, Bindings, Glyphs, Theme};
use crate::frame::{CursorShape, TerminalFeatures};

const WIRE_MAGIC: [u8; 4] = *b"MUXP";
pub const WIRE_VERSION: u16 = 2;
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
    RefreshClient,
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
    pub terminal: TerminalFeatures,
    /// Which glyphs this client's font has for the sidebar.
    pub glyphs: Glyphs,
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
    /// The client's terminal gained (`true`) or lost focus.
    Focus(bool),
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

/// Version 1's shutdown frame is a permanent control message, independent of
/// `ClientMessage`'s variant order and of the interactive protocol version.
const SHUTDOWN_FRAME: &[u8] = b"MUXP\x00\x01\x00\x00\x00\x01\x08";

fn wire_config() -> impl bincode::config::Config {
    bincode::config::standard().with_limit::<MAX_MESSAGE_SIZE>()
}

pub fn write_message<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<()> {
    let bytes =
        bincode::serde::encode_to_vec(value, wire_config()).context("encode protocol message")?;
    if bytes.len() > MAX_MESSAGE_SIZE {
        bail!("protocol message exceeds 16 MiB");
    }
    writer.write_all(&WIRE_MAGIC)?;
    writer.write_all(&WIRE_VERSION.to_be_bytes())?;
    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

pub fn write_shutdown(writer: &mut impl Write) -> Result<()> {
    writer.write_all(SHUTDOWN_FRAME)?;
    writer.flush()?;
    Ok(())
}

pub fn read_shutdown_response(reader: &mut impl Read) -> Result<()> {
    let Some((_, bytes)) = read_frame(reader)? else {
        bail!("multiplexer server disconnected before confirming shutdown");
    };
    // Detached has the same single-byte encoding in both framed protocols.
    if bytes != [3] {
        bail!("multiplexer server did not confirm shutdown");
    }
    Ok(())
}

pub fn read_client_message(reader: &mut impl Read) -> Result<Option<ClientMessage>> {
    let Some((version, bytes)) = read_frame(reader)? else {
        return Ok(None);
    };
    if version == 1 && bytes == [8] {
        return Ok(Some(ClientMessage::Shutdown));
    }
    decode_message(version, &bytes).map(Some)
}

pub fn read_message<T: DeserializeOwned>(reader: &mut impl Read) -> Result<Option<T>> {
    let Some((version, bytes)) = read_frame(reader)? else {
        return Ok(None);
    };
    decode_message(version, &bytes).map(Some)
}

/// Reads one frame's version and payload; `None` only for a clean disconnect
/// before the first byte.
fn read_frame(reader: &mut impl Read) -> Result<Option<(u16, Vec<u8>)>> {
    let mut magic = [0; 4];
    if reader.read(&mut magic[..1])? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut magic[1..])?;
    if magic != WIRE_MAGIC {
        bail!("incompatible mux protocol; client and daemon must use the same mux version");
    }
    let mut header = [0; 6];
    reader.read_exact(&mut header)?;
    let version = u16::from_be_bytes([header[0], header[1]]);
    let length = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;
    if length > MAX_MESSAGE_SIZE {
        bail!("protocol message exceeds 16 MiB");
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(Some((version, bytes)))
}

fn decode_message<T: DeserializeOwned>(version: u16, bytes: &[u8]) -> Result<T> {
    if version != WIRE_VERSION {
        bail!(
            "incompatible mux protocol version {version} (expected {WIRE_VERSION}); client and daemon must use the same mux version"
        );
    }
    let (value, used) = bincode::serde::decode_from_slice(bytes, wire_config())
        .context("decode protocol message")?;
    if used != bytes.len() {
        bail!("protocol message contains trailing bytes");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(version: u16, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::from(WIRE_MAGIC);
        bytes.extend_from_slice(&version.to_be_bytes());
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn shutdown_uses_the_version_one_encoding_and_nothing_else_does() {
        let mut bytes = Vec::new();
        write_shutdown(&mut bytes).unwrap();
        assert_eq!(bytes, frame(1, &[8]));
        assert!(matches!(
            read_client_message(&mut bytes.as_slice()).unwrap(),
            Some(ClientMessage::Shutdown)
        ));
        for payload in [&[5][..], &[8, 0]] {
            assert!(read_client_message(&mut frame(1, payload).as_slice()).is_err());
        }
    }

    #[test]
    fn shutdown_confirmation_survives_protocol_version_changes() {
        for version in [1_u16, WIRE_VERSION, WIRE_VERSION + 1] {
            read_shutdown_response(&mut frame(version, &[3]).as_slice()).unwrap();
        }
        let mut bytes = Vec::new();
        write_message(&mut bytes, &ServerMessage::Done).unwrap();
        assert!(read_shutdown_response(&mut bytes.as_slice()).is_err());
        assert!(read_shutdown_response(&mut &[][..]).is_err());
    }

    #[test]
    fn framed_message_round_trips() {
        let mut bytes = Vec::new();
        write_message(&mut bytes, &ServerMessage::Detached).unwrap();
        assert_eq!(bytes, frame(WIRE_VERSION, &[3]));
        assert!(matches!(
            read_message(&mut bytes.as_slice()).unwrap(),
            Some(ServerMessage::Detached)
        ));
    }

    #[test]
    fn wrong_wire_version_has_an_actionable_error() {
        let bytes = frame(WIRE_VERSION + 1, &[]);
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
    fn oversized_messages_are_rejected() {
        let mut bytes = Vec::from(WIRE_MAGIC);
        bytes.extend_from_slice(&WIRE_VERSION.to_be_bytes());
        bytes.extend_from_slice(&((MAX_MESSAGE_SIZE as u32) + 1).to_be_bytes());
        let error = read_message::<ServerMessage>(&mut bytes.as_slice()).unwrap_err();
        assert_eq!(error.to_string(), "protocol message exceeds 16 MiB");

        let value = ServerMessage::Render(vec![0; MAX_MESSAGE_SIZE + 1]);
        let error = write_message(&mut Vec::new(), &value).unwrap_err();
        assert!(
            error.to_string().contains("encode protocol message")
                || error.to_string().contains("exceeds 16 MiB")
        );
    }
}
