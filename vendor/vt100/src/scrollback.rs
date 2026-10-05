use std::{
    collections::VecDeque,
    fs::File,
    os::unix::fs::FileExt,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex, OnceLock, RwLock,
    },
    thread,
};

const BLOCK_ROWS: usize = 128;
const SPILL_QUEUE_JOBS: usize = 64;
const SPILL_QUEUE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub struct Scrollback {
    blocks: VecDeque<Block>,
    tail: Vec<crate::row::Row>,
    len: usize,
    backing: Option<Arc<FileAllocator>>,
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
    /// Queued for the writer; still readable from memory meanwhile.
    Spilling(Arc<[u8]>),
    File(Arc<FileAllocation>),
}

#[derive(Debug)]
struct SpillJob {
    backing: Arc<RwLock<Backing>>,
    bytes: Arc<[u8]>,
    allocation: Arc<FileAllocation>,
}

#[derive(Debug)]
struct FileAllocator {
    file: File,
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

// A clone shares the stored bytes but decodes on its own.
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
            let block = Block::new(&std::mem::take(&mut self.tail));
            if let Some(backing) = &self.backing {
                backing.spill(&block.storage);
            }
            self.blocks.push_back(block);
            self.tail = Vec::with_capacity(BLOCK_ROWS);
        }
    }

    pub(crate) fn set_backing(&mut self, file: File) {
        let backing = Arc::new(FileAllocator {
            file,
            state: Mutex::new(AllocatorState::default()),
        });
        for block in &self.blocks {
            backing.spill(&block.storage);
        }
        self.backing = Some(backing);
    }

    pub(crate) fn flush_backing(&self) {
        if self.backing.is_some() {
            let (sender, receiver) = mpsc::sync_channel(0);
            spill_sender()
                .send(SpillCommand::Flush(sender))
                .expect("scrollback spill writer stopped");
            receiver.recv().expect("scrollback spill writer stopped");
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

    /// Drops every row, keeping the backing file for rows to come.
    pub(crate) fn clear(&mut self) {
        self.blocks.clear();
        self.tail.clear();
        self.len = 0;
    }

    /// Takes the newest row back out, as a terminal growing taller pulls
    /// history back onto the screen.
    pub(crate) fn pop_back(&mut self) -> Option<crate::row::Row> {
        if self.tail.is_empty() {
            let block = self.blocks.pop_back()?;
            let first = block.first;
            self.tail = block.into_rows().into_iter().skip(first).collect();
        }
        let row = self.tail.pop()?;
        self.len -= 1;
        Some(row)
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
            + self
                .tail
                .iter()
                .map(crate::row::Row::heap_bytes)
                .sum::<usize>()
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
        self.tail.get(index - (self.blocks.len() - 1) * BLOCK_ROWS)
    }
}

pub struct Iter<'a> {
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
    fn new(rows: &[crate::row::Row]) -> Self {
        let mut raw = Vec::new();
        for row in rows {
            row.encode(&mut raw);
        }
        let uncompressed_len = raw.len();
        let (bytes, compressed) = compress_if_smaller(raw);
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

    fn into_rows(mut self) -> Vec<crate::row::Row> {
        self.decoded.take().unwrap_or_else(|| self.decode())
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
            Backing::Heap(bytes) | Backing::Spilling(bytes) => bytes.to_vec(),
            Backing::File(allocation) => {
                let mut bytes = vec![0; allocation.len];
                allocation
                    .allocator
                    .file
                    .read_exact_at(&mut bytes, allocation.offset)
                    .expect("read internal scrollback block");
                bytes
            }
        }
    }

    fn heap_bytes(&self) -> usize {
        match &*self.backing.read().expect("lock scrollback storage") {
            Backing::Heap(bytes) | Backing::Spilling(bytes) => bytes.len(),
            Backing::File(_) => 0,
        }
    }
}

/// Compresses `raw` with zstd unless that would not make it smaller. Returns
/// the bytes to store and whether they are compressed.
pub fn compress_if_smaller(raw: Vec<u8>) -> (Vec<u8>, bool) {
    let compressed = zstd::bulk::compress(&raw, 1).expect("compress scrollback");
    if compressed.len() < raw.len() {
        (compressed, true)
    } else {
        (raw, false)
    }
}

/// Bytes queued for the spill writer across every scrollback.
static SPILL_PENDING: AtomicUsize = AtomicUsize::new(0);

fn reserve_pending(pending: &AtomicUsize, len: usize) -> bool {
    pending
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current
                .checked_add(len)
                .filter(|next| *next <= SPILL_QUEUE_BYTES)
        })
        .is_ok()
}

impl FileAllocator {
    /// Queues `storage` for the writer thread. When the queue is full the
    /// block simply stays in memory: terminal processing never waits.
    fn spill(self: &Arc<Self>, storage: &StoredBytes) {
        let len = match &*storage.backing.read().expect("lock scrollback storage") {
            Backing::Heap(bytes) => bytes.len(),
            _ => return,
        };
        if !reserve_pending(&SPILL_PENDING, len) {
            return;
        }
        let allocation = self.allocate(len);
        let bytes = {
            let mut backing = storage.backing.write().expect("lock scrollback storage");
            let Backing::Heap(bytes) = &*backing else {
                SPILL_PENDING.fetch_sub(len, Ordering::Relaxed);
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
        };
        if spill_sender().try_send(SpillCommand::Write(job)).is_err() {
            *storage.backing.write().expect("lock scrollback storage") = Backing::Heap(bytes);
            SPILL_PENDING.fetch_sub(len, Ordering::Relaxed);
        }
    }

    fn allocate(self: &Arc<Self>, len: usize) -> Arc<FileAllocation> {
        let mut state = self.state.lock().expect("lock scrollback allocator");
        let offset =
            if let Some(index) = state.free.iter().position(|(_, free_len)| *free_len >= len) {
                let (offset, free_len) = state.free[index];
                if free_len == len {
                    state.free.remove(index);
                } else {
                    state.free[index] = (offset + to_u64(len), free_len - len);
                }
                offset
            } else {
                let offset = state.end;
                state.end += to_u64(len);
                offset
            };
        drop(state);
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
                    if *previous_offset + to_u64(*previous_len) == offset =>
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

/// The one writer thread, shared by every scrollback.
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
                let written = job
                    .allocation
                    .allocator
                    .file
                    .write_all_at(&job.bytes, job.allocation.offset);
                SPILL_PENDING.fetch_sub(job.bytes.len(), Ordering::Relaxed);
                *job.backing.write().expect("lock scrollback storage") = if written.is_ok() {
                    Backing::File(job.allocation)
                } else {
                    Backing::Heap(job.bytes)
                };
            }
        });
        sender
    })
}

fn to_u64(len: usize) -> u64 {
    u64::try_from(len).expect("usize fits in u64")
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
            file: backing_file(),
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
