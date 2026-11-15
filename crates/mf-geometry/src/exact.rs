//! Exact geometric decisions (D2, ADR-0011): Shewchuk's adaptive
//! `orient2d`/`orient3d`, through `robust`. Every decision is exact;
//! nothing uses a tolerance.
//!
//! This module is **the only place** MeshFixer decides whether a point is
//! on a plane ([`on_plane`], [`side_of`]), whether two points are the same
//! ([`same_point`]) or whether a point is inside a solid
//! ([`point_in_solid`]). Code elsewhere that needs one of those answers
//! calls these, never a distance against an epsilon (tolerances that
//! remain are listed, with a reason each, in `docs/tolerances.allow`).
//! The rest is shared by the output validator ([`crate::validate_solid`])
//! and intersection analysis.

use robust::{Coord, Coord3D, orient2d as orient2d_robust, orient3d as orient3d_robust};

fn c3(p: [f64; 3]) -> Coord3D<f64> {
    Coord3D {
        x: p[0],
        y: p[1],
        z: p[2],
    }
}

/// `orient3d(a, b, c, d)`: positive, negative or exactly zero.
pub(crate) fn o3(a: [f64; 3], b: [f64; 3], c: [f64; 3], d: [f64; 3]) -> f64 {
    orient3d_robust(c3(a), c3(b), c3(c), c3(d))
}

/// `orient3d(a, b, c, d)`, exactly: positive when `d` lies below the
/// plane through `a`, `b`, `c` (counter-clockwise seen from above, the
/// side `(b − a) × (c − a)` points to), negative above, zero on it.
pub fn orient3d(a: [f64; 3], b: [f64; 3], c: [f64; 3], d: [f64; 3]) -> f64 {
    o3(a, b, c, d)
}

/// `orient2d(a, b, c)`, exactly: positive when `a`, `b`, `c` turn
/// counter-clockwise, negative clockwise, zero when collinear.
pub fn orient2d(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    let c2 = |p: [f64; 2]| Coord { x: p[0], y: p[1] };
    orient2d_robust(c2(a), c2(b), c2(c))
}

/// Which side of a plane a point is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The side the plane's normal `(b − a) × (c − a)` points to.
    Positive,
    /// Exactly on the plane.
    On,
    /// The other side.
    Negative,
}

/// The side of the plane through `plane`'s three points (not collinear)
/// that `p` is on, exactly.
pub fn side_of(p: [f64; 3], plane: [[f64; 3]; 3]) -> Side {
    let o = o3(plane[0], plane[1], plane[2], p);
    if o < 0.0 {
        Side::Positive
    } else if o > 0.0 {
        Side::Negative
    } else {
        Side::On
    }
}

/// Whether `p` lies exactly on the plane through `plane`'s three points.
/// The plane is given by points, never by a rounded normal and offset.
pub fn on_plane(p: [f64; 3], plane: [[f64; 3]; 3]) -> bool {
    side_of(p, plane) == Side::On
}

/// Whether `a` and `b` are the same point: equal coordinates (`0.0` and
/// `-0.0` are equal). Points an ulp apart are different points.
pub fn same_point(a: [f64; 3], b: [f64; 3]) -> bool {
    a == b
}

/// Where a point is relative to a closed solid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Containment {
    Inside,
    Outside,
    /// Exactly on one of its faces (an edge or a corner included).
    OnSurface,
}

