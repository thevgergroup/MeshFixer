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

/// Elements per **group**: the unit a hash is cached for and the GPU's
/// face chunk (MF-RENDER-003). A group is a run of storage chunks.
pub const CHUNK: usize = 32_768;

/// Elements per **storage chunk**: the copy-on-write unit (MF-DOC-002).
/// Small, so one moved vertex copies 24 KB rather than a whole group
/// (Phase 1 Task 16b, M4 review I2): a 1,000-stroke sculpt history stays
/// near one revision plus the stroke footprints, while hashes and GPU
/// buffers stay per group.
pub const STORAGE_CHUNK: usize = 1_024;

const PER_GROUP: usize = CHUNK / STORAGE_CHUNK;

/// One group's storage chunks plus the group's cached hash. Cloning (copy
/// on write) starts with an empty cache, and every write clears it, so a
/// changed group never carries a stale hash.
pub struct Group<T> {
    chunks: Vec<Arc<Vec<T>>>,
    hash: OnceLock<[u8; 32]>,
}

impl<T> Clone for Group<T> {
    fn clone(&self) -> Self {
        Group {
            chunks: self.chunks.clone(),
            hash: OnceLock::new(),
        }
    }
}

impl<T: ChunkBytes> Group<T> {
    /// The group's hash: BLAKE3 over its elements in order (the same bytes
    /// whatever the storage chunk size), computed on first use.
    fn digest(&self) -> &[u8; 32] {
        self.hash.get_or_init(|| {
            CHUNK_HASHES.fetch_add(1, Ordering::Relaxed);
            let mut ch = blake3::Hasher::new();
            for x in self.chunks.iter().flat_map(|c| c.iter()) {
                x.hash_into(&mut ch);
            }
            *ch.finalize().as_bytes()
        })
    }
}

impl<T: Copy> Group<T> {
    fn from_slice(items: &[T]) -> Self {
        Group {
            chunks: items
                .chunks(STORAGE_CHUNK)
                .map(|c| Arc::new(c.to_vec()))
                .collect(),
            hash: OnceLock::new(),
        }
    }

    fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        self.chunks.iter().flat_map(|c| c.iter())
    }
}

#[allow(clippy::len_without_is_empty)] // crate-internal
#[derive(Clone)]
pub struct Chunked<T> {
    groups: Vec<Arc<Group<T>>>,
    len: usize,
}

impl<T> Default for Chunked<T> {
    fn default() -> Self {
        Chunked {
            groups: Vec::new(),
            len: 0,
        }
    }
}

impl<T: Copy> Chunked<T> {
    pub fn from_vec(v: Vec<T>) -> Self {
        let len = v.len();
        let groups = v
            .chunks(CHUNK)
            .map(|c| Arc::new(Group::from_slice(c)))
            .collect();
        Chunked { groups, len }
    }

    /// As [`Chunked::from_vec`], checking for cancellation between groups.
    pub fn from_vec_with(v: Vec<T>, ctx: &RunContext) -> Result<Self, Cancelled> {
        let len = v.len();
        let mut groups = Vec::with_capacity(len.div_ceil(CHUNK));
        for c in v.chunks(CHUNK) {
            ctx.checkpoint()?;
            groups.push(Arc::new(Group::from_slice(c)));
        }
        Ok(Chunked { groups, len })
    }

    /// Collect `items` straight into chunks, checking for cancellation
    /// between groups. Unlike [`Chunked::from_vec_with`] there's no whole
    /// `Vec` to copy from, so the transient cost is one storage chunk.
    pub fn from_iter_with(
        items: impl IntoIterator<Item = T>,
        ctx: &RunContext,
    ) -> Result<Self, Cancelled> {
        let mut out = Chunked::default();
        let mut current: Vec<T> = Vec::with_capacity(STORAGE_CHUNK);
        for x in items {
            current.push(x);
            if current.len() == STORAGE_CHUNK {
                let full = std::mem::replace(&mut current, Vec::with_capacity(STORAGE_CHUNK));
                out.push_chunk(full);
                if out.len.is_multiple_of(CHUNK) {
                    ctx.checkpoint()?;
                }
            }
        }
        if !current.is_empty() {
            current.shrink_to_fit();
            out.push_chunk(current);
        }
        Ok(out)
    }

    /// Append a storage chunk; `self.len` must be a multiple of
    /// [`STORAGE_CHUNK`].
    fn push_chunk(&mut self, chunk: Vec<T>) {
        debug_assert!(self.len.is_multiple_of(STORAGE_CHUNK));
        if self.len.is_multiple_of(CHUNK) {
            self.groups.push(Arc::new(Group {
                chunks: Vec::with_capacity(PER_GROUP),
                hash: OnceLock::new(),
            }));
        }
        self.len += chunk.len();
        let g = self.last_group_mut();
        g.chunks.push(Arc::new(chunk));
    }

    /// The last group, unshared, with its hash cleared.
    fn last_group_mut(&mut self) -> &mut Group<T> {
        let g = Arc::make_mut(self.groups.last_mut().expect("there is a last group"));
        g.hash = OnceLock::new();
        g
    }

    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn get(&self, i: usize) -> T {
        self.groups[i / CHUNK].chunks[(i % CHUNK) / STORAGE_CHUNK][i % STORAGE_CHUNK]
    }

