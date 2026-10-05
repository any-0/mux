//! A pane's append-only record of everything its terminal has shown.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, ErrorKind, Read, Write},
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
const JOURNAL_BUFFER: usize = 64 * 1024;

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
    Flush(SyncSender<io::Result<()>>),
    Replace(PathBuf, Vec<u8>, Sender<()>),
    Truncate(u64, SyncSender<io::Result<()>>),
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

    /// Reports the worker's first failure, abandoning the journal.
    fn check_worker(&mut self) -> Result<()> {
        if let Ok(error) = self.failures.try_recv() {
            self.abandoned = true;
            bail!(error);
        }
        Ok(())
    }

    fn abandon_on_error(&mut self, result: Result<()>) -> Result<()> {
        if result.is_err() {
            self.abandoned = true;
        }
        result
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

    /// The first failure is reported; after that the journal stays quiet, so
    /// a full disk does not repeat itself on every chunk of terminal output.
    pub(super) fn append_output(
        &mut self,
        bytes: &[u8],
        permit: Option<OutputPermit>,
    ) -> Result<()> {
        if self.abandoned {
            return Ok(());
        }
        self.check_worker()?;
        self.queue_record(JOURNAL_OUTPUT, bytes, permit)
    }

    pub(super) fn append_resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        if self.abandoned {
            return Ok(());
        }
        self.check_worker()?;
        let result = self.queue_record(JOURNAL_RESIZE, &resize_payload(rows, cols), None);
        self.abandon_on_error(result)
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
        self.abandon_on_error(result)
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
        self.check_worker()
    }

    /// Queues a replacement journal, `done` hearing when the worker is through
    /// with it. Writes queued after it remain ordered behind the replacement.
    fn queue_replacement(
        &mut self,
        path: PathBuf,
        records: Vec<u8>,
        done: Sender<()>,
    ) -> Result<()> {
        self.sender
            .send(JournalCommand::Replace(path, records, done))
            .context("pane journal writer stopped")
    }

    fn replaced(&mut self, length: u64) {
        self.length = length;
        self.compact_at = MAX_JOURNAL_BYTES.max(length.saturating_mul(2));
    }

    /// Replaces the journal with `records`, which must replay to the same
    /// screen the pane is showing now, and waits for the outcome.
    #[cfg(test)]
    pub(super) fn replace(&mut self, path: PathBuf, records: &[u8]) -> Result<()> {
        let (done, finished) = mpsc::channel();
        self.queue_replacement(path, records.to_vec(), done)?;
        finished.recv().context("pane journal writer stopped")?;
        if let Ok(error) = self.failures.try_recv() {
            bail!("replace pane journal: {error}");
        }
        self.replaced(records.len() as u64);
        Ok(())
    }

    /// Queues a compacted replacement without waiting for its fsyncs.
    pub(super) fn replace_async(&mut self, path: PathBuf, records: Vec<u8>) -> Result<()> {
        self.check_worker()?;
        let length = records.len() as u64;
        self.queue_replacement(path, records, self.compaction_sender.clone())?;
        self.compacting = true;
        self.replaced(length);
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
}

/// The worker's side of a journal. After the first failure every write is
/// dropped, and the failure is reported through `failures`.
struct JournalWriter {
    file: BufWriter<File>,
    failure: Option<String>,
    failures: Sender<String>,
    buffered: bool,
    unsynced: bool,
    flush_at: Option<Instant>,
    sync_at: Option<Instant>,
}

impl JournalWriter {
    /// Records `error` as the journal's failure, returning a copy of it.
    fn fail(&mut self, error: io::Error) -> io::Error {
        let message = error.to_string();
        self.failure = Some(message.clone());
        let _ = self.failures.send(message.clone());
        io::Error::new(error.kind(), message)
    }

