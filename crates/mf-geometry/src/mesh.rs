use std::sync::{Arc, OnceLock};

use serde::Serialize;

use crate::hash::ContentHash;
use crate::topology::Topology;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GeometryError {
    #[error("triangle {triangle} references vertex {index}, which does not exist")]
    IndexOutOfRange { triangle: usize, index: u32 },
    #[error("vertex {vertex} has a non-finite coordinate")]
    NonFinite { vertex: usize },
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

/// An immutable indexed triangle mesh (working representation, MF-GEO-001).
///
/// Cloning is cheap: buffers and derived caches are shared behind an `Arc`.
/// A `Mesh` is never mutated after construction (MF-DOC-001).
#[derive(Clone)]
pub struct Mesh {
    inner: Arc<Inner>,
}

struct Inner {
    positions: Vec<[f64; 3]>,
    triangles: Vec<[u32; 3]>,
    hash: OnceLock<ContentHash>,
    topology: OnceLock<Arc<Topology>>,
}

impl std::fmt::Debug for Mesh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mesh")
            .field("vertices", &self.vertex_count())
            .field("triangles", &self.triangle_count())
            .finish()
    }
}

impl Mesh {
    pub fn new(positions: Vec<[f64; 3]>, triangles: Vec<[u32; 3]>) -> Result<Mesh, GeometryError> {
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
        Ok(Mesh {
            inner: Arc::new(Inner {
                positions,
                triangles,
                hash: OnceLock::new(),
                topology: OnceLock::new(),
            }),
        })
    }

    pub fn empty() -> Mesh {
        Mesh::new(Vec::new(), Vec::new()).expect("empty mesh is valid")
    }

    pub fn positions(&self) -> &[[f64; 3]] {
        &self.inner.positions
    }

    pub fn triangles(&self) -> &[[u32; 3]] {
        &self.inner.triangles
    }

    pub fn vertex_count(&self) -> usize {
        self.inner.positions.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.inner.triangles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.triangles.is_empty()
    }

    pub fn bounding_box(&self) -> Option<Aabb> {
        let first = *self.inner.positions.first()?;
        let mut bb = Aabb {
            min: first,
            max: first,
        };
        for p in &self.inner.positions {
            for (i, &c) in p.iter().enumerate() {
                bb.min[i] = bb.min[i].min(c);
                bb.max[i] = bb.max[i].max(c);
            }
        }
        Some(bb)
    }

    /// Canonical content hash (MF-DOC-003), computed once and cached.
    pub fn content_hash(&self) -> ContentHash {
        *self.inner.hash.get_or_init(|| {
            let mut h = blake3::Hasher::new();
            h.update(b"mf-mesh-v1");
            h.update(&(self.inner.positions.len() as u64).to_le_bytes());
            for p in &self.inner.positions {
                for c in p {
                    h.update(&c.to_le_bytes());
                }
            }
            h.update(&(self.inner.triangles.len() as u64).to_le_bytes());
            for t in &self.inner.triangles {
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
            .get_or_init(|| Arc::new(Topology::compute(self.vertex_count(), self.triangles())))
            .clone()
    }

    /// Heap bytes owned by the mesh buffers (excludes cached topology).
    pub fn heap_bytes(&self) -> u64 {
        (std::mem::size_of::<Inner>()
            + self.inner.positions.capacity() * std::mem::size_of::<[f64; 3]>()
            + self.inner.triangles.capacity() * std::mem::size_of::<[u32; 3]>()) as u64
    }

    /// True if both handles point at the same underlying storage.
    pub fn ptr_eq(a: &Mesh, b: &Mesh) -> bool {
        Arc::ptr_eq(&a.inner, &b.inner)
    }
}
