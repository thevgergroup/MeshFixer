//! The exact minimum distance between two meshes' surfaces (owner
//! requirements 2026-11-11, the fit check: "measure the actual gap
//! between the mating surfaces exactly, using the BVH minimum distance").
//! Used by the connector fit check and by a connector's own "leaves never
//! touch" tests.
//!
//! Exact point-triangle distance (no sampling), with `b`'s bounding box
//! used to prune `a`'s BVH to the faces that could be closest (a single
//! box query at the current best distance, shrunk as closer pairs are
//! found). Deterministic; cancellable.

use glam::{DMat4, DVec3};
use mf_runtime::{Cancelled, RunContext};

use crate::bvh::Bvh;
use crate::ids::FaceId;
use crate::mesh::Mesh;

fn corners(m: &Mesh, f: FaceId) -> Option<[DVec3; 3]> {
    m.face(f)
        .map(|t| t.map(|v| DVec3::from(m.corner_position(v))))
}

/// The closest point on triangle `(a, b, c)` to `p`, and the squared
/// distance to it (Ericson, *Real-Time Collision Detection* 5.1.5).
fn closest_point_on_triangle(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> (DVec3, f64) {
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return (a, (p - a).length_squared());
    }
    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return (b, (p - b).length_squared());
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        let q = a + v * ab;
        return (q, (p - q).length_squared());
    }
    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return (c, (p - c).length_squared());
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        let q = a + w * ac;
        return (q, (p - q).length_squared());
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        let q = b + w * (c - b);
        return (q, (p - q).length_squared());
    }
    let denom = 1.0 / (va + vb + vc);
    let v = vb * denom;
    let w = vc * denom;
    let q = a + ab * v + ac * w;
    (q, (p - q).length_squared())
}

/// The minimum squared distance between two triangles, by checking every
/// vertex of each against the other's plane-closest point (a correct,
/// if not maximally fast, exact test: it covers vertex-vertex, vertex-
/// edge, vertex-face and edge-edge cases because each edge's endpoints
/// are vertices of a triangle being tested against the other).
fn triangle_distance_squared(a: [DVec3; 3], b: [DVec3; 3]) -> f64 {
    let mut best = f64::INFINITY;
    for &p in &a {
        let (_, d2) = closest_point_on_triangle(p, b[0], b[1], b[2]);
        best = best.min(d2);
    }
    for &p in &b {
        let (_, d2) = closest_point_on_triangle(p, a[0], a[1], a[2]);
        best = best.min(d2);
    }
    best
}

/// The exact minimum distance between `a` and `b`'s surfaces (mm, in
/// `a`'s frame; `b` is placed by `b_to_a`). `a_bvh` is a BVH of `a`;
/// `b`'s faces are checked directly (connector parts are small, so no
/// BVH is needed on that side). Deterministic; cancellable; `None` if
/// either mesh has no live faces.
pub fn minimum_distance(
    a: &Mesh,
    a_bvh: &Bvh,
    b: &Mesh,
    b_to_a: &DMat4,
    ctx: &RunContext,
) -> Result<Option<f64>, Cancelled> {
    let b_faces: Vec<[DVec3; 3]> = b
        .faces()
        .filter_map(|(f, _)| corners(b, f))
        .map(|c| c.map(|p| b_to_a.transform_point3(p)))
        .collect();
    if b_faces.is_empty() {
        return Ok(None);
    }
    let mut best = f64::INFINITY;
    let mut tick = ctx.ticker();
    // A generous box around each b-triangle, widened by the current best
    // distance, prunes which of a's faces are worth an exact test.
    for cb in &b_faces {
        tick.tick()?;
        let pad = best.sqrt();
        let pad = if pad.is_finite() { pad } else { f64::MAX / 4.0 };
        let lo = [0, 1, 2].map(|k| cb[0][k].min(cb[1][k]).min(cb[2][k]) - pad);
        let hi = [0, 1, 2].map(|k| cb[0][k].max(cb[1][k]).max(cb[2][k]) + pad);
        a_bvh.for_each_overlapping(lo, hi, |fa| {
            if let Some(ca) = corners(a, fa) {
                let d2 = triangle_distance_squared(ca, *cb);
                best = best.min(d2);
            }
        });
    }
    if best.is_finite() {
        Ok(Some(best.sqrt()))
    } else {
        Ok(None)
    }
}
