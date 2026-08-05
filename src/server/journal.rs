//! A pane's append-only record of everything its terminal has shown.

use std::{
    fs::File,
    io::{BufReader, BufWriter, ErrorKind, Read, Write},
};

use anyhow::{Context, Result, bail};

use crate::frame::{ColorDepth, write_cell_attributes};

use super::{snapshot::snapshot_screen, terminal::process_terminal_bytes};

pub(super) const JOURNAL_OUTPUT: u8 = 1;

pub(super) const JOURNAL_RESIZE: u8 = 2;

const MAX_JOURNAL_RECORD: usize = 16 * 1024 * 1024;

/// Journal size that triggers compaction the next time the daemon is idle.
pub(super) const MAX_JOURNAL_BYTES: u64 = 32 * 1024 * 1024;

/// Terminal rows a compacted journal keeps.
const COMPACTED_JOURNAL_ROWS: usize = 5_000;

pub(super) fn encode_journal_record(kind: u8, payload: &[u8]) -> Result<Vec<u8>> {
    let length = u32::try_from(payload.len()).context("pane journal record is too large")?;
    let mut record = Vec::with_capacity(payload.len() + 5);
    record.push(kind);
    record.extend_from_slice(&length.to_be_bytes());
    record.extend_from_slice(payload);
    Ok(record)
}

fn resize_payload(rows: u16, cols: u16) -> [u8; 4] {
    let mut payload = [0; 4];
    payload[..2].copy_from_slice(&rows.to_be_bytes());
    payload[2..].copy_from_slice(&cols.to_be_bytes());
    payload
}

/// A pane's append-only record of everything its terminal has shown.
///
/// Records are buffered so a flood of small PTY chunks becomes a handful of
/// writes; the daemon flushes as soon as it goes idle, which is exactly when
/// unflushed output would otherwise be at risk.
pub(super) struct PaneJournal {
    pub(super) file: BufWriter<File>,
    pub(super) length: u64,
    pub(super) unflushed: bool,
    /// Set once a write has failed. The pane keeps running with a history that
    /// stops here, which is a far smaller loss than the pane itself.
    pub(super) abandoned: bool,
}

impl PaneJournal {
    pub(super) fn new(file: File, length: u64) -> Self {
        Self {
            file: BufWriter::with_capacity(64 * 1024, file),
            length,
            unflushed: false,
            abandoned: false,
        }
    }

    /// Appends a record. The first failure is reported; after that the journal
    /// stays quiet, so a full disk does not repeat itself on every chunk of
    /// terminal output.
    pub(super) fn append(&mut self, kind: u8, payload: &[u8]) -> Result<()> {
        if self.abandoned {
            return Ok(());
        }
        match self.write_record(kind, payload) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.abandoned = true;
                Err(error)
            }
        }
    }

    fn write_record(&mut self, kind: u8, payload: &[u8]) -> Result<()> {
        let record = encode_journal_record(kind, payload)?;
        self.file.write_all(&record)?;
        self.length += record.len() as u64;
        self.unflushed = true;
        Ok(())
    }

    pub(super) fn append_output(&mut self, bytes: &[u8]) -> Result<()> {
        self.append(JOURNAL_OUTPUT, bytes)
    }

    pub(super) fn append_resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.append(JOURNAL_RESIZE, &resize_payload(rows, cols))
    }

    pub(super) fn flush(&mut self) -> Result<()> {
        if !self.unflushed || self.abandoned {
            return Ok(());
        }
        match self.file.flush().context("flush pane journal") {
            Ok(()) => {
                self.unflushed = false;
                Ok(())
            }
            Err(error) => {
                self.abandoned = true;
                Err(error)
            }
        }
    }

    pub(super) fn needs_compaction(&self) -> bool {
        !self.abandoned && self.length > MAX_JOURNAL_BYTES
    }

    /// Replaces the journal with `records`, which must replay to the same
    /// screen the pane is showing now.
    pub(super) fn replace(&mut self, file: File, records: &[u8]) -> Result<()> {
        self.file = BufWriter::with_capacity(64 * 1024, file);
        self.length = 0;
        self.unflushed = false;
        self.file.write_all(records)?;
        self.length = records.len() as u64;
        self.unflushed = true;
        self.flush()
    }

    pub(super) fn truncate(&mut self, length: u64) -> Result<()> {
        self.flush()?;
        self.file.get_ref().set_len(length)?;
        self.length = length;
        Ok(())
    }
}

