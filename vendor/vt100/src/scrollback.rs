use std::{
    collections::VecDeque,
    fs::File,
    iter::FromIterator,
    os::unix::fs::FileExt,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, SyncSender, TrySendError},
        Arc, Mutex, OnceLock, RwLock,
    },
    thread,
};

const BLOCK_ROWS: usize = 128;
const SPILL_QUEUE_JOBS: usize = 64;
const SPILL_QUEUE_BYTES: usize = 8 * 1024 * 1024;

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
    Heap(Arc<[u8]>),
    Spilling(Arc<[u8]>),
    File { allocation: Arc<FileAllocation> },
}

#[derive(Clone, Debug)]
struct SpillWriter {
    sender: SyncSender<SpillCommand>,
    allocator: Arc<FileAllocator>,
    pending_bytes: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct SpillJob {
    backing: Arc<RwLock<Backing>>,
    bytes: Arc<[u8]>,
    allocation: Arc<FileAllocation>,
    pending_bytes: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct FileAllocator {
    file: Arc<File>,
    state: Mutex<AllocatorState>,
}

#[derive(Debug, Default)]
struct AllocatorState {
    end: u64,
    free: Vec<(u64, usize)>,
}

#[derive(Debug)]
struct FileAllocation {
    allocator: Arc<FileAllocator>,
    offset: u64,
    len: usize,
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
                backing: Arc::new(RwLock::new(Backing::Heap(bytes.into()))),
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
            Backing::File { allocation } => {
                let mut bytes = vec![0; allocation.len];
                let mut read = 0;
                while read < bytes.len() {
                    let count = allocation
                        .allocator
                        .file
                        .read_at(&mut bytes[read..], allocation.offset + read as u64)
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
        Self {
            sender: spill_sender().clone(),
            allocator: Arc::new(FileAllocator {
                file: Arc::new(file),
                state: Mutex::new(AllocatorState::default()),
            }),
            pending_bytes: spill_pending().clone(),
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
        let len = {
            let backing = storage.backing.read().expect("lock scrollback storage");
            let Backing::Heap(bytes) = &*backing else {
                return;
            };
            bytes.len()
        };
        if !reserve_pending(&self.pending_bytes, len) {
            return;
        }
        let allocation = self.allocator.allocate(len);
        let bytes = {
            let mut backing = storage.backing.write().expect("lock scrollback storage");
            let Backing::Heap(bytes) = &*backing else {
                self.pending_bytes.fetch_sub(len, Ordering::Relaxed);
                return;
            };
            let bytes = bytes.clone();
            *backing = Backing::Spilling(bytes.clone());
            bytes
        };
        let job = SpillJob {
            backing: storage.backing.clone(),
            bytes: bytes.clone(),
            allocation,
            pending_bytes: self.pending_bytes.clone(),
        };
        if let Err(TrySendError::Full(SpillCommand::Write(_))
            | TrySendError::Disconnected(SpillCommand::Write(_))) =
            self.sender.try_send(SpillCommand::Write(job))
        {
            *storage.backing.write().expect("lock scrollback storage") = Backing::Heap(bytes);
            self.pending_bytes.fetch_sub(len, Ordering::Relaxed);
        }
    }
}

fn reserve_pending(pending: &AtomicUsize, len: usize) -> bool {
    let mut current = pending.load(Ordering::Relaxed);
    loop {
        let Some(next) = current
            .checked_add(len)
            .filter(|next| *next <= SPILL_QUEUE_BYTES)
        else {
            return false;
        };
        match pending.compare_exchange_weak(
            current,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

fn spill_pending() -> &'static Arc<AtomicUsize> {
    static PENDING: OnceLock<Arc<AtomicUsize>> = OnceLock::new();
    PENDING.get_or_init(|| Arc::new(AtomicUsize::new(0)))
}

impl FileAllocator {
    fn allocate(self: &Arc<Self>, len: usize) -> Arc<FileAllocation> {
        let mut state = self.state.lock().expect("lock scrollback allocator");
        let offset = if let Some(index) = state
            .free
            .iter()
            .position(|(_, free_len)| *free_len >= len)
        {
            let (offset, free_len) = state.free[index];
            if free_len == len {
                state.free.remove(index);
            } else {
                state.free[index] = (offset + len as u64, free_len - len);
            }
            offset
        } else {
            let offset = state.end;
            state.end += len as u64;
            offset
        };
        Arc::new(FileAllocation {
            allocator: self.clone(),
            offset,
            len,
        })
    }

    fn free(&self, offset: u64, len: usize) {
        let mut state = self.state.lock().expect("lock scrollback allocator");
        state.free.push((offset, len));
        state.free.sort_unstable_by_key(|extent| extent.0);
        let mut merged: Vec<(u64, usize)> = Vec::with_capacity(state.free.len());
        for (offset, len) in state.free.drain(..) {
            match merged.last_mut() {
                Some((previous_offset, previous_len))
                    if *previous_offset + *previous_len as u64 == offset =>
                {
                    *previous_len += len;
                }
                _ => merged.push((offset, len)),
            }
        }
        state.free = merged;
    }
}

impl Drop for FileAllocation {
    fn drop(&mut self) {
        self.allocator.free(self.offset, self.len);
    }
}

fn spill_sender() -> &'static SyncSender<SpillCommand> {
    static SENDER: OnceLock<SyncSender<SpillCommand>> = OnceLock::new();
    SENDER.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel(SPILL_QUEUE_JOBS);
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
                    match job.allocation.allocator.file.write_at(
                        &job.bytes[written..],
                        job.allocation.offset + written as u64,
                    )
                    {
                        Ok(0) | Err(_) => {
                            failed = true;
                            break;
                        }
                        Ok(count) => written += count,
                    }
                }
                job.pending_bytes.fetch_sub(job.bytes.len(), Ordering::Relaxed);
                if !failed {
                    *job.backing.write().expect("lock scrollback storage") = Backing::File {
                        allocation: job.allocation,
                    };
                } else {
                    *job.backing.write().expect("lock scrollback storage") =
                        Backing::Heap(job.bytes.clone());
                }
            }
        });
        sender
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;

    fn backing_file() -> File {
        static NEXT_FILE: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "vt100-scrollback-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(path).unwrap();
        file
    }

    fn allocator() -> Arc<FileAllocator> {
        Arc::new(FileAllocator {
            file: Arc::new(backing_file()),
            state: Mutex::new(AllocatorState::default()),
        })
    }

    #[test]
    fn file_extents_are_reused_after_the_last_snapshot_releases_them() {
        let allocator = allocator();
        let first = allocator.allocate(100);
        let snapshot = first.clone();
        drop(first);

        let second = allocator.allocate(100);
        assert_eq!(second.offset, 100);

        drop(snapshot);
        let reused = allocator.allocate(50);
        assert_eq!(reused.offset, 0);

        drop(reused);
        drop(second);
        let merged = allocator.allocate(200);
        assert_eq!(merged.offset, 0);
        assert_eq!(allocator.state.lock().unwrap().end, 200);
    }

    #[test]
    fn spill_byte_budget_is_bounded_and_nonblocking() {
        let pending = AtomicUsize::new(0);
        assert!(reserve_pending(&pending, SPILL_QUEUE_BYTES));
        assert!(!reserve_pending(&pending, 1));
        assert_eq!(pending.load(Ordering::Relaxed), SPILL_QUEUE_BYTES);
    }

    #[test]
    fn backing_file_plateaus_while_old_blocks_expire() {
        let file = backing_file();
        let observer = file.try_clone().unwrap();
        let mut scrollback = Scrollback::default();
        scrollback.set_backing(file);

        for _ in 0..BLOCK_ROWS {
            scrollback.push_back(crate::row::Row::new(80));
        }
        scrollback.flush_backing();
        let snapshot = scrollback.clone();

        for _ in 0..BLOCK_ROWS {
            scrollback.pop_front();
            scrollback.push_back(crate::row::Row::new(80));
        }
        scrollback.flush_backing();
        let plateau = observer.metadata().unwrap().len();
        assert!(plateau > 0);

        for _ in 0..20 {
            for _ in 0..BLOCK_ROWS {
                scrollback.pop_front();
                scrollback.push_back(crate::row::Row::new(80));
            }
            scrollback.flush_backing();
            assert_eq!(observer.metadata().unwrap().len(), plateau);
        }

        assert_eq!(snapshot.len(), BLOCK_ROWS);
        assert_eq!(snapshot.get(0).unwrap().cells().count(), 80);
    }
}
