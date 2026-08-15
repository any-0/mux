use std::{
    collections::VecDeque,
    iter::FromIterator,
    sync::OnceLock,
};

const BLOCK_ROWS: usize = 256;

#[derive(Debug)]
pub(crate) struct Scrollback {
    blocks: VecDeque<Block>,
    tail: Vec<crate::row::Row>,
    len: usize,
}

#[derive(Debug)]
struct Block {
    storage: Storage,
    uncompressed_len: usize,
    rows: usize,
    first: usize,
    decoded: OnceLock<Vec<crate::row::Row>>,
}

#[derive(Clone, Debug)]
enum Storage {
    Raw(Box<[u8]>),
    Zstd(Box<[u8]>),
}

impl Default for Scrollback {
    fn default() -> Self {
        Self {
            blocks: VecDeque::new(),
            tail: Vec::new(),
            len: 0,
        }
    }
}

impl Clone for Scrollback {
    fn clone(&self) -> Self {
        Self {
            blocks: self.blocks.clone(),
            tail: self.tail.clone(),
            len: self.len,
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
            self.blocks.push_back(Block::new(std::mem::take(&mut self.tail)));
            self.tail = Vec::with_capacity(BLOCK_ROWS);
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
        let storage = if compressed.len() < raw.len() {
            Storage::Zstd(compressed.into_boxed_slice())
        } else {
            Storage::Raw(raw.into_boxed_slice())
        };
        Self {
            storage,
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
        let raw = match &self.storage {
            Storage::Raw(raw) => raw.to_vec(),
            Storage::Zstd(compressed) => zstd::bulk::decompress(compressed, self.uncompressed_len)
                .expect("decompress internal scrollback block"),
        };
        let mut input = raw.as_slice();
        let rows = (0..self.rows)
            .map(|_| crate::row::Row::decode(&mut input))
            .collect();
        assert!(input.is_empty());
        rows
    }

    fn heap_bytes(&self) -> usize {
        let storage = match &self.storage {
            Storage::Raw(bytes) | Storage::Zstd(bytes) => bytes.len(),
        };
        storage
            + self.decoded.get().map_or(0, |rows| {
                rows.capacity() * std::mem::size_of::<crate::row::Row>()
                    + rows.iter().map(crate::row::Row::heap_bytes).sum::<usize>()
            })
    }
}