/// Where `p` is relative to `mesh` (closed, consistently oriented), by
/// the exact parity of a probe segment's crossings with its faces. A
/// probe that meets an edge or a vertex exactly decides nothing and the
/// next direction is tried; `None` when every probe did (no real input
/// does: each direction is a measure-zero coincidence).
pub fn point_in_solid(mesh: &crate::Mesh, p: [f64; 3]) -> Option<Containment> {
    let bb = mesh.bounding_box()?;
    let reach = (0..3)
        .map(|a| {
            (bb.max[a] - bb.min[a]).abs() + (p[a] - bb.min[a]).abs() + (p[a] - bb.max[a]).abs()
        })
        .fold(1.0, f64::max)
        * 2.0;
    let tris: Vec<[[f64; 3]; 3]> = mesh
        .faces()
        .map(|(_, t)| t.map(|v| mesh.corner_position(v)))
        .collect();
    // On a face: decided once, whichever probe.
    for t in &tris {
        if o3(t[0], t[1], t[2], p) == 0.0
            && let Some(k) = drop_axis(t[0], t[1], t[2])
            && point_in_tri_2d(p, *t, k)
        {
            return Some(Containment::OnSurface);
        }
    }
    // Directions with no simple relation to the axes or each other (not
    // unit length: only the segment matters).
    const PROBES: [[f64; 3]; 8] = [
        [0.5773, 0.6123, 0.5401],
        [-0.6929, 0.3162, 0.6478],
        [0.2673, -0.8018, 0.5345],
        [0.8165, 0.4082, -0.4082],
        [-0.3015, -0.9045, -0.3015],
        [0.1104, 0.4417, -0.8903],
        [-0.6396, 0.6396, -0.4264],
        [0.9487, -0.1054, 0.2981],
    ];
    'probe: for d in PROBES {
        let q = [0, 1, 2].map(|a| p[a] + d[a] * reach);
        let mut crossings = 0usize;
        for t in &tris {
            let [a, b, c] = *t;
            let (sp, sq) = (o3(a, b, c, p), o3(a, b, c, q));
            if sp == 0.0 && sq == 0.0 {
                continue 'probe; // the probe lies in the face's plane
            }
            if sp == 0.0 || sq == 0.0 || (sp > 0.0) == (sq > 0.0) {
                // Touches the plane only at an end (`p` isn't on the face,
                // `q` is outside the bounds), or doesn't reach it.
                continue;
            }
            let e = [o3(p, q, a, b), o3(p, q, b, c), o3(p, q, c, a)];
            if e.contains(&0.0) {
                continue 'probe; // through an edge or a vertex exactly
            }
            if e.iter().all(|&x| x > 0.0) || e.iter().all(|&x| x < 0.0) {
                crossings += 1;
            }
        }
        return Some(if crossings % 2 == 1 {
            Containment::Inside
        } else {
            Containment::Outside
        });
    }
    None
}

/// `p` with axis `drop` removed.
pub(crate) fn proj(p: [f64; 3], drop: usize) -> Coord<f64> {
    let (x, y) = match drop {
        0 => (p[1], p[2]),
        1 => (p[2], p[0]),
        _ => (p[0], p[1]),
    };
    Coord { x, y }
}

pub(crate) fn o2(a: [f64; 3], b: [f64; 3], c: [f64; 3], drop: usize) -> f64 {
    orient2d_robust(proj(a, drop), proj(b, drop), proj(c, drop))
}

