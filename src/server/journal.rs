//! A pane's append-only record of everything its terminal has shown.

use std::{
    fs::File,
    io::{BufReader, BufWriter, ErrorKind, Read, Write},
    sync::mpsc::{self, Receiver, Sender, SyncSender},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};

use crate::frame::{ColorDepth, write_cell_attributes};

use super::{
    snapshot::snapshot_screen,
    terminal::{SCROLLBACK_LINES, process_terminal_bytes},
};

pub(super) const JOURNAL_OUTPUT: u8 = 1;

pub(super) const JOURNAL_RESIZE: u8 = 2;

const MAX_JOURNAL_RECORD: usize = 16 * 1024 * 1024;

/// Journal size that triggers compaction before the next event loop starts.
/// How large a journal may grow before it is worth rewriting. A pane's whole
/// scrollback compacts to a few megabytes, and every byte over that is replayed
/// again at every startup, so the bar is low and compaction is frequent.
pub(super) const MAX_JOURNAL_BYTES: u64 = 4 * 1024 * 1024;

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
/// Records are written on a worker so storage latency never stalls terminal
/// parsing, input, or rendering in the daemon thread.
pub(super) struct PaneJournal {
    sender: Sender<JournalCommand>,
    failures: Receiver<String>,
    pub(super) length: u64,
    /// What this journal has to reach to be worth rewriting again. A pane whose
    /// content genuinely compacts to near the limit would otherwise be rewritten
    /// on every idle moment, so the bar rises with what compaction achieved.
    compact_at: u64,
    /// Set once a write has failed. The pane keeps running with a history that
    /// stops here, which is a far smaller loss than the pane itself.
    pub(super) abandoned: bool,
}

enum JournalCommand {
    Write(Vec<u8>),
    Flush(SyncSender<std::io::Result<()>>),
    Replace(File, Vec<u8>, SyncSender<std::io::Result<()>>),
    Truncate(u64, SyncSender<std::io::Result<()>>),
}

impl PaneJournal {
    pub(super) fn new(file: File, length: u64) -> Self {
        let (sender, receiver) = mpsc::channel();
        let (failure_sender, failures) = mpsc::channel();
        thread::spawn(move || journal_writer(file, receiver, failure_sender));
        Self {
            sender,
            failures,
            length,
            compact_at: MAX_JOURNAL_BYTES,
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
        if let Some(error) = self.worker_failure() {
            self.abandoned = true;
            return Err(anyhow::anyhow!(error));
        }
        match self.queue_record(kind, payload) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.abandoned = true;
                Err(error)
            }
        }
    }

    fn queue_record(&mut self, kind: u8, payload: &[u8]) -> Result<()> {
        let record = encode_journal_record(kind, payload)?;
        let length = record.len() as u64;
        self.sender
            .send(JournalCommand::Write(record))
            .context("pane journal writer stopped")?;
        self.length += length;
        Ok(())
    }

    pub(super) fn append_output(&mut self, bytes: &[u8]) -> Result<()> {
        self.append(JOURNAL_OUTPUT, bytes)
    }

    pub(super) fn append_resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.append(JOURNAL_RESIZE, &resize_payload(rows, cols))
    }

    pub(super) fn flush(&mut self) -> Result<()> {
        if self.abandoned {
            return Ok(());
        }
        let (sender, receiver) = mpsc::sync_channel(0);
        let result = self
            .sender
            .send(JournalCommand::Flush(sender))
            .context("pane journal writer stopped")
            .and_then(|()| {
                receiver
                    .recv()
                    .context("pane journal writer stopped")?
                    .context("flush pane journal")
            });
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                self.abandoned = true;
                Err(error)
            }
        }
    }

    pub(super) fn needs_compaction(&self) -> bool {
        !self.abandoned && self.length > self.compact_at
    }

    pub(super) fn poll_failure(&mut self) -> Result<()> {
        if self.abandoned {
            return Ok(());
        }
        if let Some(error) = self.worker_failure() {
            self.abandoned = true;
            return Err(anyhow::anyhow!(error));
        }
        Ok(())
    }

    /// Replaces the journal with `records`, which must replay to the same
    /// screen the pane is showing now.
    pub(super) fn replace(&mut self, file: File, records: &[u8]) -> Result<()> {
        let (sender, receiver) = mpsc::sync_channel(0);
        self.sender
            .send(JournalCommand::Replace(file, records.to_vec(), sender))
            .context("pane journal writer stopped")?;
        receiver
            .recv()
            .context("pane journal writer stopped")?
            .context("replace pane journal")?;
        self.length = records.len() as u64;
        self.compact_at = MAX_JOURNAL_BYTES.max(self.length.saturating_mul(2));
        Ok(())
    }

    pub(super) fn truncate(&mut self, length: u64) -> Result<()> {
        let (sender, receiver) = mpsc::sync_channel(0);
        self.sender
            .send(JournalCommand::Truncate(length, sender))
            .context("pane journal writer stopped")?;
        receiver
            .recv()
            .context("pane journal writer stopped")?
            .context("truncate pane journal")?;
        self.length = length;
        Ok(())
    }

    fn worker_failure(&self) -> Option<String> {
        self.failures.try_recv().ok()
    }
}

