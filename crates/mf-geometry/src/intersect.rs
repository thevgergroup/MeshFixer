//! Exact intersection analysis (MF-MODEL-002, Phase 1 Task 9): where two
//! objects, or an object and itself, intersect.
//!
//! - [`tri_tri_intersect`] decides exactly (Shewchuk's predicates, D2)
//!   whether two triangles are disjoint, only touch (they meet, but no
//!   point is inside both), or cross (their interiors meet). Non-coplanar
//!   pairs use Guigue and Devillers' interval test; coplanar pairs a
//!   separating-edge test with `orient2d`.
//! - [`intersections_between`] finds every crossing pair of faces between
//!   two meshes through their [`Bvh`]s, the second placed in the first's
//!   frame by an affine transform (applied exactly as the boolean applies
//!   it, so detection and the boolean agree), and groups them into
//!   regions.
//! - [`self_intersections`] does the same within one mesh, skipping pairs
//!   that share a vertex unless they meet beyond what they share.
//!
//! Segments are f64 constructions, for display only; no decision uses them.

use std::collections::HashMap;

use glam::DMat4;
use mf_runtime::{Cancelled, RunContext};
use rayon::prelude::*;

use crate::affine::{apply, from_dmat4, inverse};
use crate::exact::{faces_meet, o2, o3, tri_tri};
use crate::{Bvh, FaceId, Mesh};

/// How two closed triangles meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriTri {
    Disjoint,
    /// They meet, but only on the boundary of one or both (a shared or
    /// touching vertex or edge): not counted as an intersection.
    Touching,
    /// Their interiors meet.
    Crossing,
}