    fn write(&mut self, record: &[u8]) {
        if self.failure.is_some() {
            return;
        }
        match self.file.write_all(record) {
            Ok(()) => {
                self.buffered = true;
                self.unsynced = true;
                self.flush_at
                    .get_or_insert_with(|| Instant::now() + JOURNAL_FLUSH_DELAY);
                self.sync_at
                    .get_or_insert_with(|| Instant::now() + JOURNAL_SYNC_INTERVAL);
            }
            Err(error) => {
                self.fail(error);
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(error) = &self.failure {
            return Err(io::Error::other(error.clone()));
        }
        if self.buffered {
            self.file.flush().map_err(|error| self.fail(error))?;
            self.buffered = false;
        }
        Ok(())
    }

    fn sync(&mut self) -> io::Result<()> {
        self.flush()?;
        if self.unsynced {
            self.file
                .get_ref()
                .sync_data()
                .map_err(|error| self.fail(error))?;
            self.unsynced = false;
        }
        Ok(())
    }

    /// Syncs ahead of a command that needs everything so far on disk, which
    /// also settles both pending deadlines.
    fn sync_now(&mut self) -> io::Result<()> {
        let result = self.sync();
        self.flush_at = None;
        self.sync_at = None;
        result
    }
}

fn journal_writer(file: File, receiver: Receiver<JournalCommand>, failures: Sender<String>) {
    let mut writer = JournalWriter {
        file: BufWriter::with_capacity(JOURNAL_BUFFER, file),
        failure: None,
        failures,
        buffered: false,
        unsynced: false,
        flush_at: None,
        sync_at: None,
    };
    loop {
        let now = Instant::now();
        if writer.flush_at.is_some_and(|deadline| now >= deadline) {
            let _ = writer.flush();
            writer.flush_at = None;
        }
        if writer.sync_at.is_some_and(|deadline| now >= deadline) {
            let _ = writer.sync();
            writer.sync_at = None;
        }
        let deadline = [writer.flush_at, writer.sync_at]
            .into_iter()
            .flatten()
            .min();
        let command = match deadline {
            Some(deadline) => receiver.recv_timeout(deadline.saturating_duration_since(now)),
            None => receiver
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match command {
            Ok(JournalCommand::Write(record, _permit)) => writer.write(&record),
            Ok(JournalCommand::Flush(reply)) => {
                let _ = reply.send(writer.sync_now());
            }
            Ok(JournalCommand::Replace(path, records, done)) => {
                // Drain earlier writes before replacing the inode. The old
                // journal remains intact until the complete new one is synced.
                match writer
                    .sync_now()
                    .and_then(|()| replace_journal_file(&path, &records))
                {
                    Ok(file) => writer.file = BufWriter::with_capacity(JOURNAL_BUFFER, file),
                    Err(error) => {
                        writer.fail(error);
                    }
                }
                let _ = done.send(());
            }
            Ok(JournalCommand::Truncate(length, reply)) => {
                let result = writer
                    .sync_now()
                    .and_then(|()| writer.file.get_ref().set_len(length));
                let _ = reply.send(result);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = writer.sync();
                return;
            }
        }
    }
}

fn replace_journal_file(path: &Path, records: &[u8]) -> io::Result<File> {
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

/// Replays records into `parser`, returning the length of the readable prefix:
/// a torn final record is left for the caller to cut off.
pub(super) fn replay_pane_journal<CB: vt100::Callbacks>(
    parser: &mut vt100::Parser<CB>,
    reader: impl Read,
) -> Result<u64> {
    let mut reader = BufReader::with_capacity(JOURNAL_BUFFER, reader);
    let mut offset = 0u64;
    loop {
        let mut header = [0; 5];
        let mut payload = Vec::new();
        let read = reader.read_exact(&mut header).and_then(|()| {
            let length = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
            if length > MAX_JOURNAL_RECORD {
                return Err(io::Error::other("pane journal record exceeds 16 MiB"));
            }
            payload.resize(length, 0);
            reader.read_exact(&mut payload)
        });
        match read {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => return Ok(offset),
            Err(error) => return Err(error.into()),
        }
        match header[0] {
            JOURNAL_OUTPUT => process_terminal_bytes(parser, &payload),
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
            kind => bail!("pane journal contains unknown record type {kind}"),
        }
        offset += 5 + payload.len() as u64;
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
