use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use mf_runtime::{Cancelled, RunContext};
use serde::Serialize;

use crate::chunked::{Bits, CHUNK, Chunked, STORAGE_CHUNK};
use crate::edit::MeshEdit;
use crate::hash::ContentHash;
use crate::ids::{FaceId, SlotRemap, VertexId};
use crate::materials::{Material, MaterialData, NO_MATERIAL};
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

/// The new number of a vertex a live face uses. Faces never reference
/// removed vertices (enforced by [`MeshEdit::finish`]).
fn live_vertex(map: &[Option<VertexId>], v: u32) -> u32 {
    map[v as usize]
        .expect("a live face only uses live vertices (MeshEdit::finish checks)")
        .0
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

pub(crate) struct Inner {
    pub(crate) positions: Chunked<[f64; 3]>,
    pub(crate) faces: Chunked<[u32; 3]>,
    pub(crate) face_live: Bits,
    pub(crate) vertex_live: Bits,
    pub(crate) face_count: usize,
    pub(crate) vertex_count: usize,
    /// Per-face materials (MF-ISSUE-091).
    pub(crate) materials: MaterialData,
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
        Mesh::from_parts_with(
            positions,
            faces,
            face_live,
            vertex_live,
            face_count,
            vertex_count,
            MaterialData::default(),
        )
    }

    pub(crate) fn from_parts_with(
        positions: Chunked<[f64; 3]>,
        faces: Chunked<[u32; 3]>,
        face_live: Bits,
        vertex_live: Bits,
        face_count: usize,
        vertex_count: usize,
        materials: MaterialData,
    ) -> Mesh {
        Mesh {
            inner: Arc::new(Inner {
                positions,
                faces,
                face_live,
                vertex_live,
                face_count,
                vertex_count,
                materials,
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
        let vertex_map = self.vertex_renumbering();
        let positions = self.vertices().map(|(_, p)| p).collect();
        let triangles = self
            .faces()
            .map(|(_, t)| t.map(|v| live_vertex(&vertex_map, v)))
            .collect();
        (positions, triangles)
    }

    /// Old vertex slot → dense number, in slot order.
    fn vertex_renumbering(&self) -> Vec<Option<VertexId>> {
        let mut map = vec![None; self.inner.positions.len()];
        for (n, (id, _)) in self.vertices().enumerate() {
            map[id.0 as usize] = Some(VertexId(n as u32));
        }
        map
    }

    /// A tombstone-free copy plus the slot maps (used before export).
    pub fn compacted(&self) -> (Mesh, Compaction) {
        self.compacted_with(&RunContext::none())
            .expect("a context that is never cancelled can't be cancelled")
    }

    /// As [`Mesh::compacted`], cancellable. The live elements keep their
    /// order and are numbered densely from 0 (`mesh.compact`,
    /// MF-ISSUE-020). Built chunk by chunk, so the only transient memory is
    /// the slot maps (which the caller keeps as provenance).
    pub fn compacted_with(&self, ctx: &RunContext) -> Result<(Mesh, Compaction), Cancelled> {
        let mut tick = ctx.ticker();
        let mut vertex_map = vec![None; self.inner.positions.len()];
        for (n, (id, _)) in self.vertices().enumerate() {
            tick.tick()?;
            vertex_map[id.0 as usize] = Some(VertexId(n as u32));
        }
        let mut face_map = vec![None; self.inner.faces.len()];
        for (n, (id, _)) in self.faces().enumerate() {
            tick.tick()?;
            face_map[id.0 as usize] = Some(FaceId(n as u32));
        }
        let positions = Chunked::from_iter_with(self.vertices().map(|(_, p)| p), ctx)?;
        let faces = Chunked::from_iter_with(
            self.faces()
                .map(|(_, t)| t.map(|v| live_vertex(&vertex_map, v))),
            ctx,
        )?;
        let (face_count, vertex_count) = (self.face_count(), self.vertex_count());
        let m = &self.inner.materials;
        let materials = MaterialData {
            palette: m.palette.clone(),
            default: m.default,
            faces: match &m.faces {
                None => None,
                Some(f) => Some(Chunked::from_iter_with(
                    self.faces().map(|(id, _)| f.get(id.0 as usize)),
                    ctx,
                )?),
            },
        };
        let mesh = Mesh::from_parts_with(
            positions,
            faces,
            Bits::all_set(face_count),
            Bits::all_set(vertex_count),
            face_count,
            vertex_count,
            materials,
        );
        let map = Compaction {
            faces: face_map,
            vertices: vertex_map,
        };
        Ok((mesh, map))
    }

    /// Fraction of all slots (faces and vertices) that are tombstones:
    /// storage held by earlier edits that compaction would reclaim.
    pub fn dead_slot_ratio(&self) -> f64 {
        let slots = self.inner.faces.len() + self.inner.positions.len();
        if slots == 0 {
            return 0.0;
        }
        let live = self.inner.face_count + self.inner.vertex_count;
        (slots - live) as f64 / slots as f64
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
            // Nothing at all for a mesh without materials, so hashes from
            // before MF-ISSUE-091 are unchanged.
            self.inner.materials.hash_into(&mut h);
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
    /// Compute, then publish: the cache is only ever locked briefly (to
    /// read it, or to store a finished result), never while computing. A
    /// caller never waits for another caller's computation: inside a
    /// thread pool the computing caller may itself be waiting, in a rayon
    /// join, for the very worker that would be waiting for it, and the
    /// pool would deadlock (M4 Task 8a, fix round 1). So callers that
    /// arrive while one computes compute too, and the first result stored
    /// is kept (the others are dropped; the topology is deterministic, so
    /// they're equal). The cost: occasional duplicate work, and while it
    /// lasts the transient memory of each computation (about the
    /// topology's own size, per caller).
    pub fn topology_with(&self, ctx: &RunContext) -> Result<Arc<Topology>, Cancelled> {
        if let Some(topo) = self.lock_topology().clone() {
            ctx.progress.report("topology", 1.0);
            return Ok(topo);
        }
        let topo = Arc::new(Topology::compute_with(self, ctx)?);
        Ok(self.lock_topology().get_or_insert(topo).clone())
    }

    /// The topology if it is already cached; never computes it (O(1), for
    /// diagnostics on a UI thread).
    pub fn cached_topology(&self) -> Option<Arc<Topology>> {
        self.lock_topology().clone()
    }

    /// The cached topology's slot. Only ever held briefly (see
    /// [`Mesh::topology_with`]).
    fn lock_topology(&self) -> std::sync::MutexGuard<'_, Option<Arc<Topology>>> {
        // Nothing is half-written under this lock (a finished result is
        // stored whole), so a poisoned lock is safe to use.
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
    /// Never waits on a computation (the cache's lock is only held for a
    /// moment), so a UI thread can ask every frame (MF-RT-001).
    pub fn has_cached_topology(&self) -> bool {
        self.lock_topology().is_some()
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
            + self
                .inner
                .materials
                .faces
                .as_ref()
                .map_or(0, |f| f.unique_heap_bytes(seen))
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
        let (now, then) = (&self.inner.positions, &prev.inner.positions);
        now.chunk_len(j) >= then.chunk_len(j)
            && then.chunk_len(j) > 0
            && now
                .chunk_iter(j)
                .zip(then.chunk_iter(j))
                .all(|(a, b)| a.map(f64::to_bits) == b.map(f64::to_bits))
    }

    /// The vertex slots `prev` has whose position differs here (bitwise),
    /// ascending. Vertices appended since `prev` aren't "moved". Only
    /// storage chunks not shared with `prev` are compared, so after a
    /// local edit this is O(chunks + the copied chunks' vertices)
    /// (Phase 1 Task 16b: what a dab invalidates on the GPU).
    pub fn moved_vertices(&self, prev: &Mesh) -> Vec<u32> {
        let (now, then) = (&self.inner.positions, &prev.inner.positions);
        let mut out = Vec::new();
        for k in 0..then.storage_chunk_count() {
            if now.shares_storage_chunk(then, k) {
                continue;
            }
            let (a, b) = (now.storage_chunk(k), then.storage_chunk(k));
            for (i, q) in b.iter().enumerate() {
                if a.get(i)
                    .is_none_or(|p| p.map(f64::to_bits) != q.map(f64::to_bits))
                {
                    out.push((k * STORAGE_CHUNK + i) as u32);
                }
            }
        }
        out
    }

    /// Live faces in face chunk `i`, in ascending ID order.
    pub fn faces_in_chunk(&self, i: usize) -> impl Iterator<Item = (FaceId, [u32; 3])> + '_ {
        let live = &self.inner.face_live;
        let base = i * CHUNK;
        self.inner
            .faces
            .chunk_iter(i)
            .enumerate()
            .map(move |(k, t)| (base + k, *t))
            .filter(move |(f, _)| live.get(*f))
            .map(|(f, t)| (FaceId(f as u32), t))
    }

    /// The material palette (MF-ISSUE-091); empty without materials.
    pub fn materials(&self) -> &[Material] {
        &self.inner.materials.palette
    }

    /// The object-wide default material, if any.
    pub fn default_material(&self) -> Option<u32> {
        let d = self.inner.materials.default;
        (d != NO_MATERIAL).then_some(d)
    }

    /// True if the mesh has any material data.
    pub fn has_materials(&self) -> bool {
        !self.inner.materials.is_empty()
    }

    /// True if faces carry their own materials (not just a default).
    pub fn has_face_materials(&self) -> bool {
        self.inner.materials.faces.is_some()
    }

    /// A live face's own material ([`NO_MATERIAL`] if it has none).
    pub fn face_material_override(&self, id: FaceId) -> u32 {
        match &self.inner.materials.faces {
            Some(f) if (id.0 as usize) < f.len() => f.get(id.0 as usize),
            _ => NO_MATERIAL,
        }
    }

    /// The material a live face prints with: its own, else the default.
    pub fn face_material(&self, id: FaceId) -> Option<u32> {
        let own = self.face_material_override(id);
        let m = if own == NO_MATERIAL {
            self.inner.materials.default
        } else {
            own
        };
        (m != NO_MATERIAL).then_some(m)
    }

    /// This mesh with `palette`, `default` and per-face materials replaced:
    /// `faces[slot]` per face slot (`None`: no per-face materials). The
    /// geometry is shared, not copied.
    pub fn with_materials(
        &self,
        palette: Vec<Material>,
        default: Option<u32>,
        faces: Option<Vec<u32>>,
    ) -> Mesh {
        let i = &self.inner;
        Mesh::from_parts_with(
            i.positions.clone(),
            i.faces.clone(),
            i.face_live.clone(),
            i.vertex_live.clone(),
            i.face_count,
            i.vertex_count,
            MaterialData {
                palette: Arc::new(palette),
                default: default.unwrap_or(NO_MATERIAL),
                faces: faces.map(Chunked::from_vec),
            },
        )
    }

    /// This mesh with `palette` and some faces' own materials set (others
    /// unchanged; storage shared where untouched). The palette must extend
    /// the current one (same entries first).
    pub fn with_face_materials(&self, palette: Vec<Material>, set: &[(FaceId, u32)]) -> Mesh {
        let i = &self.inner;
        let slots = i.faces.len();
        let mut faces = match &i.materials.faces {
            Some(f) => f.clone(),
            None => Chunked::from_vec(vec![NO_MATERIAL; slots]),
        };
        while faces.len() < slots {
            faces.push(NO_MATERIAL);
        }
        for &(f, m) in set {
            faces.set(f.0 as usize, m);
        }
        Mesh::from_parts_with(
            i.positions.clone(),
            i.faces.clone(),
            i.face_live.clone(),
            i.vertex_live.clone(),
            i.face_count,
            i.vertex_count,
            MaterialData {
                palette: Arc::new(palette),
                default: i.materials.default,
                faces: Some(faces),
            },
        )
    }

    /// True if both handles point at the same revision.
    pub fn ptr_eq(a: &Mesh, b: &Mesh) -> bool {
        Arc::ptr_eq(&a.inner, &b.inner)
    }
}
