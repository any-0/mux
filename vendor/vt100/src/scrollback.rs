use std::{
    collections::VecDeque,
    fs::File,
    iter::FromIterator,
    os::unix::fs::FileExt,
    sync::{
        Arc, OnceLock, RwLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Sender, SyncSender},
    },
    thread,
};

const BLOCK_ROWS: usize = 128;

#[derive(Debug)]
pub(crate) struct Scrollback {
    blocks: VecDeque<Block>,
    tail: Vec<crate::row::Row>,
    len: usize,
    backing: Option<SpillWriter>,
}

#[derive(Debug)]
struct Block {
    storage: StoredBytes,
    uncompressed_len: usize,
    rows: usize,
    first: usize,
    decoded: OnceLock<Vec<crate::row::Row>>,
}

#[derive(Clone, Debug)]
struct StoredBytes {
    compressed: bool,
    backing: Arc<RwLock<Backing>>,
}

#[derive(Debug)]
enum Backing {
    Heap(Box<[u8]>),
    Spilling(Arc<[u8]>),
    File {
        file: Arc<File>,
        offset: u64,
        len: usize,
    },
}

#[derive(Clone, Debug)]
struct SpillWriter {
    sender: Sender<SpillCommand>,
    next_offset: Arc<AtomicU64>,
    file: Arc<File>,
}

#[derive(Debug)]
struct SpillJob {
    backing: Arc<RwLock<Backing>>,
    bytes: Arc<[u8]>,
    file: Arc<File>,
    offset: u64,
}

#[derive(Debug)]
enum SpillCommand {
    Write(SpillJob),
    Flush(SyncSender<()>),
}

impl Default for Scrollback {
    fn default() -> Self {
        Self {
            blocks: VecDeque::new(),
            tail: Vec::new(),
            len: 0,
            backing: None,
        }
    }
}

impl Clone for Scrollback {
    fn clone(&self) -> Self {
        Self {
            blocks: self.blocks.clone(),
            tail: self.tail.clone(),
            len: self.len,
            backing: self.backing.clone(),
        }
    }
}

impl Clone for Block {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            uncompressed_len: self.uncompressed_len,
            rows: self.rows,
            first: self.first,
            decoded: OnceLock::new(),
        }
    }
}

impl Scrollback {
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn push_back(&mut self, mut row: crate::row::Row) {
        row.compact();
        self.tail.push(row);
        self.len += 1;
        if self.tail.len() == BLOCK_ROWS {
            let block = Block::new(std::mem::take(&mut self.tail));
            if let Some(backing) = &self.backing {
                backing.spill(&block.storage);
            }
            self.blocks.push_back(block);
            self.tail = Vec::with_capacity(BLOCK_ROWS);
        }
    }

    pub(crate) fn set_backing(&mut self, file: File) {
        let backing = SpillWriter::new(file);
        for block in &self.blocks {
            backing.spill(&block.storage);
        }
        self.backing = Some(backing);
    }

    pub(crate) fn flush_backing(&self) {
        if let Some(backing) = &self.backing {
            backing.flush();
        }
    }

    pub(crate) fn pop_front(&mut self) {
        if let Some(block) = self.blocks.front_mut() {
            block.first += 1;
            self.len -= 1;
            if block.first == block.rows {
                self.blocks.pop_front();
            }
        } else if !self.tail.is_empty() {
            self.tail.remove(0);
            self.len -= 1;
        }
    }

    pub(crate) fn iter(&self) -> Iter<'_> {
        Iter {
            scrollback: self,
            index: 0,
        }
    }

    pub(crate) fn clear_decoded(&mut self) {
        for block in &mut self.blocks {
            block.decoded.take();
        }
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.blocks.capacity() * std::mem::size_of::<Block>()
            + self.blocks.iter().map(Block::heap_bytes).sum::<usize>()
            + self.tail.capacity() * std::mem::size_of::<crate::row::Row>()
            + self.tail.iter().map(crate::row::Row::heap_bytes).sum::<usize>()
    }

    pub(crate) fn into_rows(self) -> Vec<crate::row::Row> {
        let mut rows = Vec::with_capacity(self.len);
        for block in self.blocks {
            let first = block.first;
            rows.extend(block.into_rows().into_iter().skip(first));
        }
        rows.extend(self.tail);
        rows
    }

    pub(crate) fn get(&self, mut index: usize) -> Option<&crate::row::Row> {
        if index >= self.len {
            return None;
        }
        let Some(first) = self.blocks.front() else {
            return self.tail.get(index);
        };
        let first_rows = first.rows - first.first;
        if index < first_rows {
            return Some(&first.rows()[first.first + index]);
        }
        index -= first_rows;
        let block_index = 1 + index / BLOCK_ROWS;
        if let Some(block) = self.blocks.get(block_index) {
            return Some(&block.rows()[index % BLOCK_ROWS]);
        }
        self.tail
            .get(index - (self.blocks.len() - 1) * BLOCK_ROWS)
    }
}

impl FromIterator<crate::row::Row> for Scrollback {
    fn from_iter<T: IntoIterator<Item = crate::row::Row>>(iter: T) -> Self {
        let mut scrollback = Self::default();
        for row in iter {
            scrollback.push_back(row);
        }
        scrollback
    }
}

pub(crate) struct Iter<'a> {
    scrollback: &'a Scrollback,
    index: usize,
}