/// The axis to drop so the triangle's projection keeps the most area
/// (exact orientations, so a non-degenerate triangle never projects to a
/// segment). `None` for a degenerate (zero-area) triangle.
pub(crate) fn drop_axis(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> Option<usize> {
    let mut best = None;
    let mut best_abs = 0.0;
    for k in 0..3 {
        let d = o2(a, b, c, k).abs();
        if d > best_abs {
            best_abs = d;
            best = Some(k);
        }
    }
    best
}

/// Whether a triangle has zero area (a repeated vertex, or collinear
/// corners), exactly.
pub(crate) fn degenerate(idx: [u32; 3], c: [[f64; 3]; 3]) -> bool {
    idx[0] == idx[1]
        || idx[1] == idx[2]
        || idx[0] == idx[2]
        || drop_axis(c[0], c[1], c[2]).is_none()
}

/// Whether faces `a` and `b` (vertex indices `ia`, `ib`, corners `ca`, `cb`,
/// both non-degenerate) meet anywhere other than at what they share by
/// vertex index. Touching at a point or along a segment they don't share
/// counts.
pub(crate) fn faces_meet(ia: [u32; 3], ca: [[f64; 3]; 3], ib: [u32; 3], cb: [[f64; 3]; 3]) -> bool {
    // The corners they share by index, without allocating (this runs for
    // every pair of nearby faces a check meets).
    let mut shared = [(0usize, 0usize); 9];
    let mut count = 0;
    for (i, a) in ia.iter().enumerate() {
        for (j, b) in ib.iter().enumerate() {
            if a == b {
                shared[count] = (i, j);
                count += 1;
            }
        }
    }
    let shared = &shared[..count];
    match shared.len() {
        0 => {
            // Every corner of one strictly on one side of the other's
            // plane: they can't meet (exact, so the same answer tri_tri
            // would give).
            let side = |t: [[f64; 3]; 3], u: [[f64; 3]; 3]| {
                let s = u.map(|p| o3(t[0], t[1], t[2], p));
                s.iter().all(|&x| x > 0.0) || s.iter().all(|&x| x < 0.0)
            };
            if side(ca, cb) || side(cb, ca) {
                return false;
            }
            tri_tri(ca, cb)
        }
        1 => {
            let (i, j) = shared[0];
            shared_vertex(rot(ca, i), rot(cb, j))
        }
        2 => {
            let i = (0..3).find(|i| shared.iter().all(|s| s.0 != *i));
            let j = (0..3).find(|j| shared.iter().all(|s| s.1 != *j));
            match (i, j) {
                (Some(i), Some(j)) => shared_edge(ca[shared[0].0], ca[shared[1].0], ca[i], cb[j]),
                _ => true,
            }
        }
        _ => true,
    }
}

fn rot(c: [[f64; 3]; 3], i: usize) -> [[f64; 3]; 3] {
    [c[i], c[(i + 1) % 3], c[(i + 2) % 3]]
}

/// Closed triangles: they intersect exactly when an edge of one meets the
/// other.
pub(crate) fn tri_tri(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> bool {
    (0..3).any(|i| seg_tri(a[i], a[(i + 1) % 3], b))
        || (0..3).any(|i| seg_tri(b[i], b[(i + 1) % 3], a))
}

/// Closed segment `pq` against closed triangle `t`.
pub(crate) fn seg_tri(p: [f64; 3], q: [f64; 3], t: [[f64; 3]; 3]) -> bool {
    let [a, b, c] = t;
    let s1 = o3(a, b, c, p);
    let s2 = o3(a, b, c, q);
    if (s1 > 0.0 && s2 > 0.0) || (s1 < 0.0 && s2 < 0.0) {
        return false;
    }
    if s1 == 0.0 && s2 == 0.0 {
        let Some(k) = drop_axis(a, b, c) else {
            return true;
        };
        return seg_tri_2d(p, q, t, k);
    }
    let t1 = o3(p, q, a, b);
    let t2 = o3(p, q, b, c);
    let t3 = o3(p, q, c, a);
    (t1 >= 0.0 && t2 >= 0.0 && t3 >= 0.0) || (t1 <= 0.0 && t2 <= 0.0 && t3 <= 0.0)
}

pub(crate) fn point_in_tri_2d(p: [f64; 3], t: [[f64; 3]; 3], k: usize) -> bool {
    let [a, b, c] = t;
    let (d1, d2, d3) = (o2(a, b, p, k), o2(b, c, p, k), o2(c, a, p, k));
    (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0)
}

fn on_segment_2d(p: [f64; 3], q: [f64; 3], r: [f64; 3], k: usize) -> bool {
    let (p, q, r) = (proj(p, k), proj(q, k), proj(r, k));
    p.x.min(q.x) <= r.x && r.x <= p.x.max(q.x) && p.y.min(q.y) <= r.y && r.y <= p.y.max(q.y)
}

fn seg_seg_2d(p: [f64; 3], q: [f64; 3], r: [f64; 3], s: [f64; 3], k: usize) -> bool {
    let d1 = o2(p, q, r, k);
    let d2 = o2(p, q, s, k);
    let d3 = o2(r, s, p, k);
    let d4 = o2(r, s, q, k);
    let opposite = |x: f64, y: f64| (x > 0.0 && y < 0.0) || (x < 0.0 && y > 0.0);
    if opposite(d1, d2) && opposite(d3, d4) {
        return true;
    }
    (d1 == 0.0 && on_segment_2d(p, q, r, k))
        || (d2 == 0.0 && on_segment_2d(p, q, s, k))
        || (d3 == 0.0 && on_segment_2d(r, s, p, k))
        || (d4 == 0.0 && on_segment_2d(r, s, q, k))
}

fn seg_tri_2d(p: [f64; 3], q: [f64; 3], t: [[f64; 3]; 3], k: usize) -> bool {
    point_in_tri_2d(p, t, k)
        || point_in_tri_2d(q, t, k)
        || (0..3).any(|i| seg_seg_2d(p, q, t[i], t[(i + 1) % 3], k))
}

/// Faces sharing exactly vertex `a[0] == b[0]`.
fn shared_vertex(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> bool {
    if seg_tri(a[1], a[2], b) || seg_tri(b[1], b[2], a) {
        return true;
    }
    edge_into(a, b) || edge_into(b, a)
}

fn edge_into(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> bool {
    let [v, b1, b2] = b;
    let Some(k) = drop_axis(v, b1, b2) else {
        return true;
    };
    let s = o2(v, b1, b2, k).signum();
    a[1..].iter().any(|&x| {
        o3(v, b1, b2, x) == 0.0 && s * o2(v, b1, x, k) >= 0.0 && s * o2(v, x, b2, k) >= 0.0
    })
}

/// Faces `(u, w, x)` and `(u, w, y)` sharing edge `uw`: they overlap only
/// when coplanar with `x` and `y` on the same side of the edge (a fold).
fn shared_edge(u: [f64; 3], w: [f64; 3], x: [f64; 3], y: [f64; 3]) -> bool {
    if o3(u, w, x, y) != 0.0 {
        return false;
    }
    let Some(k) = drop_axis(u, w, x) else {
        return true;
    };
    let (sx, sy) = (o2(u, w, x, k), o2(u, w, y, k));
    (sx > 0.0 && sy > 0.0) || (sx < 0.0 && sy < 0.0)
}
