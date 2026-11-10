//! `validate_solid`: D4's independent check of a modelling result, local to
//! the faces the operation created (Phase 1 Task 8).
//!
//! Around every created face, checked in this order:
//! 1. **closed**: each of its edges has a face on the other side (an edge
//!    the caller allows to stay open, such as a scan's own hole kept as it
//!    was, is skipped);
//! 2. **edge-manifold**: no edge of it has more than two faces;
//! 3. **vertex-manifold**: the faces around each of its corners form one
//!    fan (one cycle, or one chain at an allowed opening);
//! 4. **consistently oriented**: the two faces on each of its edges run
//!    the edge in opposite directions;
//! 5. **not degenerate**: it has area (exactly);
//! 6. **no self-intersection**: it meets no other face except at the
//!    vertices and edges they share by index (exact, D2), found through the
//!    caller's [`Bvh`];
//! 7. **positive volume**, when the created faces are the whole mesh.
//!
//! The [`Bvh`] must cover every live face that could touch a created face
//! (a BVH over the whole mesh, or over the faces near the created ones).
//! Faces the operation kept are not re-checked against each other: their
//! defects are the input's, not the operation's. Deterministic at any
//! thread count; cancellable.

use std::collections::HashMap;

use mf_runtime::{Cancelled, RunContext};
use rayon::prelude::*;

use crate::exact::{degenerate, faces_meet};
use crate::{Bvh, FaceId, Mesh};

/// Why a result is not a valid solid around the faces an operation
/// created. Lists are sorted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SolidDefect {
    Open { edges: Vec<[u32; 2]> },
    NonManifoldEdge { edges: Vec<[u32; 2]> },
    NonManifoldVertex { vertices: Vec<u32> },
    Inconsistent { edges: Vec<[u32; 2]> },
    Degenerate { faces: Vec<FaceId> },
    SelfIntersects { pairs: Vec<[FaceId; 2]> },
    NonPositiveVolume,
}

impl SolidDefect {
    /// A stable, machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            SolidDefect::Open { .. } => "open",
            SolidDefect::NonManifoldEdge { .. } => "non_manifold_edge",
            SolidDefect::NonManifoldVertex { .. } => "non_manifold_vertex",
            SolidDefect::Inconsistent { .. } => "inconsistent",
            SolidDefect::Degenerate { .. } => "degenerate",
            SolidDefect::SelfIntersects { .. } => "self_intersects",
            SolidDefect::NonPositiveVolume => "non_positive_volume",
        }
    }
}

impl std::fmt::Display for SolidDefect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SolidDefect::Open { edges } => {
                write!(f, "{} open edges (first {:?})", edges.len(), edges[0])
            }
            SolidDefect::NonManifoldEdge { edges } => {
                write!(
                    f,
                    "{} edges on more than two faces (first {:?})",
                    edges.len(),
                    edges[0]
                )
            }
            SolidDefect::NonManifoldVertex { vertices } => write!(
                f,
                "{} vertices where separate sheets meet (first {})",
                vertices.len(),
                vertices[0]
            ),
            SolidDefect::Inconsistent { edges } => write!(
                f,
                "{} edges between faces wound in opposite directions (first {:?})",
                edges.len(),
                edges[0]
            ),
            SolidDefect::Degenerate { faces } => {
                write!(
                    f,
                    "{} faces with no area (first {})",
                    faces.len(),
                    faces[0].0
                )
            }
            SolidDefect::SelfIntersects { pairs } => write!(
                f,
                "{} pairs of faces intersect (first {} and {})",
                pairs.len(),
                pairs[0][0].0,
                pairs[0][1].0
            ),
            SolidDefect::NonPositiveVolume => write!(f, "the result encloses no volume"),
        }
    }
}