impl<'a> Iterator for Iter<'a> {
    type Item = &'a crate::row::Row;

    fn next(&mut self) -> Option<Self::Item> {
        let row = self.scrollback.get(self.index);
        self.index += usize::from(row.is_some());
        row
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        self.index = self.index.saturating_add(n).min(self.scrollback.len);
        self.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.scrollback.len - self.index;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for Iter<'_> {}

impl Block {
    fn new(rows: Vec<crate::row::Row>) -> Self {
        let mut raw = Vec::new();
        for row in &rows {
            row.encode(&mut raw);
        }
        let uncompressed_len = raw.len();
        let compressed = zstd::bulk::compress(&raw, 1).expect("compress internal scrollback block");
        let (bytes, compressed) = if compressed.len() < raw.len() {
            (compressed.into_boxed_slice(), true)
        } else {
            (raw.into_boxed_slice(), false)
        };
        Self {
            storage: StoredBytes {
                compressed,
                backing: Arc::new(RwLock::new(Backing::Heap(bytes))),
            },
            uncompressed_len,
            rows: rows.len(),
            first: 0,
            decoded: OnceLock::new(),
        }
    }

    fn rows(&self) -> &[crate::row::Row] {
        self.decoded.get_or_init(|| self.decode())
    }

    fn into_rows(self) -> Vec<crate::row::Row> {
        if self.decoded.get().is_some() {
            self.decoded.into_inner().unwrap()
        } else {
            self.decode()
        }
    }

    fn decode(&self) -> Vec<crate::row::Row> {
        let bytes = self.storage.read();
        let raw = if self.storage.compressed {
            zstd::bulk::decompress(&bytes, self.uncompressed_len)
                .expect("decompress internal scrollback block")
        } else {
            bytes
        };
        let mut input = raw.as_slice();
        let rows = (0..self.rows)
            .map(|_| crate::row::Row::decode(&mut input))
            .collect();
        assert!(input.is_empty());
        rows
    }

    fn heap_bytes(&self) -> usize {
        self.storage.heap_bytes()
            + self.decoded.get().map_or(0, |rows| {
                rows.capacity() * std::mem::size_of::<crate::row::Row>()
                    + rows.iter().map(crate::row::Row::heap_bytes).sum::<usize>()
            })
    }
}

impl StoredBytes {
    fn read(&self) -> Vec<u8> {
        match &*self.backing.read().expect("lock scrollback storage") {
            Backing::Heap(bytes) => bytes.to_vec(),
            Backing::Spilling(bytes) => bytes.to_vec(),
            Backing::File { file, offset, len } => {
                let mut bytes = vec![0; *len];
                let mut read = 0;
                while read < bytes.len() {
                    let count = file
                        .read_at(&mut bytes[read..], *offset + read as u64)
                        .expect("read internal scrollback block");
                    assert!(count > 0, "internal scrollback file ended early");
                    read += count;
                }
                bytes
            }
        }
    }

    fn heap_bytes(&self) -> usize {
        match &*self.backing.read().expect("lock scrollback storage") {
            Backing::Heap(bytes) => bytes.len(),
            Backing::Spilling(bytes) => bytes.len(),
            Backing::File { .. } => 0,
        }
    }
}

impl SpillWriter {
    fn new(file: File) -> Self {
        let file = Arc::new(file);
        Self {
            sender: spill_sender().clone(),
            next_offset: Arc::new(AtomicU64::new(0)),
            file,
        }
    }

    fn flush(&self) {
        let (sender, receiver) = mpsc::sync_channel(0);
        self.sender
            .send(SpillCommand::Flush(sender))
            .expect("scrollback spill writer stopped");
        receiver.recv().expect("scrollback spill writer stopped");
    }

    fn spill(&self, storage: &StoredBytes) {
        let bytes = {
            let mut backing = storage.backing.write().expect("lock scrollback storage");
            let Backing::Heap(bytes) = std::mem::replace(
                &mut *backing,
                Backing::Heap(Box::new([])),
            ) else {
                return;
            };
            let bytes: Arc<[u8]> = bytes.into();
            *backing = Backing::Spilling(bytes.clone());
            bytes
        };
        let offset = self
            .next_offset
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        let _ = self.sender.send(SpillCommand::Write(SpillJob {
            backing: storage.backing.clone(),
            bytes,
            file: self.file.clone(),
            offset,
        }));
    }
}

fn spill_sender() -> &'static Sender<SpillCommand> {
    static SENDER: OnceLock<Sender<SpillCommand>> = OnceLock::new();
    SENDER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(command) = receiver.recv() {
                let job = match command {
                    SpillCommand::Write(job) => job,
                    SpillCommand::Flush(reply) => {
                        let _ = reply.send(());
                        continue;
                    }
                };
                let mut written = 0;
                let mut failed = false;
                while written < job.bytes.len() {
                    match job
                        .file
                        .write_at(&job.bytes[written..], job.offset + written as u64)
                    {
                        Ok(0) | Err(_) => {
                            failed = true;
                            break;
                        }
                        Ok(count) => written += count,
                    }
                }
                if !failed {
                    *job.backing.write().expect("lock scrollback storage") = Backing::File {
                        file: job.file,
                        offset: job.offset,
                        len: job.bytes.len(),
                    };
                }
            }
        });
        sender
    })
}
