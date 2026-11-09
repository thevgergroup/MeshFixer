//! Chunked, reference-counted storage with copy-on-write per chunk
//! (MF-DOC-002, ADR-0001). Revisions share every chunk they didn't touch.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use mf_runtime::{Cancelled, RunContext};
use rayon::prelude::*;

static CHUNK_HASHES: AtomicU64 = AtomicU64::new(0);

/// Number of chunk hashes computed so far in this process (diagnostics and
/// tests: proves a revision rehashes only the chunks it copied).
pub fn chunk_hashes_computed() -> u64 {
    CHUNK_HASHES.load(Ordering::Relaxed)
}

/// Canonical little-endian bytes of a stored element.
pub trait ChunkBytes {
    fn hash_into(&self, h: &mut blake3::Hasher);
}

impl ChunkBytes for [f64; 3] {
    fn hash_into(&self, h: &mut blake3::Hasher) {
        for c in self {
            h.update(&c.to_le_bytes());
        }
    }
}

impl ChunkBytes for [u32; 3] {
    fn hash_into(&self, h: &mut blake3::Hasher) {
        for c in self {
            h.update(&c.to_le_bytes());
        }
    }
}

impl ChunkBytes for u64 {
    fn hash_into(&self, h: &mut blake3::Hasher) {
        h.update(&self.to_le_bytes());
    }
}

/// One chunk of elements plus its cached hash. Cloning (copy-on-write)
/// starts with an empty cache, so a copied chunk can never carry a stale hash.
pub struct Chunk<T> {
    data: Vec<T>,
    hash: OnceLock<[u8; 32]>,
}

impl<T: Clone> Clone for Chunk<T> {
    fn clone(&self) -> Self {
        Chunk {
            data: self.data.clone(),
            hash: OnceLock::new(),
        }
    }
}

impl<T: ChunkBytes> Chunk<T> {
    /// The chunk's hash, computed on first use.
    fn digest(&self) -> &[u8; 32] {
        self.hash.get_or_init(|| {
            CHUNK_HASHES.fetch_add(1, Ordering::Relaxed);
            let mut ch = blake3::Hasher::new();
            for x in &self.data {
                x.hash_into(&mut ch);
            }
            *ch.finalize().as_bytes()
        })
    }
}

impl<T> Chunk<T> {
    fn new(data: Vec<T>) -> Self {
        Chunk {
            data,
            hash: OnceLock::new(),
        }
    }

    /// Mutable access; clears the cached hash.
    fn data_mut(&mut self) -> &mut Vec<T> {
        self.hash = OnceLock::new();
        &mut self.data
    }
}

/// Elements per chunk.
pub const CHUNK: usize = 32_768;

#[allow(clippy::len_without_is_empty)] // crate-internal
#[derive(Clone, Default)]
pub struct Chunked<T> {
    chunks: Vec<Arc<Chunk<T>>>,
    len: usize,
}

impl<T: Copy> Chunked<T> {
    pub fn from_vec(v: Vec<T>) -> Self {
        let len = v.len();
        let chunks = v
            .chunks(CHUNK)
            .map(|c| Arc::new(Chunk::new(c.to_vec())))
            .collect();
        Chunked { chunks, len }
    }

    /// As [`Chunked::from_vec`], checking for cancellation between chunks.
    pub fn from_vec_with(v: Vec<T>, ctx: &RunContext) -> Result<Self, Cancelled> {
        let len = v.len();
        let mut chunks = Vec::with_capacity(len.div_ceil(CHUNK));
        for c in v.chunks(CHUNK) {
            ctx.checkpoint()?;
            chunks.push(Arc::new(Chunk::new(c.to_vec())));
        }
        Ok(Chunked { chunks, len })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn get(&self, i: usize) -> T {
        self.chunks[i / CHUNK].data[i % CHUNK]
    }

    pub fn iter(&self) -> impl Iterator<Item = T> + '_ {
        self.chunks.iter().flat_map(|c| c.data.iter().copied())
    }

    /// Write one element, copying its chunk first if another revision shares it.
    pub fn set(&mut self, i: usize, value: T) {
        Arc::make_mut(&mut self.chunks[i / CHUNK]).data_mut()[i % CHUNK] = value;
    }

    pub fn push(&mut self, value: T) {
        if self.len.is_multiple_of(CHUNK) {
            self.chunks.push(Arc::new(Chunk::new(Vec::new())));
        }
        if let Some(last) = self.chunks.last_mut() {
            Arc::make_mut(last).data_mut().push(value);
        }
        self.len += 1;
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// The elements of chunk `i` (empty past the end).
    pub fn chunk(&self, i: usize) -> &[T] {
        self.chunks.get(i).map_or(&[], |c| &c.data)
    }

    pub fn shares_chunk(&self, other: &Chunked<T>, i: usize) -> bool {
        match (self.chunks.get(i), other.chunks.get(i)) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Feed every chunk's (cached) hash into `h`, plus the length.
    pub fn hash_into(&self, h: &mut blake3::Hasher)
    where
        T: ChunkBytes,
    {
        h.update(&(self.len as u64).to_le_bytes());
        for c in &self.chunks {
            h.update(c.digest());
        }
    }

    /// Compute and cache every chunk's hash, in parallel, checking for
    /// cancellation between chunks. Cached hashes are kept either way, so
    /// a cancelled call loses no work.
    pub fn warm_hashes(&self, ctx: &RunContext) -> Result<(), Cancelled>
    where
        T: ChunkBytes + Send + Sync,
    {
        self.chunks.par_iter().try_for_each(|c| {
            ctx.checkpoint()?;
            c.digest();
            Ok(())
        })
    }

    /// Heap bytes of chunks not already in `seen` (chunk identity by address).
    pub fn unique_heap_bytes(&self, seen: &mut HashSet<usize>) -> u64 {
        let own = self.chunks.capacity() * std::mem::size_of::<Arc<Chunk<T>>>();
        let chunks: usize = self
            .chunks
            .iter()
            .filter(|c| seen.insert(Arc::as_ptr(c) as usize))
            .map(|c| {
                c.data.capacity() * std::mem::size_of::<T>()
                    + std::mem::size_of::<Chunk<T>>()
                    + 2 * std::mem::size_of::<usize>()
            })
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

    /// Word `w` (bits `64w..64w+64`); zero past the end, like the unused
    /// bits of the last word.
    fn word(&self, w: usize) -> u64 {
        if w < self.words.len() {
            self.words.get(w)
        } else {
            0
        }
    }

    /// True if bits `start..start + len` are the same in both. `start`
    /// must be a multiple of 64. O(len / 64), or O(1) when the words are
    /// shared storage.
    pub fn range_eq(&self, other: &Bits, start: usize, len: usize) -> bool {
        debug_assert!(start.is_multiple_of(64));
        let (first, end) = (start / 64, (start + len).div_ceil(64));
        if end == first {
            return true;
        }
        let chunk = first / CHUNK;
        if (end - 1) / CHUNK == chunk && self.words.shares_chunk(&other.words, chunk) {
            return true;
        }
        (first..end).all(|w| self.word(w) == other.word(w))
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

    pub fn hash_into(&self, h: &mut blake3::Hasher) {
        h.update(&(self.len as u64).to_le_bytes());
        self.words.hash_into(h);
    }

    pub fn warm_hashes(&self, ctx: &RunContext) -> Result<(), Cancelled> {
        self.words.warm_hashes(ctx)
    }
}