/// Exactly how triangles `a` and `b` meet (see [`TriTri`]). Degenerate
/// triangles have no interior: they are at most `Touching`.
pub fn tri_tri_intersect(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> TriTri {
    if !tri_tri(a, b) {
        return TriTri::Disjoint;
    }
    if crate::exact::drop_axis(a[0], a[1], a[2]).is_none()
        || crate::exact::drop_axis(b[0], b[1], b[2]).is_none()
    {
        return TriTri::Touching;
    }
    let sa = [0, 1, 2].map(|i| sign(o3(b[0], b[1], b[2], a[i])));
    let sb = [0, 1, 2].map(|i| sign(o3(a[0], a[1], a[2], b[i])));
    if sa == [0, 0, 0] {
        return if coplanar_interiors_meet(a, b) {
            TriTri::Crossing
        } else {
            TriTri::Touching
        };
    }
    // An interior point of both lies on both planes: each triangle must
    // have corners strictly on both sides of the other's plane.
    let straddles = |s: [i8; 3]| s.contains(&1) && s.contains(&-1);
    if !(straddles(sa) && straddles(sb)) {
        return TriTri::Touching;
    }
    if intervals_overlap_strictly(a, sa, b, sb) {
        TriTri::Crossing
    } else {
        TriTri::Touching
    }
}

fn sign(x: f64) -> i8 {
    if x > 0.0 {
        1
    } else if x < 0.0 {
        -1
    } else {
        0
    }
}

/// Coplanar: interiors meet unless an edge line of one triangle has the
/// other on its outer (closed) side.
fn coplanar_interiors_meet(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> bool {
    let Some(k) = crate::exact::drop_axis(a[0], a[1], a[2]) else {
        return false;
    };
    let separates = |t: [[f64; 3]; 3], other: [[f64; 3]; 3]| {
        let s = sign(o2(t[0], t[1], t[2], k));
        (0..3).any(|i| {
            let (p, q) = (t[i], t[(i + 1) % 3]);
            other.iter().all(|&x| f64::from(s) * o2(p, q, x, k) <= 0.0)
        })
    };
    !(separates(a, b) || separates(b, a))
}

/// Both triangles straddle each other's plane strictly, so each meets the
/// planes' common line in a segment of positive length through its
/// interior. Whether the two segments overlap in more than a point
/// (Guigue–Devillers' `CHECK_MIN_MAX`, with strict comparisons).
fn intervals_overlap_strictly(
    a: [[f64; 3]; 3],
    sa: [i8; 3],
    b: [[f64; 3]; 3],
    sb: [i8; 3],
) -> bool {
    // Rotate a so a[0] is alone on its side of b's plane (the other two
    // are on the opposite closed side), and b likewise.
    let (a, sa) = alone_first(a, sa);
    let (mut b, sb) = alone_first(b, sb);
    // The reference's orientation convention: when the lone corner of one
    // triangle is on the positive side of the other's plane (Shewchuk's
    // sign), the other triangle's last two corners swap.
    if sa[0] > 0 {
        b.swap(1, 2);
    }
    let a = if sb[0] > 0 { [a[0], a[2], a[1]] } else { a };
    let [p1, q1, r1] = a;
    let [p2, q2, r2] = b;
    o3(p2, p1, q2, q1) < 0.0 && o3(p2, r1, r2, p1) < 0.0
}

/// The triangle rotated so its first corner is the one alone on its side
/// (strictly), with its signs.
fn alone_first(t: [[f64; 3]; 3], s: [i8; 3]) -> ([[f64; 3]; 3], [i8; 3]) {
    let alone = (0..3)
        .find(|&i| s[i] != 0 && s.iter().enumerate().all(|(j, &x)| j == i || x != s[i]))
        .unwrap_or(0); // allow-swallow: a straddling triangle always has a corner alone on its side
    (
        [t[alone], t[(alone + 1) % 3], t[(alone + 2) % 3]],
        [s[alone], s[(alone + 1) % 3], s[(alone + 2) % 3]],
    )
}

/// A connected group of crossing faces.
#[derive(Debug, Clone, PartialEq)]
pub struct IntersectionRegion {
    /// Faces of the first mesh, sorted.
    pub faces_a: Vec<FaceId>,
    /// Faces of the second mesh (of the same mesh, for self-intersections),
    /// sorted.
    pub faces_b: Vec<FaceId>,
    /// The diagonal of the region's segments' bounds (first mesh's frame).
    pub extent: f64,
    pub centroid: [f64; 3],
    /// Where the faces cross (display only).
    pub segments: Vec<[[f64; 3]; 2]>,
}

/// Every crossing between two meshes, or within one.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IntersectionReport {
    /// Largest first (by face count), then by lowest face.
    pub regions: Vec<IntersectionRegion>,
    pub pair_count: u64,
    /// Pairs that only touch (between two meshes; for self-intersections,
    /// always 0: neighbouring faces touch by construction). Not counted as
    /// intersections, but the objects are in contact.
    pub touching_pairs: u64,
}

fn corners(m: &Mesh, f: FaceId) -> Option<[[f64; 3]; 3]> {
    m.face(f).map(|t| t.map(|v| m.corner_position(v)))
}

fn bounds(c: &[[f64; 3]; 3]) -> ([f64; 3], [f64; 3]) {
    let lo = [0, 1, 2].map(|k| c[0][k].min(c[1][k]).min(c[2][k]));
    let hi = [0, 1, 2].map(|k| c[0][k].max(c[1][k]).max(c[2][k]));
    (lo, hi)
}

/// Every crossing face pair between `a` and `b`, with `b` placed in `a`'s
/// frame by `b_to_a` (see the module docs). `a_bvh` and `b_bvh` are BVHs
/// of `a` and `b` (each in its own frame); the smaller mesh's faces are
/// queried against the larger one's BVH. Deterministic; cancellable.
pub fn intersections_between(
    a: &Mesh,
    a_bvh: &Bvh,
    b: &Mesh,
    b_bvh: &Bvh,
    b_to_a: &DMat4,
    ctx: &RunContext,
) -> Result<IntersectionReport, Cancelled> {
    let t = from_dmat4(b_to_a);
    let b_in_a = |f: FaceId| corners(b, f).map(|c| c.map(|p| apply(&t, p)));
    // (fa, fb, crossing): touching pairs are counted apart.
    let tagged: Vec<(FaceId, FaceId, bool)> = if b.face_count() <= a.face_count() {
        let faces: Vec<FaceId> = b.faces().map(|(f, _)| f).collect();
        par_pairs(&faces, ctx, |fb, out| {
            let Some(cb) = b_in_a(fb) else { return };
            let (lo, hi) = bounds(&cb);
            a_bvh.for_each_overlapping(lo, hi, |fa| {
                if let Some(ca) = corners(a, fa) {
                    match tri_tri_intersect(ca, cb) {
                        TriTri::Crossing => out.push((fa, fb, true)),
                        TriTri::Touching => out.push((fa, fb, false)),
                        TriTri::Disjoint => {}
                    }
                }
            });
        })?
    } else {
        let Some(a_to_b) = inverse(&t) else {
            return Ok(IntersectionReport::default());
        };
        let faces: Vec<FaceId> = a.faces().map(|(f, _)| f).collect();
        par_pairs(&faces, ctx, |fa, out| {
            let Some(ca) = corners(a, fa) else { return };
            let (lo, hi) = bounds(&ca.map(|p| apply(&a_to_b, p)));
            // Widen by rounding of the inverse: the exact test decides.
            let pad = (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max) * 1e-9 + 1e-300;
            b_bvh.for_each_overlapping(lo.map(|x| x - pad), hi.map(|x| x + pad), |fb| {
                if let Some(cb) = b_in_a(fb) {
                    match tri_tri_intersect(ca, cb) {
                        TriTri::Crossing => out.push((fa, fb, true)),
                        TriTri::Touching => out.push((fa, fb, false)),
                        TriTri::Disjoint => {}
                    }
                }
            });
        })?
    };
    let touching = tagged.iter().filter(|p| !p.2).count() as u64;
    let pairs = tagged
        .into_iter()
        .filter(|p| p.2)
        .map(|p| (p.0, p.1))
        .collect();
    let mut r = report(
        pairs,
        |fa| corners(a, fa),
        b_in_a,
        |f| a.face(f),
        |f| b.face(f),
    );
    r.touching_pairs = touching;
    Ok(r)
}

/// Every pair of faces of `a` that cross (see the module docs), through
/// `bvh` (built for `a`). Deterministic; cancellable.
pub fn self_intersections(
    a: &Mesh,
    bvh: &Bvh,
    ctx: &RunContext,
) -> Result<IntersectionReport, Cancelled> {
    let faces: Vec<FaceId> = a.faces().map(|(f, _)| f).collect();
    let pairs = par_pairs(&faces, ctx, |f, out| {
        let (Some(tf), Some(cf)) = (a.face(f), corners(a, f)) else {
            return;
        };
        let (lo, hi) = bounds(&cf);
        bvh.for_each_overlapping(lo, hi, |g| {
            if g <= f {
                return;
            }
            let (Some(tg), Some(cg)) = (a.face(g), corners(a, g)) else {
                return;
            };
            let shares = tf.iter().any(|v| tg.contains(v));
            let hit = if shares {
                !crate::exact::degenerate(tf, cf)
                    && !crate::exact::degenerate(tg, cg)
                    && faces_meet(tf, cf, tg, cg)
            } else {
                tri_tri_intersect(cf, cg) == TriTri::Crossing
            };
            if hit {
                out.push((f, g, true));
            }
        });
    })?;
    let pairs = pairs.into_iter().map(|p| (p.0, p.1)).collect();
    Ok(report(
        pairs,
        |f| corners(a, f),
        |g| corners(a, g),
        |f| a.face(f),
        |f| a.face(f),
    ))
}

/// Runs `find` over `faces` in parallel chunks, merging in order.
type Tagged = (FaceId, FaceId, bool);

/// A region being gathered: faces of each side, and segments.
type Group = (Vec<FaceId>, Vec<FaceId>, Vec<[[f64; 3]; 2]>);

fn par_pairs(
    faces: &[FaceId],
    ctx: &RunContext,
    find: impl Fn(FaceId, &mut Vec<Tagged>) + Sync,
) -> Result<Vec<Tagged>, Cancelled> {
    let mut pairs: Vec<Tagged> = faces
        .par_chunks(1 << 12)
        .map(|chunk| -> Result<Vec<Tagged>, Cancelled> {
            ctx.checkpoint()?;
            let mut out = Vec::new();
            for &f in chunk {
                find(f, &mut out);
            }
            Ok(out)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    pairs.sort_unstable();
    pairs.dedup();
    Ok(pairs)
}

/// Groups crossing pairs into regions: components of the graph whose
/// nodes are faces, joined by a crossing pair, or by sharing a vertex with
/// another crossing face of the same mesh (so a curve passing exactly
/// through a vertex stays one region).
fn report(
    pairs: Vec<(FaceId, FaceId)>,
    ca: impl Fn(FaceId) -> Option<[[f64; 3]; 3]>,
    cb: impl Fn(FaceId) -> Option<[[f64; 3]; 3]>,
    ta: impl Fn(FaceId) -> Option<[u32; 3]>,
    tb: impl Fn(FaceId) -> Option<[u32; 3]>,
) -> IntersectionReport {
    let mut node: HashMap<(bool, FaceId), usize> = HashMap::new();
    let mut parent: Vec<usize> = Vec::new();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let mut id = |key: (bool, FaceId), parent: &mut Vec<usize>| -> usize {
        *node.entry(key).or_insert_with(|| {
            parent.push(parent.len());
            parent.len() - 1
        })
    };
    for &(fa, fb) in &pairs {
        let (x, y) = (id((false, fa), &mut parent), id((true, fb), &mut parent));
        let (rx, ry) = (find(&mut parent, x), find(&mut parent, y));
        if rx != ry {
            let (lo, hi) = (rx.min(ry), rx.max(ry));
            parent[hi] = lo;
        }
    }
    // Same-mesh faces sharing a vertex.
    let keys: Vec<(bool, FaceId)> = node.keys().copied().collect();
    let mut by_vertex: HashMap<(bool, u32), usize> = HashMap::new();
    let mut sorted = keys;
    sorted.sort_unstable();
    for key in sorted {
        let tri = if key.0 { tb(key.1) } else { ta(key.1) };
        let x = node[&key];
        for v in tri.into_iter().flatten() {
            let first = *by_vertex.entry((key.0, v)).or_insert(x);
            let (rx, ry) = (find(&mut parent, x), find(&mut parent, first));
            if rx != ry {
                let (lo, hi) = (rx.min(ry), rx.max(ry));
                parent[hi] = lo;
            }
        }
    }
    let mut groups: HashMap<usize, Group> = HashMap::new();
    for &(fa, fb) in &pairs {
        let x = node[&(false, fa)];
        let r = find(&mut parent, x);
        let g = groups.entry(r).or_default();
        g.0.push(fa);
        g.1.push(fb);
        if let (Some(a), Some(b)) = (ca(fa), cb(fb))
            && let Some(s) = segment(a, b)
        {
            g.2.push(s);
        }
    }
    let mut regions: Vec<IntersectionRegion> = groups
        .into_values()
        .map(|(mut fa, mut fb, segments)| {
            fa.sort_unstable();
            fa.dedup();
            fb.sort_unstable();
            fb.dedup();
            let mut lo = [f64::INFINITY; 3];
            let mut hi = [f64::NEG_INFINITY; 3];
            let mut sum = [0.0; 3];
            for s in &segments {
                for p in s {
                    for k in 0..3 {
                        lo[k] = lo[k].min(p[k]);
                        hi[k] = hi[k].max(p[k]);
                        sum[k] += p[k];
                    }
                }
            }
            let n = (2 * segments.len()).max(1) as f64;
            let extent = if segments.is_empty() {
                0.0
            } else {
                ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2) + (hi[2] - lo[2]).powi(2)).sqrt()
            };
            IntersectionRegion {
                faces_a: fa,
                faces_b: fb,
                extent,
                centroid: sum.map(|x| x / n),
                segments,
            }
        })
        .collect();
    regions.sort_by(|x, y| {
        (y.faces_a.len() + y.faces_b.len())
            .cmp(&(x.faces_a.len() + x.faces_b.len()))
            .then(x.faces_a.cmp(&y.faces_a))
            .then(x.faces_b.cmp(&y.faces_b))
    });
    IntersectionReport {
        regions,
        pair_count: pairs.len() as u64,
        touching_pairs: 0,
    }
}

/// Where two crossing triangles meet (f64, display only): the overlap of
/// the segments each cuts from the other's plane. `None` if coplanar.
fn segment(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> Option<[[f64; 3]; 2]> {
    let cut = |t: [[f64; 3]; 3], plane: [[f64; 3]; 3]| -> Vec<[f64; 3]> {
        let n = normal(plane);
        let d = |p: [f64; 3]| dot(n, sub(p, plane[0]));
        let mut out = Vec::new();
        for i in 0..3 {
            let (p, q) = (t[i], t[(i + 1) % 3]);
            let (dp, dq) = (d(p), d(q));
            if dp == 0.0 {
                out.push(p);
            }
            if (dp < 0.0 && dq > 0.0) || (dp > 0.0 && dq < 0.0) {
                let s = dp / (dp - dq);
                out.push([0, 1, 2].map(|k| p[k] + s * (q[k] - p[k])));
            }
        }
        out
    };
    let (sa, sb) = (cut(a, b), cut(b, a));
    if sa.len() < 2 || sb.len() < 2 {
        return None;
    }
    let dir = cross(normal(a), normal(b));
    if dot(dir, dir) == 0.0 {
        return None;
    }
    let along = |p: [f64; 3]| dot(dir, p);
    let range = |s: &[[f64; 3]]| {
        let mut v: Vec<(f64, [f64; 3])> = s.iter().map(|&p| (along(p), p)).collect();
        v.sort_by(|x, y| x.0.total_cmp(&y.0));
        (v[0], v[v.len() - 1])
    };
    let ((a0, pa0), (a1, pa1)) = range(&sa);
    let ((b0, pb0), (b1, pb1)) = range(&sb);
    let start = if a0 >= b0 { pa0 } else { pb0 };
    let end = if a1 <= b1 { pa1 } else { pb1 };
    (a0.max(b0) <= a1.min(b1)).then_some([start, end])
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normal(t: [[f64; 3]; 3]) -> [f64; 3] {
    cross(sub(t[1], t[0]), sub(t[2], t[0]))
}
