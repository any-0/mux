//! A pane's append-only record of everything its terminal has shown.

use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, ErrorKind, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender, SyncSender},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use super::{output_budget::OutputPermit, terminal::process_terminal_bytes};

pub(super) const JOURNAL_OUTPUT: u8 = 1;

pub(super) const JOURNAL_RESIZE: u8 = 2;
/// A pane's scrollback, packed the way the scrollback itself stores it. It
/// costs a read and a decompression to restore, where the same rows written as
/// terminal output cost a full parse.
pub(super) const JOURNAL_HISTORY: u8 = 3;

const MAX_JOURNAL_RECORD: usize = 16 * 1024 * 1024;
const JOURNAL_FLUSH_DELAY: Duration = Duration::from_millis(8);
const JOURNAL_SYNC_INTERVAL: Duration = Duration::from_secs(1);

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
    compaction_sender: Sender<()>,
    compactions: Receiver<()>,
    pub(super) length: u64,
    /// What this journal has to reach to be worth rewriting again. A pane whose
    /// content genuinely compacts to near the limit would otherwise be rewritten
    /// on every idle moment, so the bar rises with what compaction achieved.
    compact_at: u64,
    compacting: bool,
    /// Set once a write has failed. The pane keeps running with a history that
    /// stops here, which is a far smaller loss than the pane itself.
    pub(super) abandoned: bool,
}

enum JournalCommand {
    Write(Vec<u8>, Option<OutputPermit>),
    Flush(SyncSender<std::io::Result<()>>),
    #[cfg(test)]
    Replace(PathBuf, Vec<u8>, SyncSender<std::io::Result<()>>),
    ReplaceAsync(PathBuf, Vec<u8>, Sender<()>),
    Truncate(u64, SyncSender<std::io::Result<()>>),
}