/// [`validate_solid_with`], with no edge allowed to stay open.
pub fn validate_solid(
    mesh: &Mesh,
    created: &[FaceId],
    bvh: &Bvh,
    ctx: &RunContext,
) -> Result<Result<(), SolidDefect>, Cancelled> {
    validate_solid_with(mesh, created, bvh, &|_, _| false, ctx)
}

/// Checks `mesh` around the `created` faces (see the module docs).
/// `allow_open(u, v)` says whether edge `{u, v}` may stay open (a hole the
/// input already had); it is asked only about edges with one face.
pub fn validate_solid_with(
    mesh: &Mesh,
    created: &[FaceId],
    bvh: &Bvh,
    allow_open: &(dyn Fn(u32, u32) -> bool + Sync),
    ctx: &RunContext,
) -> Result<Result<(), SolidDefect>, Cancelled> {
    ctx.checkpoint()?;
    let mut created: Vec<FaceId> = created
        .iter()
        .copied()
        .filter(|&f| mesh.is_face_live(f))
        .collect();
    created.sort_unstable();
    created.dedup();
    let corners = |t: [u32; 3]| t.map(|v| mesh.corner_position(v));
    let tri = |f: FaceId| mesh.face(f).unwrap_or([0, 0, 0]);

    // The faces near each created face (bounds overlap, from the BVH),
    // with the created face first.
    let near: Vec<Vec<FaceId>> = created
        .par_iter()
        .map(|&f| {
            let c = corners(tri(f));
            let (lo, hi) = bounds(&c);
            let mut out = Vec::new();
            bvh.for_each_overlapping(lo, hi, |g| {
                if g != f && mesh.is_face_live(g) {
                    out.push(g);
                }
            });
            out.sort_unstable();
            out
        })
        .collect();
    ctx.checkpoint()?;

    // 1, 2, 4: every edge of a created face, against the faces near it
    // (a face sharing the edge shares its corners, so its bounds overlap).
    let mut open = Vec::new();
    let mut non_manifold = Vec::new();
    let mut inconsistent = Vec::new();
    let mut ticker = ctx.ticker();
    for (i, &f) in created.iter().enumerate() {
        ticker.tick()?;
        let t = tri(f);
        for k in 0..3 {
            let (u, v) = (t[k], t[(k + 1) % 3]);
            let key = [u.min(v), u.max(v)];
            let mut faces = 1;
            let mut same_direction = 0;
            for &g in &near[i] {
                let s = tri(g);
                for j in 0..3 {
                    let (a, b) = (s[j], s[(j + 1) % 3]);
                    if a == u && b == v {
                        faces += 1;
                        same_direction += 1;
                    } else if a == v && b == u {
                        faces += 1;
                    }
                }
            }
            match faces {
                1 if !allow_open(u, v) => open.push(key),
                1 => {}
                2 if same_direction > 0 => inconsistent.push(key),
                2 => {}
                _ => non_manifold.push(key),
            }
        }
    }
    for list in [&mut open, &mut non_manifold, &mut inconsistent] {
        list.sort_unstable();
        list.dedup();
    }
    if !open.is_empty() {
        return Ok(Err(SolidDefect::Open { edges: open }));
    }
    if !non_manifold.is_empty() {
        return Ok(Err(SolidDefect::NonManifoldEdge {
            edges: non_manifold,
        }));
    }

    // 3: the fan around each corner of a created face.
    let mut around: HashMap<u32, Vec<FaceId>> = HashMap::new();
    for (i, &f) in created.iter().enumerate() {
        let t = tri(f);
        for v in t {
            let list = around.entry(v).or_default();
            list.push(f);
            list.extend(near[i].iter().copied().filter(|&g| tri(g).contains(&v)));
        }
    }
    ctx.checkpoint()?;
    let mut vertices: Vec<u32> = around
        .par_iter_mut()
        .filter_map(|(&v, faces)| {
            faces.sort_unstable();
            faces.dedup();
            (fans(v, faces, &tri) != 1).then_some(v)
        })
        .collect();
    vertices.sort_unstable();
    if !vertices.is_empty() {
        return Ok(Err(SolidDefect::NonManifoldVertex { vertices }));
    }
    if !inconsistent.is_empty() {
        return Ok(Err(SolidDefect::Inconsistent {
            edges: inconsistent,
        }));
    }

    // 5.
    let degenerate_faces: Vec<FaceId> = created
        .par_iter()
        .copied()
        .filter(|&f| degenerate(tri(f), corners(tri(f))))
        .collect();
    if !degenerate_faces.is_empty() {
        return Ok(Err(SolidDefect::Degenerate {
            faces: degenerate_faces,
        }));
    }

    // 6: each created face against every face near it; a pair of created
    // faces once, from the lower id.
    let is_created = |g: FaceId| created.binary_search(&g).is_ok();
    let pairs: Vec<[FaceId; 2]> = created
        .par_iter()
        .enumerate()
        .map(|(i, &f)| -> Result<Vec<[FaceId; 2]>, Cancelled> {
            ctx.checkpoint()?;
            let (tf, cf) = (tri(f), corners(tri(f)));
            Ok(near[i]
                .iter()
                .copied()
                .filter(|&g| !(is_created(g) && g < f))
                .filter(|&g| {
                    let tg = tri(g);
                    let cg = corners(tg);
                    // A degenerate kept face meets whatever touches it.
                    if degenerate(tg, cg) {
                        overlap(&cf, &cg)
                    } else {
                        faces_meet(tf, cf, tg, cg)
                    }
                })
                .map(|g| [f.min(g), f.max(g)])
                .collect())
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    if !pairs.is_empty() {
        let mut pairs = pairs;
        pairs.sort_unstable();
        pairs.dedup();
        return Ok(Err(SolidDefect::SelfIntersects { pairs }));
    }

    // 7.
    if created.len() == mesh.face_count() && signed_volume(mesh) <= 0.0 {
        return Ok(Err(SolidDefect::NonPositiveVolume));
    }
    Ok(Ok(()))
}

fn bounds(c: &[[f64; 3]; 3]) -> ([f64; 3], [f64; 3]) {
    let mut lo = c[0];
    let mut hi = c[0];
    for p in &c[1..] {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}

fn overlap(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> bool {
    let (alo, ahi) = bounds(a);
    let (blo, bhi) = bounds(b);
    (0..3).all(|k| alo[k] <= bhi[k] && blo[k] <= ahi[k])
}

/// How many separate fans the `faces` around `v` form (faces joined across
/// an edge `{v, w}` they share).
fn fans(v: u32, faces: &[FaceId], tri: &impl Fn(FaceId) -> [u32; 3]) -> usize {
    let n = faces.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    // Other corner → the faces around v using edge {v, w}.
    let mut by_edge: HashMap<u32, Vec<usize>> = HashMap::new();
    for (i, &f) in faces.iter().enumerate() {
        for w in tri(f) {
            if w != v {
                by_edge.entry(w).or_default().push(i);
            }
        }
    }
    for list in by_edge.values() {
        for pair in list.windows(2) {
            let (a, b) = (find(&mut parent, pair[0]), find(&mut parent, pair[1]));
            parent[a] = b;
        }
    }
    (0..n).filter(|&i| find(&mut parent, i) == i).count()
}

/// Six times the signed volume the mesh's faces enclose (positive for an
/// outward-facing closed surface).
pub fn signed_volume(mesh: &Mesh) -> f64 {
    let faces: Vec<[u32; 3]> = mesh.faces().map(|(_, t)| t).collect();
    faces
        .par_chunks(1 << 14)
        .map(|chunk| {
            chunk
                .iter()
                .map(|t| {
                    let [a, b, c] = t.map(|v| mesh.corner_position(v));
                    a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                        + a[2] * (b[0] * c[1] - b[1] * c[0])
                })
                .sum::<f64>()
        })
        .collect::<Vec<f64>>()
        .into_iter()
        .sum()
}
