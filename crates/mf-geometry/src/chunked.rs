//! Chunked, reference-counted storage with copy-on-write per chunk
//! (MF-DOC-002, ADR-0001). Revisions share every chunk they didn't touch.

use std::collections::HashSet;
use std::sync::Arc;

/// Elements per chunk.
pub const CHUNK: usize = 32_768;

#[allow(clippy::len_without_is_empty)] // crate-internal
#[derive(Clone, Default)]
pub struct Chunked<T> {
    chunks: Vec<Arc<Vec<T>>>,
    len: usize,
}

impl<T: Copy> Chunked<T> {
    pub fn from_vec(v: Vec<T>) -> Self {
        let len = v.len();
        let chunks = v.chunks(CHUNK).map(|c| Arc::new(c.to_vec())).collect();
        Chunked { chunks, len }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn get(&self, i: usize) -> T {
        self.chunks[i / CHUNK][i % CHUNK]
    }

    pub fn iter(&self) -> impl Iterator<Item = T> + '_ {
        self.chunks.iter().flat_map(|c| c.iter().copied())
    }

    /// Write one element, copying its chunk first if another revision shares it.
    pub fn set(&mut self, i: usize, value: T) {
        Arc::make_mut(&mut self.chunks[i / CHUNK])[i % CHUNK] = value;
    }

    pub fn push(&mut self, value: T) {
        if self.len.is_multiple_of(CHUNK) {
            self.chunks.push(Arc::new(Vec::new()));
        }
        if let Some(last) = self.chunks.last_mut() {
            Arc::make_mut(last).push(value);
        }
        self.len += 1;
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn shares_chunk(&self, other: &Chunked<T>, i: usize) -> bool {
        match (self.chunks.get(i), other.chunks.get(i)) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Heap bytes of chunks not already in `seen` (chunk identity by address).
    pub fn unique_heap_bytes(&self, seen: &mut HashSet<usize>) -> u64 {
        let own = self.chunks.capacity() * std::mem::size_of::<Arc<Vec<T>>>();
        let chunks: usize = self
            .chunks
            .iter()
            .filter(|c| seen.insert(Arc::as_ptr(c) as usize))
            .map(|c| c.capacity() * std::mem::size_of::<T>() + 2 * std::mem::size_of::<usize>())
            .sum();
        (own + chunks) as u64
    }
}

/// A growable bitset on chunked storage (liveness of slots).
#[derive(Clone, Default)]
pub struct Bits {
    words: Chunked<u64>,
    len: usize,
}

impl Bits {
    pub fn all_set(len: usize) -> Bits {
        let mut words = vec![u64::MAX; len.div_ceil(64)];
        if !len.is_multiple_of(64)
            && let Some(last) = words.last_mut()
        {
            *last = (1u64 << (len % 64)) - 1;
        }
        Bits {
            words: Chunked::from_vec(words),
            len,
        }
    }

    pub fn get(&self, i: usize) -> bool {
        i < self.len && (self.words.get(i / 64) >> (i % 64)) & 1 == 1
    }

    pub fn set(&mut self, i: usize, value: bool) {
        let w = self.words.get(i / 64);
        let bit = 1u64 << (i % 64);
        let new = if value { w | bit } else { w & !bit };
        if new != w {
            self.words.set(i / 64, new);
        }
    }

    pub fn push(&mut self, value: bool) {
        if self.len.is_multiple_of(64) {
            self.words.push(0);
        }
        self.len += 1;
        self.set(self.len - 1, value);
    }

    pub fn unique_heap_bytes(&self, seen: &mut HashSet<usize>) -> u64 {
        self.words.unique_heap_bytes(seen)
    }
}