impl PaneJournal {
    pub(super) fn new(file: File, length: u64) -> Self {
        let (sender, receiver) = mpsc::channel();
        let (failure_sender, failures) = mpsc::channel();
        let (compaction_sender, compactions) = mpsc::channel();
        thread::spawn(move || journal_writer(file, receiver, failure_sender));
        Self {
            sender,
            failures,
            compaction_sender,
            compactions,
            length,
            compact_at: MAX_JOURNAL_BYTES,
            compacting: false,
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
        match self.queue_record(kind, payload, None) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.abandoned = true;
                Err(error)
            }
        }
    }

    fn queue_record(
        &mut self,
        kind: u8,
        payload: &[u8],
        permit: Option<OutputPermit>,
    ) -> Result<()> {
        let record = encode_journal_record(kind, payload)?;
        let length = record.len() as u64;
        self.sender
            .send(JournalCommand::Write(record, permit))
            .context("pane journal writer stopped")?;
        self.length += length;
        Ok(())
    }

    pub(super) fn append_output(
        &mut self,
        bytes: &[u8],
        permit: Option<OutputPermit>,
    ) -> Result<()> {
        if self.abandoned {
            return Ok(());
        }
        if let Some(error) = self.worker_failure() {
            self.abandoned = true;
            return Err(anyhow::anyhow!(error));
        }
        self.queue_record(JOURNAL_OUTPUT, bytes, permit)
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

    /// Asks for this journal to be rewritten at the next quiet moment, however
    /// small it is. A journal restored from the old replayable form is worth
    /// packing even when it is well under the limit, because until it is packed
    /// it is parsed in full at every startup.
    pub(super) fn compact_soon(&mut self) {
        self.compact_at = 0;
    }

    pub(super) fn needs_compaction(&self) -> bool {
        !self.abandoned && !self.compacting && self.length > self.compact_at
    }

    pub(super) fn poll_failure(&mut self) -> Result<()> {
        while self.compactions.try_recv().is_ok() {
            self.compacting = false;
        }
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
    #[cfg(test)]
    pub(super) fn replace(&mut self, path: PathBuf, records: &[u8]) -> Result<()> {
        let (sender, receiver) = mpsc::sync_channel(0);
        self.sender
            .send(JournalCommand::Replace(path, records.to_vec(), sender))
            .context("pane journal writer stopped")?;
        receiver
            .recv()
            .context("pane journal writer stopped")?
            .context("replace pane journal")?;
        self.length = records.len() as u64;
        self.compact_at = MAX_JOURNAL_BYTES.max(self.length.saturating_mul(2));
        Ok(())
    }

    /// Queues a compacted replacement without waiting for its fsyncs. Writes
    /// queued after it remain ordered behind the replacement.
    pub(super) fn replace_async(&mut self, path: PathBuf, records: Vec<u8>) -> Result<()> {
        if let Some(error) = self.worker_failure() {
            self.abandoned = true;
            return Err(anyhow::anyhow!(error));
        }
        let length = records.len() as u64;
        self.sender
            .send(JournalCommand::ReplaceAsync(
                path,
                records,
                self.compaction_sender.clone(),
            ))
            .context("pane journal writer stopped")?;
        self.compacting = true;
        self.length = length;
        self.compact_at = MAX_JOURNAL_BYTES.max(length.saturating_mul(2));
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
    let mut buffered = false;
    let mut unsynced = false;
    let mut flush_at = None;
    let mut sync_at = None;
    loop {
        let now = Instant::now();
        if flush_at.is_some_and(|deadline| now >= deadline) {
            let _ = flush_journal(&mut file, &mut buffered, &mut failure, &failures);
            flush_at = None;
        }
        if sync_at.is_some_and(|deadline| now >= deadline) {
            let _ = sync_journal(
                &mut file,
                &mut buffered,
                &mut unsynced,
                &mut failure,
                &failures,
            );
            sync_at = None;
        }
        let deadline = match (flush_at, sync_at) {
            (Some(flush), Some(sync)) => Some(flush.min(sync)),
            (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
            (None, None) => None,
        };
        let command = if let Some(deadline) = deadline {
            receiver.recv_timeout(deadline.saturating_duration_since(now))
        } else {
            receiver
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        };
        match command {
            Ok(JournalCommand::Write(record, _permit)) => {
                if failure.is_none() {
                    match file.write_all(&record) {
                        Ok(()) => {
                            buffered = true;
                            unsynced = true;
                            flush_at.get_or_insert_with(|| Instant::now() + JOURNAL_FLUSH_DELAY);
                            sync_at.get_or_insert_with(|| Instant::now() + JOURNAL_SYNC_INTERVAL);
                        }
                        Err(error) => record_failure(&mut failure, &failures, error),
                    }
                }
            }
            Ok(JournalCommand::Flush(reply)) => {
                let result = sync_journal(
                    &mut file,
                    &mut buffered,
                    &mut unsynced,
                    &mut failure,
                    &failures,
                );
                flush_at = None;
                sync_at = None;
                let _ = reply.send(result);
            }
            #[cfg(test)]
            Ok(JournalCommand::Replace(path, records, reply)) => {
                // Drain earlier writes before replacing the inode. The old
                // journal remains intact until the complete new one is synced.
                let result = sync_journal(
                    &mut file,
                    &mut buffered,
                    &mut unsynced,
                    &mut failure,
                    &failures,
                )
                .and_then(|()| replace_journal_file(&path, &records))
                .map(|new_file| {
                    file = BufWriter::with_capacity(64 * 1024, new_file);
                });
                if let Err(error) = &result {
                    record_failure(
                        &mut failure,
                        &failures,
                        std::io::Error::new(error.kind(), error.to_string()),
                    );
                }
                flush_at = None;
                sync_at = None;
                let _ = reply.send(result);
            }
            Ok(JournalCommand::ReplaceAsync(path, records, completion)) => {
                let result = sync_journal(
                    &mut file,
                    &mut buffered,
                    &mut unsynced,
                    &mut failure,
                    &failures,
                )
                .and_then(|()| replace_journal_file(&path, &records))
                .map(|new_file| file = BufWriter::with_capacity(64 * 1024, new_file));
                if let Err(error) = result {
                    record_failure(&mut failure, &failures, error);
                }
                flush_at = None;
                sync_at = None;
                let _ = completion.send(());
            }
            Ok(JournalCommand::Truncate(length, reply)) => {
                let result = sync_journal(
                    &mut file,
                    &mut buffered,
                    &mut unsynced,
                    &mut failure,
                    &failures,
                )
                .and_then(|()| file.get_ref().set_len(length));
                flush_at = None;
                sync_at = None;
                let _ = reply.send(result);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = sync_journal(
                    &mut file,
                    &mut buffered,
                    &mut unsynced,
                    &mut failure,
                    &failures,
                );
                return;
            }
        }
    }
}

fn replace_journal_file(path: &Path, records: &[u8]) -> std::io::Result<File> {
    let temporary = path.with_extension("ansi.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(records)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(file)
}

fn flush_journal(
    file: &mut BufWriter<File>,
    buffered: &mut bool,
    failure: &mut Option<String>,
    failures: &Sender<String>,
) -> std::io::Result<()> {
    if let Some(error) = failure.as_ref() {
        return Err(std::io::Error::other(error.clone()));
    }
    if !*buffered {
        return Ok(());
    }
    match file.flush() {
        Ok(()) => {
            *buffered = false;
            Ok(())
        }
        Err(error) => {
            let returned = std::io::Error::new(error.kind(), error.to_string());
            record_failure(failure, failures, error);
            Err(returned)
        }
    }
}

fn sync_journal(
    file: &mut BufWriter<File>,
    buffered: &mut bool,
    unsynced: &mut bool,
    failure: &mut Option<String>,
    failures: &Sender<String>,
) -> std::io::Result<()> {
    flush_journal(file, buffered, failure, failures)?;
    if !*unsynced {
        return Ok(());
    }
    match file.get_ref().sync_data() {
        Ok(()) => {
            *unsynced = false;
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

/// Builds a journal that restores the current screen and scrollback.
///
/// The scrollback goes in packed, as the rows it already is; only the visible
/// screen is written as terminal output, because that is the part a parser has
/// to work through to put the cursor and its attributes back.
pub(super) fn compacted_journal_records(screen: &mut vt100::Screen) -> Result<Vec<u8>> {
    let (rows, cols) = screen.size();
    screen.set_scrollback(0);
    let history = screen.encode_history();
    let contents = screen.contents_formatted();
    let mut records = encode_journal_record(JOURNAL_RESIZE, &resize_payload(rows, cols))?;
    records.extend_from_slice(&encode_journal_record(JOURNAL_HISTORY, &history)?);
    records.extend_from_slice(&encode_journal_record(JOURNAL_OUTPUT, &contents)?);
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
            JOURNAL_HISTORY => {
                if !parser.screen_mut().restore_history(&payload) {
                    bail!("pane journal contains an unreadable scrollback record");
                }
            }
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

#[cfg(test)]
mod writer_tests {
    use super::*;

    #[test]
    fn asynchronous_compaction_stays_ordered_with_later_output() {
        let directory = std::env::temp_dir().join(format!(
            "mux-async-journal-{}-{:?}",
            std::process::id(),
            thread::current().id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("pane.ansi");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        let mut journal = PaneJournal::new(file, 0);
        journal.append_output(b"old", None).unwrap();
        let replacement = encode_journal_record(JOURNAL_OUTPUT, b"replacement").unwrap();
        journal
            .replace_async(path.clone(), replacement.clone())
            .unwrap();
        journal.append_output(b"tail", None).unwrap();
        journal.flush().unwrap();

        let mut expected = replacement;
        expected.extend(encode_journal_record(JOURNAL_OUTPUT, b"tail").unwrap());
        assert_eq!(fs::read(&path).unwrap(), expected);
        drop(journal);
        let _ = fs::remove_dir_all(directory);
    }
}