fn journal_writer(file: File, receiver: Receiver<JournalCommand>, failures: Sender<String>) {
    let mut file = BufWriter::with_capacity(64 * 1024, file);
    let mut failure = None;
    let mut dirty = false;
    loop {
        match receiver.recv_timeout(Duration::from_millis(8)) {
            Ok(JournalCommand::Write(record)) => {
                if failure.is_none() {
                    match file.write_all(&record) {
                        Ok(()) => dirty = true,
                        Err(error) => record_failure(&mut failure, &failures, error),
                    }
                }
            }
            Ok(JournalCommand::Flush(reply)) => {
                let result = flush_journal(&mut file, &mut dirty, &mut failure, &failures);
                let _ = reply.send(result);
            }
            Ok(JournalCommand::Replace(new_file, records, reply)) => {
                file = BufWriter::with_capacity(64 * 1024, new_file);
                failure = None;
                dirty = false;
                let result = file.write_all(&records).and_then(|()| file.flush());
                if let Err(error) = &result {
                    record_failure(
                        &mut failure,
                        &failures,
                        std::io::Error::new(error.kind(), error.to_string()),
                    );
                }
                let _ = reply.send(result);
            }
            Ok(JournalCommand::Truncate(length, reply)) => {
                let result = flush_journal(&mut file, &mut dirty, &mut failure, &failures)
                    .and_then(|()| file.get_ref().set_len(length));
                let _ = reply.send(result);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = flush_journal(&mut file, &mut dirty, &mut failure, &failures);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = flush_journal(&mut file, &mut dirty, &mut failure, &failures);
                return;
            }
        }
    }
}

fn flush_journal(
    file: &mut BufWriter<File>,
    dirty: &mut bool,
    failure: &mut Option<String>,
    failures: &Sender<String>,
) -> std::io::Result<()> {
    if let Some(error) = failure.as_ref() {
        return Err(std::io::Error::other(error.clone()));
    }
    if !*dirty {
        return Ok(());
    }
    match file.flush() {
        Ok(()) => {
            *dirty = false;
            Ok(())
        }
        Err(error) => {
            let returned = std::io::Error::new(error.kind(), error.to_string());
            record_failure(failure, failures, error);
            Err(returned)
        }
    }
}

fn record_failure(failure: &mut Option<String>, failures: &Sender<String>, error: std::io::Error) {
    let message = error.to_string();
    *failure = Some(message.clone());
    let _ = failures.send(message);
}

/// Builds a journal that replays to the current screen and the complete
/// configured scrollback, discarding only rows beyond that limit.
pub(super) fn compacted_journal_records(screen: &mut vt100::Screen) -> Result<Vec<u8>> {
    let (rows, cols) = screen.size();
    let (buffer, _) = snapshot_screen(screen);
    let start = buffer
        .len()
        .saturating_sub(SCROLLBACK_LINES + usize::from(rows));
    let last_content = (start..buffer.len()).rposition(|row| {
        let line = buffer.line(row);
        line.cells
            .iter()
            .any(|cell| !cell.contents(&line.text).is_empty())
    });
    let end = start + last_content.map_or(0, |index| index + 1);
    let mut output = Vec::new();
    for (index, line) in (start..end).map(|row| buffer.line(row)).enumerate() {
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
