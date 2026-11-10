//! A spatial index of a revision's faces for brushes: which faces lie
//! near a point. Built once per revision (O(n), parallel, deterministic)
//! and shared by every stroke on it.
//!
//! This is the sculpt engine's own uniform grid over face centroids. The
//! plan names the face BVH (D17) here; it moves into `mf-geometry` in
//! Task 8 (the other stream), and a sphere query on it can replace this
//! index once both land.

use mf_geometry::{FaceId, Mesh};
use mf_runtime::{Cancelled, RunContext, par_sort_unstable_by_key};
use rayon::prelude::*;

const BITS: u32 = 21;
const MAX_CELL: u64 = (1 << BITS) - 1;

/// Faces of one revision bucketed by centroid in a uniform grid.
pub struct SculptIndex {
    origin: [f64; 3],
    cell: f64,
    /// (cell key, face), sorted.
    entries: Vec<(u64, u32)>,
    /// The farthest any face's corner is from its centroid: a face with a
    /// corner within `d` of a point has its centroid within
    /// `d + face_radius`.
    face_radius: f64,
}

fn centroid(mesh: &Mesh, t: [u32; 3]) -> [f64; 3] {
    let [a, b, c] = t.map(|v| mesh.corner_position(v));
    std::array::from_fn(|k| (a[k] + b[k] + c[k]) / 3.0)
}

impl SculptIndex {
    pub fn build(mesh: &Mesh, ctx: &RunContext) -> Result<SculptIndex, Cancelled> {
        let bb = mesh.bounding_box_with(ctx)?;
        let (origin, diag) = bb.map_or(([0.0; 3], 1.0), |b| (b.min, b.diagonal()));
        let cell = (diag / 256.0).max(f64::MIN_POSITIVE);
        let mut index = SculptIndex {
            origin,
            cell,
            entries: Vec::new(),
            face_radius: 0.0,
        };
        let chunks = mesh.face_chunk_count();
        let parts: Vec<(Vec<(u64, u32)>, f64)> = (0..chunks)
            .into_par_iter()
            .map(|i| -> Result<_, Cancelled> {
                ctx.checkpoint()?;
                let mut out = Vec::new();
                let mut radius = 0.0_f64;
                for (f, t) in mesh.faces_in_chunk(i) {
                    let c = centroid(mesh, t);
                    for v in t {
                        let p = mesh.corner_position(v);
                        let d2: f64 = (0..3).map(|k| (p[k] - c[k]).powi(2)).sum();
                        radius = radius.max(d2.sqrt());
                    }
                    out.push((index.key(index.cell_of(c)), f.0));
                }
                Ok((out, radius))
            })
            .collect::<Result<_, _>>()?;
        let mut entries = Vec::with_capacity(mesh.face_count());
        for (part, radius) in parts {
            entries.extend(part);
            index.face_radius = index.face_radius.max(radius);
        }
        par_sort_unstable_by_key(&mut entries, ctx, |&e| e)?;
        index.entries = entries;
        Ok(index)
    }

    fn cell_of(&self, p: [f64; 3]) -> [u64; 3] {
        std::array::from_fn(|k| {
            let x = ((p[k] - self.origin[k]) / self.cell).floor();
            // Clamped to the grid: every indexed centroid is inside it.
            if x.is_nan() || x <= 0.0 {
                0
            } else {
                (x.min(MAX_CELL as f64) as u64).min(MAX_CELL)
            }
        })
    }

    fn key(&self, c: [u64; 3]) -> u64 {
        (c[0] << (2 * BITS)) | (c[1] << BITS) | c[2]
    }

    /// The farthest a corner is from its face's centroid.
    pub fn face_radius(&self) -> f64 {
        self.face_radius
    }

    /// Appends to `out`, in ascending face id order, every indexed face
    /// whose centroid may be within `radius` of `p` (a superset: whole
    /// grid cells).
    pub fn faces_near(&self, p: [f64; 3], radius: f64, out: &mut Vec<FaceId>) {
        let lo = self.cell_of(std::array::from_fn(|k| p[k] - radius));
        let hi = self.cell_of(std::array::from_fn(|k| p[k] + radius));
        let start = out.len();
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                let (a, b) = (self.key([x, y, lo[2]]), self.key([x, y, hi[2]]));
                let from = self.entries.partition_point(|e| e.0 < a);
                for e in &self.entries[from..] {
                    if e.0 > b {
                        break;
                    }
                    out.push(FaceId(e.1));
                }
            }
        }
        out[start..].sort_unstable();
    }

    pub fn heap_bytes(&self) -> u64 {
        (self.entries.capacity() * std::mem::size_of::<(u64, u32)>()) as u64
    }
}
