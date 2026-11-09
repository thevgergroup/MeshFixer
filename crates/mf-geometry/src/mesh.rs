use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use mf_runtime::{Cancelled, RunContext};
use serde::Serialize;

use crate::chunked::{Bits, CHUNK, Chunked};
use crate::edit::MeshEdit;
use crate::hash::ContentHash;
use crate::ids::{FaceId, SlotRemap, VertexId};
use crate::topology::Topology;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GeometryError {
    #[error("triangle {triangle} references vertex {index}, which does not exist")]
    IndexOutOfRange { triangle: usize, index: u32 },
    #[error("vertex {vertex} has a non-finite coordinate")]
    NonFinite { vertex: usize },
    #[error("mesh too large: {vertices} vertices / {faces} faces exceed 32-bit indices")]
    TooLarge { vertices: usize, faces: usize },
    #[error("face {0} does not exist or was removed")]
    NoSuchFace(u32),
    #[error("vertex {0} does not exist or was removed")]
    NoSuchVertex(u32),
    #[error("face {0} is not a face of the input revision, so it can't be a source")]
    InvalidSource(u32),
    #[error("face {face} still uses removed vertex {vertex}")]
    FaceUsesRemovedVertex { face: u32, vertex: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Aabb {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

impl Aabb {
    pub fn center(&self) -> [f64; 3] {
        [0, 1, 2].map(|i| (self.min[i] + self.max[i]) * 0.5)
    }

    pub fn diagonal(&self) -> f64 {
        (0..3)
            .map(|i| (self.max[i] - self.min[i]).powi(2))
            .sum::<f64>()
            .sqrt()
    }
}

/// Old-slot → new-slot maps produced by [`Mesh::compacted`].
pub type Compaction = SlotRemap;

/// An immutable indexed triangle mesh (working representation, MF-GEO-001).
///
/// Faces and vertices live in stable slots (ADR-0001): removing one leaves a
/// tombstone, creating one appends. Storage is chunked and shared between
/// revisions, which differ only in the chunks an edit touched. Cloning is
/// cheap. A `Mesh` is never mutated after construction (MF-DOC-001).
#[derive(Clone)]
pub struct Mesh {
    inner: Arc<Inner>,
}

thread_local! {
    /// The meshes (by identity) whose topology this thread is computing,
    /// holding their topology lock (see [`Mesh::topology_with`]).
    static COMPUTING_TOPOLOGY: std::cell::RefCell<Vec<usize>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Marks a mesh's topology as being computed on this thread, until dropped
/// (also when the computation panics or is cancelled).
struct ComputingTopology(usize);

impl ComputingTopology {
    fn enter(key: usize) -> ComputingTopology {
        COMPUTING_TOPOLOGY.with(|c| c.borrow_mut().push(key));
        ComputingTopology(key)
    }
}

impl Drop for ComputingTopology {
    fn drop(&mut self) {
        COMPUTING_TOPOLOGY.with(|c| {
            let mut c = c.borrow_mut();
            if let Some(i) = c.iter().rposition(|k| *k == self.0) {
                c.remove(i);
            }
        });
    }
}

pub(crate) struct Inner {
    pub(crate) positions: Chunked<[f64; 3]>,
    pub(crate) faces: Chunked<[u32; 3]>,
    pub(crate) face_live: Bits,
    pub(crate) vertex_live: Bits,
    pub(crate) face_count: usize,
    pub(crate) vertex_count: usize,
    hash: OnceLock<ContentHash>,
    structure: OnceLock<ContentHash>,
    /// Derived, so it can be dropped and recomputed (review I5).
    topology: Mutex<Option<Arc<Topology>>>,
}

impl std::fmt::Debug for Mesh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mesh")
            .field("vertices", &self.vertex_count())
            .field("triangles", &self.triangle_count())
            .field("face_slots", &self.face_slots())
            .finish()
    }
}

impl Mesh {
    /// Element counts must fit 32-bit indices (MF-ISSUE-001).
    pub fn check_counts(vertices: usize, faces: usize) -> Result<(), GeometryError> {
        if vertices > u32::MAX as usize || faces > u32::MAX as usize / 3 {
            return Err(GeometryError::TooLarge { vertices, faces });
        }
        Ok(())
    }

    /// A mesh whose slots are all live, in the given order.
    pub fn new(positions: Vec<[f64; 3]>, triangles: Vec<[u32; 3]>) -> Result<Mesh, GeometryError> {
        Mesh::new_with(positions, triangles, &RunContext::none())
            .expect("a context that is never cancelled can't be cancelled")
    }

    /// As [`Mesh::new`], checking for cancellation while validating (it is
    /// linear in the input). The outer error is cancellation, the inner one
    /// invalid input.
    pub fn new_with(
        positions: Vec<[f64; 3]>,
        triangles: Vec<[u32; 3]>,
        ctx: &RunContext,
    ) -> Result<Result<Mesh, GeometryError>, Cancelled> {
        if let Err(e) = Self::check_counts(positions.len(), triangles.len()) {
            return Ok(Err(e));
        }
        let mut tick = ctx.ticker();
        for (vertex, p) in positions.iter().enumerate() {
            tick.tick()?;
            if !p.iter().all(|c| c.is_finite()) {
                return Ok(Err(GeometryError::NonFinite { vertex }));
            }
        }
        let n = positions.len();
        for (triangle, t) in triangles.iter().enumerate() {
            tick.tick()?;
            if let Some(&index) = t.iter().find(|&&i| i as usize >= n) {
                return Ok(Err(GeometryError::IndexOutOfRange { triangle, index }));
            }
        }
        let (face_count, vertex_count) = (triangles.len(), positions.len());
        // Copying into chunks is ~100 MB at 5M triangles.
        let positions = Chunked::from_vec_with(positions, ctx)?;
        let triangles = Chunked::from_vec_with(triangles, ctx)?;
        ctx.checkpoint()?;
        Ok(Ok(Mesh::from_parts(
            positions,
            triangles,
            Bits::all_set(face_count),
            Bits::all_set(vertex_count),
            face_count,
            vertex_count,
        )))
    }

    pub(crate) fn from_parts(
        positions: Chunked<[f64; 3]>,
        faces: Chunked<[u32; 3]>,
        face_live: Bits,
        vertex_live: Bits,
        face_count: usize,
        vertex_count: usize,
    ) -> Mesh {
        Mesh {
            inner: Arc::new(Inner {
                positions,
                faces,
                face_live,
                vertex_live,
                face_count,
                vertex_count,
                hash: OnceLock::new(),
                structure: OnceLock::new(),
                topology: Mutex::new(None),
            }),
        }
    }

    pub(crate) fn inner(&self) -> &Inner {
        &self.inner
    }

    pub fn empty() -> Mesh {
        Mesh::new(Vec::new(), Vec::new()).expect("empty mesh is valid")
    }

    /// Live triangles.
    pub fn triangle_count(&self) -> usize {
        self.inner.face_count
    }

    pub fn face_count(&self) -> usize {
        self.inner.face_count
    }

    /// Live vertices.
    pub fn vertex_count(&self) -> usize {
        self.inner.vertex_count
    }

    /// Face slots including tombstones; new faces get IDs from here up.
    pub fn face_slots(&self) -> u32 {
        self.inner.faces.len() as u32
    }

    pub fn vertex_slots(&self) -> u32 {
        self.inner.positions.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.inner.face_count == 0
    }

    pub fn is_face_live(&self, id: FaceId) -> bool {
        self.inner.face_live.get(id.0 as usize)
    }

    pub fn is_vertex_live(&self, id: VertexId) -> bool {
        self.inner.vertex_live.get(id.0 as usize)
    }

    pub fn face(&self, id: FaceId) -> Option<[u32; 3]> {
        self.is_face_live(id)
            .then(|| self.inner.faces.get(id.0 as usize))
    }

    pub fn position(&self, id: VertexId) -> Option<[f64; 3]> {
        self.is_vertex_live(id)
            .then(|| self.inner.positions.get(id.0 as usize))
    }

    /// Position of a vertex slot referenced by a live face. Faces never
    /// reference removed vertices (enforced by [`MeshEdit::finish`]).
    pub fn corner_position(&self, v: u32) -> [f64; 3] {
        self.inner.positions.get(v as usize)
    }

    /// Live faces in ascending ID order.
    pub fn faces(&self) -> impl Iterator<Item = (FaceId, [u32; 3])> + '_ {
        let live = &self.inner.face_live;
        self.inner
            .faces
            .iter()
            .enumerate()
            .filter(move |(i, _)| live.get(*i))
            .map(|(i, t)| (FaceId(i as u32), t))
    }

    /// Live vertices in ascending ID order.
    pub fn vertices(&self) -> impl Iterator<Item = (VertexId, [f64; 3])> + '_ {
        let live = &self.inner.vertex_live;
        self.inner
            .positions
            .iter()
            .enumerate()
            .filter(move |(i, _)| live.get(*i))
            .map(|(i, p)| (VertexId(i as u32), p))
    }

    /// True if no slot is tombstoned.
    pub fn is_compact(&self) -> bool {
        self.inner.face_count == self.inner.faces.len()
            && self.inner.vertex_count == self.inner.positions.len()
    }

    /// Live geometry as plain arrays, renumbered densely in slot order.
    pub fn to_arrays(&self) -> (Vec<[f64; 3]>, Vec<[u32; 3]>) {
        let (positions, triangles, _) = self.compact_arrays();
        (positions, triangles)
    }

    fn compact_arrays(&self) -> (Vec<[f64; 3]>, Vec<[u32; 3]>, Compaction) {
        let mut vertex_map = vec![None; self.inner.positions.len()];
        let mut positions = Vec::with_capacity(self.vertex_count());
        for (id, p) in self.vertices() {
            vertex_map[id.0 as usize] = Some(VertexId(positions.len() as u32));
            positions.push(p);
        }
        let mut face_map = vec![None; self.inner.faces.len()];
        let mut triangles = Vec::with_capacity(self.face_count());
        for (id, t) in self.faces() {
            face_map[id.0 as usize] = Some(FaceId(triangles.len() as u32));
            triangles.push(t.map(|v| vertex_map[v as usize].map_or(u32::MAX, |n| n.0)));
        }
        let map = Compaction {
            faces: face_map,
            vertices: vertex_map,
        };
        (positions, triangles, map)
    }

    /// A tombstone-free copy plus the slot maps (used before export).
    pub fn compacted(&self) -> (Mesh, Compaction) {
        let (positions, triangles, map) = self.compact_arrays();
        let mesh = Mesh::new(positions, triangles).expect("compacting a valid mesh is valid");
        (mesh, map)
    }

    pub fn bounding_box(&self) -> Option<Aabb> {
        self.bounding_box_with(&RunContext::none())
            .expect("a context that is never cancelled can't be cancelled")
    }

    /// As [`Mesh::bounding_box`], cancellable.
    pub fn bounding_box_with(&self, ctx: &RunContext) -> Result<Option<Aabb>, Cancelled> {
        let mut it = self.vertices().map(|(_, p)| p);
        let Some(first) = it.next() else {
            return Ok(None);
        };
        let mut bb = Aabb {
            min: first,
            max: first,
        };
        let mut tick = ctx.ticker();
        for p in it {
            tick.tick()?;
            for (i, &c) in p.iter().enumerate() {
                bb.min[i] = bb.min[i].min(c);
                bb.max[i] = bb.max[i].max(c);
            }
        }
        Ok(Some(bb))
    }

    /// Canonical content hash (MF-DOC-003), cached. Covers every slot (with
    /// liveness) of positions and faces, so slot identity is part of content
    /// identity. Built from per-chunk hashes cached on the shared chunks, so
    /// a new revision only hashes the chunks it copied.
    pub fn content_hash(&self) -> ContentHash {
        *self.inner.hash.get_or_init(|| {
            let mut h = blake3::Hasher::new();
            h.update(b"mf-mesh-v3");
            h.update(self.structure_hash().0.as_slice());
            self.inner.positions.hash_into(&mut h);
            ContentHash(*h.finalize().as_bytes())
        })
    }

    /// Hash of everything except vertex positions: slots, liveness and face
    /// indices (so also orientation). Two revisions with equal structure
    /// hashes differ at most in where their vertices are, which is what a
    /// cross-platform `Equivalent` replay may differ in (MF-CMD-004).
    pub fn structure_hash(&self) -> ContentHash {
        *self.inner.structure.get_or_init(|| {
            let mut h = blake3::Hasher::new();
            h.update(b"mf-structure-v1");
            h.update(&(self.inner.positions.len() as u64).to_le_bytes());
            self.inner.vertex_live.hash_into(&mut h);
            self.inner.face_live.hash_into(&mut h);
            self.inner.faces.hash_into(&mut h);
            ContentHash(*h.finalize().as_bytes())
        })
    }

    /// The content and structure hashes, computing any chunk hashes not yet
    /// cached in parallel and cancellably (a 5M-triangle mesh hashed from
    /// scratch takes a few hundred milliseconds single-threaded).
    pub fn hashes_with(&self, ctx: &RunContext) -> Result<(ContentHash, ContentHash), Cancelled> {
        if self.inner.hash.get().is_none() {
            self.inner.positions.warm_hashes(ctx)?;
            self.inner.faces.warm_hashes(ctx)?;
            self.inner.face_live.warm_hashes(ctx)?;
            self.inner.vertex_live.warm_hashes(ctx)?;
        }
        Ok((self.content_hash(), self.structure_hash()))
    }

    /// Derived topology, computed on first use and cached (MF-GEO-003).
    pub fn topology(&self) -> Arc<Topology> {
        self.topology_with(&RunContext::none())
            .expect("a context that is never cancelled can't be cancelled")
    }

    /// As [`Mesh::topology`], cancellable, reporting progress as stage
    /// `"topology"`. A cancelled computation caches nothing.
    ///
    /// Re-entered on a thread that is already computing this mesh's
    /// topology (a rayon worker waiting in a join inside that computation
    /// can pick up another job that asks for it), it computes its own copy
    /// rather than wait for the lock its own thread holds: that wait would
    /// never end.
    pub fn topology_with(&self, ctx: &RunContext) -> Result<Arc<Topology>, Cancelled> {
        let key = Arc::as_ptr(&self.inner) as usize;
        if COMPUTING_TOPOLOGY.with(|c| c.borrow().contains(&key)) {
            return Ok(Arc::new(Topology::compute_with(self, ctx)?));
        }
        let mut cached = self.lock_topology_with(ctx)?;
        if let Some(topo) = cached.as_ref() {
            ctx.progress.report("topology", 1.0);
            return Ok(topo.clone());
        }
        let topo = {
            let _computing = ComputingTopology::enter(key);
            Arc::new(Topology::compute_with(self, ctx)?)
        };
        *cached = Some(topo.clone());
        Ok(topo)
    }

    /// As [`Mesh::topology_with`], unless another computation of it is
    /// under way: then `None` at once (that one caches it). For background
    /// jobs that only warm the cache: waiting could deadlock a thread pool
    /// whose worker, inside that computation's parallel work, picked this
    /// job up (M4 Task 8a).
    pub fn topology_unless_busy(
        &self,
        ctx: &RunContext,
    ) -> Result<Option<Arc<Topology>>, Cancelled> {
        let key = Arc::as_ptr(&self.inner) as usize;
        if COMPUTING_TOPOLOGY.with(|c| c.borrow().contains(&key)) {
            return Ok(None);
        }
        let mut cached = match self.inner.topology.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return Ok(None),
        };
        if let Some(topo) = cached.as_ref() {
            return Ok(Some(topo.clone()));
        }
        let topo = {
            let _computing = ComputingTopology::enter(key);
            Arc::new(Topology::compute_with(self, ctx)?)
        };
        *cached = Some(topo.clone());
        Ok(Some(topo))
    }

    /// The topology lock, waited for cancellably: another thread may hold
    /// it for as long as computing the topology takes, and a cancelled
    /// caller must not wait that out (MF-RT-002).
    fn lock_topology_with(
        &self,
        ctx: &RunContext,
    ) -> Result<std::sync::MutexGuard<'_, Option<Arc<Topology>>>, Cancelled> {
        loop {
            match self.inner.topology.try_lock() {
                Ok(guard) => return Ok(guard),
                // See `lock_topology`: a poisoned lock is safe to use.
                Err(std::sync::TryLockError::Poisoned(p)) => return Ok(p.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => {
                    ctx.checkpoint()?;
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        }
    }

    fn lock_topology(&self) -> std::sync::MutexGuard<'_, Option<Arc<Topology>>> {
        // A panic while computing topology leaves nothing half-written (the
        // value is only stored on success), so a poisoned lock is safe to use.
        self.inner
            .topology
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Drop the cached topology; the next `topology()` call recomputes it.
    /// Used to keep only the current revision's topology in memory.
    pub fn release_topology(&self) {
        *self.lock_topology() = None;
    }

    /// True if this revision's topology has been computed and is cached.
    /// Never waits: while another thread holds the topology lock (for as
    /// long as computing the topology takes) it answers false, so a UI
    /// thread can ask every frame (MF-RT-001).
    pub fn has_cached_topology(&self) -> bool {
        match self.inner.topology.try_lock() {
            Ok(guard) => guard.is_some(),
            // See `lock_topology`: a poisoned lock is safe to use.
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().is_some(),
            Err(std::sync::TryLockError::WouldBlock) => false,
        }
    }

    /// Heap bytes owned by this mesh's storage (excludes cached topology).
    pub fn heap_bytes(&self) -> u64 {
        self.unique_heap_bytes(&mut HashSet::new())
    }

    /// Heap bytes of chunks not already counted in `seen`. Summing this over
    /// several revisions counts shared chunks once.
    pub fn unique_heap_bytes(&self, seen: &mut HashSet<usize>) -> u64 {
        let own = if seen.insert(Arc::as_ptr(&self.inner) as usize) {
            std::mem::size_of::<Inner>() as u64
        } else {
            0
        };
        own + self.inner.positions.unique_heap_bytes(seen)
            + self.inner.faces.unique_heap_bytes(seen)
            + self.inner.face_live.unique_heap_bytes(seen)
            + self.inner.vertex_live.unique_heap_bytes(seen)
    }

    pub fn edit(&self) -> MeshEdit {
        MeshEdit::new(self)
    }

    pub fn face_chunk_count(&self) -> usize {
        self.inner.faces.chunk_count()
    }

    pub fn position_chunk_count(&self) -> usize {
        self.inner.positions.chunk_count()
    }

    pub fn shares_face_chunk(&self, other: &Mesh, i: usize) -> bool {
        self.inner.faces.shares_chunk(&other.inner.faces, i)
    }

    pub fn shares_position_chunk(&self, other: &Mesh, i: usize) -> bool {
        self.inner.positions.shares_chunk(&other.inner.positions, i)
    }

    /// True if face chunk `i` (slots `i·CHUNK..`) is the same in both
    /// revisions: the same shared face storage *and* the same liveness, so
    /// a removal, which only clears a tombstone bit, still counts as a
    /// change (MF-RENDER-003). False past either mesh's end.
    pub fn face_chunk_shared(&self, other: &Mesh, i: usize) -> bool {
        self.inner.faces.shares_chunk(&other.inner.faces, i)
            && self
                .inner
                .face_live
                .range_eq(&other.inner.face_live, i * CHUNK, CHUNK)
    }

    /// True if vertex chunk `j` shares its positions with `other`'s (what a
    /// face referencing it is drawn with). False past either mesh's end.
    pub fn vertex_chunk_shared(&self, other: &Mesh, j: usize) -> bool {
        self.shares_position_chunk(other, j)
    }

    /// True if every vertex slot `prev` has in vertex chunk `j` holds the
    /// same position here: shared storage, or a copy that only appended
    /// vertices (as a fill does to the last chunk). What faces built for
    /// `prev` need to still be drawn right (MF-RENDER-003). O(1) when
    /// shared, else a bitwise compare of the chunk.
    pub fn vertex_chunk_kept(&self, prev: &Mesh, j: usize) -> bool {
        if self.vertex_chunk_shared(prev, j) {
            return true;
        }
        let (now, then) = (self.inner.positions.chunk(j), prev.inner.positions.chunk(j));
        now.len() >= then.len()
            && !then.is_empty()
            && now
                .iter()
                .zip(then)
                .all(|(a, b)| a.map(f64::to_bits) == b.map(f64::to_bits))
    }

    /// Live faces in face chunk `i`, in ascending ID order.
    pub fn faces_in_chunk(&self, i: usize) -> impl Iterator<Item = (FaceId, [u32; 3])> + '_ {
        let live = &self.inner.face_live;
        let base = i * CHUNK;
        self.inner
            .faces
            .chunk(i)
            .iter()
            .enumerate()
            .map(move |(k, t)| (base + k, *t))
            .filter(move |(f, _)| live.get(*f))
            .map(|(f, t)| (FaceId(f as u32), t))
    }

    /// True if both handles point at the same revision.
    pub fn ptr_eq(a: &Mesh, b: &Mesh) -> bool {
        Arc::ptr_eq(&a.inner, &b.inner)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may panic on failure
mod tests {
    use super::*;

    /// A rayon worker waiting in a join inside a topology computation can
    /// pick up another job asking for the same mesh's topology. It must
    /// not wait for the lock its own thread holds (that hung the desktop's
    /// analysis against its picking-BVH builder, M4 Task 8a): it computes
    /// its own copy.
    #[test]
    fn topology_asked_for_again_while_computing_it_on_this_thread_does_not_hang() {
        let mesh = Mesh::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0, 1, 2]],
        )
        .unwrap();
        // As topology_with does while it computes: the lock held, the mesh
        // marked as being computed on this thread.
        let held = mesh.lock_topology();
        let key = Arc::as_ptr(&mesh.inner) as usize;
        let computing = ComputingTopology::enter(key);
        let topo = mesh.topology();
        assert_eq!(topo.components.len(), 1);
        assert!(held.is_none(), "the copy isn't cached under the held lock");
        drop((computing, held));
        // Afterwards it's computed and cached as usual.
        let cached = mesh.topology();
        assert!(Arc::ptr_eq(&cached, &mesh.topology()));
    }

    /// A job that only warms the cache doesn't wait for a computation
    /// under way (it could be holding up that very computation).
    #[test]
    fn topology_unless_busy_does_not_wait() {
        let mesh = Mesh::new(
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            vec![[0, 1, 2]],
        )
        .unwrap();
        let ctx = RunContext::none();
        let held = mesh.lock_topology();
        let other = mesh.clone();
        let busy = std::thread::spawn(move || other.topology_unless_busy(&ctx).unwrap().is_none());
        assert!(busy.join().unwrap(), "busy: no answer, no wait");
        drop(held);
        let topo = mesh
            .topology_unless_busy(&RunContext::none())
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&topo, &mesh.topology()), "computed and cached");
    }
}