/// Builds a journal that replays to the current screen and the newest
/// `COMPACTED_JOURNAL_ROWS` rows of its scrollback, discarding everything the
/// terminal has already scrolled away.
pub(super) fn compacted_journal_records(screen: &mut vt100::Screen) -> Result<Vec<u8>> {
    let (rows, cols) = screen.size();
    let (lines, _) = snapshot_screen(screen);
    let start = lines.len().saturating_sub(COMPACTED_JOURNAL_ROWS);
    let kept = &lines[start..];
    let last_content = kept.iter().rposition(|line| {
        line.cells
            .iter()
            .any(|cell| !cell.contents(&line.text).is_empty())
    });
    let kept = &kept[..last_content.map_or(0, |index| index + 1)];
    let mut output = Vec::new();
    for (index, line) in kept.iter().enumerate() {
        if index > 0 {
            output.extend_from_slice(b"\r\n");
        }
        let mut attributes = None;
        for cell in &line.cells {
            if cell.wide_continuation {
                continue;
            }
            if attributes != Some(cell.attributes) {
                // A journal is replayed back through the parser rather than
                // sent to a terminal, so it keeps its exact colours.
                write_cell_attributes(&mut output, cell.attributes, ColorDepth::TrueColor);
                attributes = Some(cell.attributes);
            }
            let contents = cell.contents(&line.text);
            output.extend_from_slice(if contents.is_empty() {
                b" "
            } else {
                contents.as_bytes()
            });
        }
    }
    output.extend_from_slice(b"\x1b[0m");
    let mut records = encode_journal_record(JOURNAL_RESIZE, &resize_payload(rows, cols))?;
    records.extend_from_slice(&encode_journal_record(JOURNAL_OUTPUT, &output)?);
    Ok(records)
}

pub(super) fn replay_pane_journal<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    parser_prefix: &mut Vec<u8>,
    reader: impl Read,
) -> Result<u64> {
    let mut reader = BufReader::with_capacity(64 * 1024, reader);
    let mut offset = 0u64;
    loop {
        let record_start = offset;
        let mut header = [0; 5];
        if let Err(error) = reader.read_exact(&mut header) {
            return if error.kind() == ErrorKind::UnexpectedEof {
                Ok(record_start)
            } else {
                Err(error.into())
            };
        }
        let kind = header[0];
        let length = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
        if length > MAX_JOURNAL_RECORD {
            bail!("pane journal record exceeds 16 MiB");
        }
        offset += 5;
        let mut payload = vec![0; length];
        if let Err(error) = reader.read_exact(&mut payload) {
            return if error.kind() == ErrorKind::UnexpectedEof {
                Ok(record_start)
            } else {
                Err(error.into())
            };
        }
        match kind {
            JOURNAL_OUTPUT => process_terminal_bytes(parser, parser_prefix, &payload),
            JOURNAL_RESIZE => {
                if payload.len() != 4 {
                    bail!("pane journal contains an invalid resize record");
                }
                let rows = u16::from_be_bytes(payload[..2].try_into().unwrap()).max(1);
                let cols = u16::from_be_bytes(payload[2..].try_into().unwrap()).max(1);
                parser.screen_mut().set_size(rows, cols);
            }
            _ => bail!("pane journal contains unknown record type {kind}"),
        }
        offset += u64::try_from(length).unwrap();
    }
}
