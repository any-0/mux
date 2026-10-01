use std::{
    io::{self, IoSlice, Read, Write},
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
    JoinPane {
        window: u8,
        axis_is_vertical: bool,
    },
    SwapWindow(u8),
    JumpToBell,
    KillPane,
    KillSession,
    SelectWindow(u8),
    EnterVim,
    SetTheme(Theme),
    /// Repaint the whole screen of the attached client.
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
    /// What this client's terminal understands.
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
    let mut header = [0; 10];
    header[..4].copy_from_slice(&WIRE_MAGIC);
    header[4..6].copy_from_slice(&WIRE_VERSION.to_be_bytes());
    header[6..].copy_from_slice(&length.to_be_bytes());
    // UnixStream gathers these slices into one send without copying the
    // encoded payload. Generic writers can consume just the first slice.
    let mut buffers = [IoSlice::new(&header), IoSlice::new(&bytes)];
    let mut remaining = &mut buffers[..];
    while !remaining.is_empty() {
        match writer.write_vectored(remaining) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
            Ok(written) => IoSlice::advance_slices(&mut remaining, written),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    writer.flush()?;
    Ok(())
}

pub fn read_message<T: DeserializeOwned>(reader: &mut impl Read) -> Result<Option<T>> {
    let mut header = [0; 10];
    let mut received = loop {
        match reader.read(&mut header) {
            Ok(0) => return Ok(None),
            Ok(received) => break received,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    };
    // Check each prefix as soon as it arrives, retaining early rejection of
    // incompatible peers even if they stop sending before the complete header.
    if received < 4 {
        reader.read_exact(&mut header[received..4])?;
        received = 4;
    }
    if header[..4] != WIRE_MAGIC {
        bail!("incompatible mux protocol; client and daemon must use the same mux version");
    }
    if received < 6 {
        reader.read_exact(&mut header[received..6])?;
        received = 6;
    }
    let version = u16::from_be_bytes(header[4..6].try_into().unwrap());
    if version != WIRE_VERSION {
        bail!(
            "incompatible mux protocol version {version} (expected {WIRE_VERSION}); client and daemon must use the same mux version"
        );
    }
    if received < header.len() {
        reader.read_exact(&mut header[received..])?;
    }
    let length = u32::from_be_bytes(header[6..].try_into().unwrap()) as usize;
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

    struct ShortWriter {
        bytes: Vec<u8>,
        limit: usize,
        interrupted: bool,
        calls: usize,
    }

    impl Write for ShortWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let count = bytes.len().min(self.limit);
            self.bytes.extend_from_slice(&bytes[..count]);
            Ok(count)
        }

        fn write_vectored(&mut self, buffers: &[IoSlice<'_>]) -> io::Result<usize> {
            self.calls += 1;
            if self.interrupted {
                self.interrupted = false;
                return Err(io::ErrorKind::Interrupted.into());
            }
            let before = self.bytes.len();
            self.bytes.extend(
                buffers
                    .iter()
                    .flat_map(|buffer| buffer.iter())
                    .take(self.limit),
            );
            Ok(self.bytes.len() - before)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct ShortReader<'a> {
        bytes: &'a [u8],
        limit: usize,
        interrupted: bool,
    }

    impl Read for ShortReader<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if self.interrupted {
                self.interrupted = false;
                return Err(io::ErrorKind::Interrupted.into());
            }
            let count = self.bytes.len().min(output.len()).min(self.limit);
            output[..count].copy_from_slice(&self.bytes[..count]);
            self.bytes = &self.bytes[count..];
            Ok(count)
        }
    }

    #[test]
    fn gathered_frame_keeps_wire_bytes_and_handles_short_interrupted_writes() {
        let golden = b"MUXP\0\x01\0\0\0\x04\x01\0t\x02";
        for limit in 1..=golden.len() {
            let mut writer = ShortWriter {
                bytes: Vec::new(),
                limit,
                interrupted: true,
                calls: 0,
            };
            let key = Key {
                code: KeyCode::Char('t'),
                modifiers: ALT,
            };
            write_message(&mut writer, &ClientMessage::Key(key)).unwrap();
            assert_eq!(writer.bytes, golden);
            if limit == golden.len() {
                assert_eq!(writer.calls, 2); // interrupted, then one gathered write
            }
        }
        let mut writer = ShortWriter {
            bytes: Vec::new(),
            limit: 0,
            interrupted: false,
            calls: 0,
        };
        let error = write_message(&mut writer, &ServerMessage::Done).unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::WriteZero
        );
    }

    #[test]
    fn every_short_header_read_preserves_concatenated_frame_boundaries() {
        let mut bytes = Vec::new();
        write_message(&mut bytes, &ServerMessage::Done).unwrap();
        let payload = "界\x1b[4:3mtext".as_bytes().to_vec();
        write_message(&mut bytes, &ServerMessage::Render(payload.clone())).unwrap();
        for limit in 1..=bytes.len() {
            let mut reader = ShortReader {
                bytes: &bytes,
                limit,
                interrupted: true,
            };
            assert!(matches!(
                read_message(&mut reader).unwrap(),
                Some(ServerMessage::Done)
            ));
            let Some(ServerMessage::Render(actual)) = read_message(&mut reader).unwrap() else {
                panic!("lost the second frame");
            };
            assert_eq!(actual, payload);
            assert!(
                read_message::<ServerMessage>(&mut reader)
                    .unwrap()
                    .is_none()
            );
        }
        for end in 1..11 {
            assert!(read_message::<ServerMessage>(&mut &bytes[..end]).is_err());
        }
    }

    #[test]
    fn incompatible_prefix_is_rejected_without_waiting_for_remaining_header() {
        let wrong_magic = read_message::<ServerMessage>(&mut b"BAD!".as_slice()).unwrap_err();
        assert!(
            wrong_magic
                .to_string()
                .contains("incompatible mux protocol")
        );
        let wrong_version =
            read_message::<ServerMessage>(&mut b"MUXP\0\x02".as_slice()).unwrap_err();
        assert!(
            wrong_version
                .to_string()
                .contains("incompatible mux protocol version")
        );
    }

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
