//! Which faces of a revision lie near a point, for brushes. Built once per
//! revision and shared by every stroke on it.
//!
//! It wraps the face BVH (D17, `mf_geometry::Bvh`): a box query of the
//! point ± the radius finds every face with a corner within the radius
//! (the BVH's bounds are rounded outward). Sculpting and picking can share
//! the one BVH a revision already has: see [`SculptIndex::from_bvh`].

use std::sync::Arc;

use mf_geometry::{Bvh, FaceId, Mesh};
use mf_runtime::{Cancelled, RunContext};

/// The faces of one revision, for "faces near this point" queries.
#[derive(Clone)]
pub struct SculptIndex {
    bvh: Arc<Bvh>,
}

impl SculptIndex {
    /// Builds the revision's BVH (O(n log n), parallel, deterministic).
    pub fn build(mesh: &Mesh, ctx: &RunContext) -> Result<SculptIndex, Cancelled> {
        Ok(SculptIndex {
            bvh: Arc::new(Bvh::build(mesh, ctx)?),
        })
    }

    /// Uses a BVH already built for this revision (picking's).
    pub fn from_bvh(bvh: Arc<Bvh>) -> SculptIndex {
        SculptIndex { bvh }
    }

    /// Extra reach to add to a corner-distance query. The BVH bounds the
    /// faces themselves, so none is needed.
    pub fn face_radius(&self) -> f64 {
        0.0
    }

    /// Appends to `out`, in ascending face id order, every indexed face
    /// that may have a point within `radius` of `p` (a superset: boxes).
    pub fn faces_near(&self, p: [f64; 3], radius: f64, out: &mut Vec<FaceId>) {
        let start = out.len();
        let lo = std::array::from_fn(|k| p[k] - radius);
        let hi = std::array::from_fn(|k| p[k] + radius);
        self.bvh.for_each_overlapping(lo, hi, |f| out.push(f));
        out[start..].sort_unstable();
    }

    pub fn heap_bytes(&self) -> u64 {
        self.bvh.heap_bytes()
    }
}
