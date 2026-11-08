use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use serde::Serialize;

use crate::chunked::{Bits, Chunked};
use crate::edit::MeshEdit;
use crate::hash::ContentHash;
use crate::ids::{FaceId, VertexId};
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compaction {
    pub faces: Vec<Option<FaceId>>,
    pub vertices: Vec<Option<VertexId>>,
}

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
    hash: OnceLock<ContentHash>,
    topology: OnceLock<Arc<Topology>>,
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
        Self::check_counts(positions.len(), triangles.len())?;
        if let Some(vertex) = positions
            .iter()
            .position(|p| !p.iter().all(|c| c.is_finite()))
        {
            return Err(GeometryError::NonFinite { vertex });
        }
        let n = positions.len();
        for (triangle, t) in triangles.iter().enumerate() {
            if let Some(&index) = t.iter().find(|&&i| i as usize >= n) {
                return Err(GeometryError::IndexOutOfRange { triangle, index });
            }
        }
        let (face_count, vertex_count) = (triangles.len(), positions.len());
        Ok(Mesh::from_parts(
            Chunked::from_vec(positions),
            Chunked::from_vec(triangles),
            Bits::all_set(face_count),
            Bits::all_set(vertex_count),
            face_count,
            vertex_count,
        ))
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
                topology: OnceLock::new(),
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
        let mut it = self.vertices().map(|(_, p)| p);
        let first = it.next()?;
        let mut bb = Aabb {
            min: first,
            max: first,
        };
        for p in it {
            for (i, &c) in p.iter().enumerate() {
                bb.min[i] = bb.min[i].min(c);
                bb.max[i] = bb.max[i].max(c);
            }
        }
        Some(bb)
    }

    /// Canonical content hash (MF-DOC-003), cached. Covers live elements
    /// *with their slot IDs*: slot identity is part of content identity.
    pub fn content_hash(&self) -> ContentHash {
        *self.inner.hash.get_or_init(|| {
            let mut h = blake3::Hasher::new();
            h.update(b"mf-mesh-v2");
            h.update(&(self.vertex_count() as u64).to_le_bytes());
            for (id, p) in self.vertices() {
                h.update(&id.0.to_le_bytes());
                for c in p {
                    h.update(&c.to_le_bytes());
                }
            }
            h.update(&(self.face_count() as u64).to_le_bytes());
            for (id, t) in self.faces() {
                h.update(&id.0.to_le_bytes());
                for i in t {
                    h.update(&i.to_le_bytes());
                }
            }
            ContentHash(*h.finalize().as_bytes())
        })
    }

    /// Derived topology, computed on first use and cached (MF-GEO-003).
    pub fn topology(&self) -> Arc<Topology> {
        self.inner
            .topology
            .get_or_init(|| Arc::new(Topology::compute(self)))
            .clone()
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

    /// True if both handles point at the same revision.
    pub fn ptr_eq(a: &Mesh, b: &Mesh) -> bool {
        Arc::ptr_eq(&a.inner, &b.inner)
    }
}