    pub fn iter(&self) -> impl Iterator<Item = T> + '_ {
        self.groups.iter().flat_map(|g| g.iter().copied())
    }

    /// Write one element, copying its storage chunk (and its group's index)
    /// first if another revision shares them.
    pub fn set(&mut self, i: usize, value: T) {
        let g = Arc::make_mut(&mut self.groups[i / CHUNK]);
        g.hash = OnceLock::new();
        Arc::make_mut(&mut g.chunks[(i % CHUNK) / STORAGE_CHUNK])[i % STORAGE_CHUNK] = value;
    }

    pub fn push(&mut self, value: T) {
        if self.len.is_multiple_of(STORAGE_CHUNK) {
            self.push_chunk(vec![value]);
            return;
        }
        self.len += 1;
        let g = self.last_group_mut();
        let last = g.chunks.last_mut().expect("a partial chunk exists");
        Arc::make_mut(last).push(value);
    }

    /// Groups ([`CHUNK`] elements each, the last possibly fewer).
    pub fn chunk_count(&self) -> usize {
        self.groups.len()
    }

    /// The elements of group `i`, in order (empty past the end).
    pub fn chunk_iter(&self, i: usize) -> impl Iterator<Item = &T> + '_ {
        self.groups.get(i).into_iter().flat_map(|g| g.iter())
    }

    /// Number of elements in group `i` (0 past the end).
    pub fn chunk_len(&self, i: usize) -> usize {
        if i < self.groups.len() {
            (self.len - i * CHUNK).min(CHUNK)
        } else {
            0
        }
    }

    /// True if group `i` holds the same storage in both: one shared group,
    /// or every storage chunk of it shared. False past either end.
    pub fn shares_chunk(&self, other: &Chunked<T>, i: usize) -> bool {
        match (self.groups.get(i), other.groups.get(i)) {
            (Some(a), Some(b)) => {
                Arc::ptr_eq(a, b)
                    || (a.chunks.len() == b.chunks.len()
                        && a.chunks
                            .iter()
                            .zip(&b.chunks)
                            .all(|(x, y)| Arc::ptr_eq(x, y)))
            }
            _ => false,
        }
    }

    /// Storage chunks ([`STORAGE_CHUNK`] elements each).
    pub fn storage_chunk_count(&self) -> usize {
        self.len.div_ceil(STORAGE_CHUNK)
    }

    /// Storage chunk `k` (elements `k·STORAGE_CHUNK..`; empty past the end).
    pub fn storage_chunk(&self, k: usize) -> &[T] {
        self.groups
            .get(k / PER_GROUP)
            .and_then(|g| g.chunks.get(k % PER_GROUP))
            .map_or(&[], |c| c.as_slice())
    }

    /// True if storage chunk `k` is the same storage in both. False past
    /// either end.
    pub fn shares_storage_chunk(&self, other: &Chunked<T>, k: usize) -> bool {
        let get = |c: &Chunked<T>| {
            c.groups
                .get(k / PER_GROUP)
                .and_then(|g| g.chunks.get(k % PER_GROUP))
                .cloned()
        };
        match (get(self), get(other)) {
            (Some(a), Some(b)) => Arc::ptr_eq(&a, &b),
            _ => false,
        }
    }

    /// Feed every group's (cached) hash into `h`, plus the length.
    pub fn hash_into(&self, h: &mut blake3::Hasher)
    where
        T: ChunkBytes,
    {
        h.update(&(self.len as u64).to_le_bytes());
        for g in &self.groups {
            h.update(g.digest());
        }
    }

    /// Compute and cache every group's hash, in parallel, checking for
    /// cancellation between groups. Cached hashes are kept either way, so
    /// a cancelled call loses no work.
    pub fn warm_hashes(&self, ctx: &RunContext) -> Result<(), Cancelled>
    where
        T: ChunkBytes + Send + Sync,
    {
        self.groups.par_iter().try_for_each(|g| {
            ctx.checkpoint()?;
            g.digest();
            Ok(())
        })
    }

    /// Heap bytes of groups and storage chunks not already in `seen`
    /// (identity by address).
    pub fn unique_heap_bytes(&self, seen: &mut HashSet<usize>) -> u64 {
        let arc = 2 * std::mem::size_of::<usize>();
        let mut total = self.groups.capacity() * std::mem::size_of::<Arc<Group<T>>>();
        for g in &self.groups {
            if !seen.insert(Arc::as_ptr(g) as usize) {
                continue;
            }
            total += std::mem::size_of::<Group<T>>()
                + arc
                + g.chunks.capacity() * std::mem::size_of::<Arc<Vec<T>>>();
            for c in &g.chunks {
                if seen.insert(Arc::as_ptr(c) as usize) {
                    total += c.capacity() * std::mem::size_of::<T>()
                        + std::mem::size_of::<Vec<T>>()
                        + arc;
                }
            }
        }
        total as u64
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
