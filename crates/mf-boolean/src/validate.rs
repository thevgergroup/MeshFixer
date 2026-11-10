//! `validate_solid`: the independent output check of D4, in its minimal
//! Task 7 form. Task 8 extends it (vertex-manifoldness, positive volume,
//! created-faces-only intersection tests against the BVH).
//!
//! A triangle mesh passes when, checked in this order:
//! 1. it has at least one face and every index is in range;
//! 2. it is **closed**, **edge-manifold** and **consistently oriented**:
//!    every edge `{u, v}` is used by exactly two faces, once as `u→v` and
//!    once as `v→u` (edges are vertex indices, so a crack between two
//!    coincident but unwelded vertices is an open edge);
//! 3. no face is degenerate (a repeated vertex, or its corners collinear,
//!    exactly; D4). A zero-area face also makes the next check
//!    meaningless around it: the faces either side of a sliver touch along
//!    its long edge without sharing it;
//! 4. no two faces intersect other than at the vertices and edges they
//!    share by index (exact: Shewchuk's `orient2d`/`orient3d`, D2). Faces
//!    touching at a point or along a segment they don't share count as
//!    intersecting: the surface would not bound a solid unambiguously.
//!
//! Every decision is exact; nothing is decided with a tolerance.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use mf_runtime::{Cancelled, RunContext, par_sort_unstable_by_key};
use rayon::prelude::*;
use robust::{Coord, Coord3D, orient2d, orient3d};

use crate::kernel::{KernelMesh, KernelResult};

/// Why a mesh is not a valid solid. Each variant names the first offender
/// (in a deterministic order) and how many there are.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SolidError {
    #[error("the mesh has no faces")]
    Empty,
    #[error("face {face} names a vertex that doesn't exist")]
    IndexOutOfRange { face: u32 },
    #[error("{faces} faces have no area (first: face {face})")]
    DegenerateFace { face: u32, faces: usize },
    #[error("the surface has {boundary_edges} open edges (first: {edge:?})")]
    NotClosed {
        edge: [u32; 2],
        boundary_edges: usize,
    },
    #[error("{edges} edges are shared by more than two faces (first: {edge:?}, {faces} faces)")]
    NotManifold {
        edge: [u32; 2],
        faces: usize,
        edges: usize,
    },
    #[error("{edges} edges join faces wound in opposite directions (first: {edge:?})")]
    InconsistentOrientation { edge: [u32; 2], edges: usize },
    #[error("{pairs} pairs of faces intersect (first: faces {faces:?})")]
    SelfIntersecting { faces: [u32; 2], pairs: usize },
    #[error("cancelled by the user")]
    Cancelled,
}

impl SolidError {
    /// A stable, machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            SolidError::Empty => "empty",
            SolidError::IndexOutOfRange { .. } => "index_out_of_range",
            SolidError::DegenerateFace { .. } => "degenerate_face",
            SolidError::NotClosed { .. } => "not_closed",
            SolidError::NotManifold { .. } => "not_manifold",
            SolidError::InconsistentOrientation { .. } => "inconsistent_orientation",
            SolidError::SelfIntersecting { .. } => "self_intersecting",
            SolidError::Cancelled => "cancelled",
        }
    }
}

impl From<Cancelled> for SolidError {
    fn from(_: Cancelled) -> Self {
        SolidError::Cancelled
    }
}

/// Checks that `triangles` over `positions` bound a solid (see the module
/// docs). Parallel on the current `rayon` pool; deterministic; cancellable.
pub fn validate_solid(
    positions: &[[f64; 3]],
    triangles: &[[u32; 3]],
    ctx: &RunContext,
) -> Result<(), SolidError> {
    prechecks(positions, triangles, ctx)?;
    check_intersections(positions, triangles, &|_, _| true, ctx)
}

/// Steps 1–3 of [`validate_solid`].
fn prechecks(
    positions: &[[f64; 3]],
    triangles: &[[u32; 3]],
    ctx: &RunContext,
) -> Result<(), SolidError> {
    ctx.checkpoint()?;
    if triangles.is_empty() {
        return Err(SolidError::Empty);
    }
    let n = positions.len() as u64;
    if let Some(face) = triangles
        .iter()
        .position(|t| t.iter().any(|&v| u64::from(v) >= n))
    {
        return Err(SolidError::IndexOutOfRange { face: face as u32 });
    }
    check_edges(triangles, ctx)?;
    let degenerate = degenerate_faces(positions, triangles, ctx)?;
    if let Some(face) = degenerate.iter().position(|&d| d) {
        return Err(SolidError::DegenerateFace {
            face: face as u32,
            faces: degenerate.iter().filter(|&&d| d).count(),
        });
    }
    Ok(())
}

/// Checks a boolean result (D4, Task 7b): everything [`validate_solid`]
/// checks, except that a self-intersection already present in an input is
/// the input's, not the kernel's. A pair of intersecting output faces
/// `(f, g)` is excused only when it is *that* crossing (Task 7b review
/// C1, geometry-granular): `f` and `g` lie on faces `x` and `y` of the
/// same operand (found through `result.origin` and the operands'
/// `face_ids`), `x` and `y` intersect each other in that operand, and
/// `f` lies within `x` and `g` within `y` (every corner exactly on the
/// input face: in its plane and inside it, by exact orientations). Then
/// `f ∩ g ⊆ x ∩ y`: the output crossing lies inside the input's own
/// crossing set. A piece that has moved off its input face (a kernel
/// defect, or a constructed point rounded off the plane) is never
/// excused, however its labelled faces met. Faces of the two operands
/// meeting is always new: the kernel should have resolved it. Two pieces
/// of one input face meeting is new too.
///
/// Labels in `result.origin` that name no input face make the pair new.
pub fn validate_boolean(
    result: &KernelResult,
    inputs: [&KernelMesh; 2],
    ctx: &RunContext,
) -> Result<(), SolidError> {
    let (positions, triangles) = (&result.positions, &result.triangles);
    prechecks(positions, triangles, ctx)?;
    // Label → (operand, triangle).
    let mut source: HashMap<(u8, u32), (usize, u32)> = HashMap::new();
    for (i, m) in inputs.iter().enumerate() {
        for (t, label) in m.face_ids.iter().enumerate() {
            source.insert(*label, (i, t as u32));
        }
    }
    let src: Vec<Option<(usize, u32)>> = result
        .origin
        .iter()
        .map(|o| source.get(o).copied())
        .collect();
    let on_source = |f: u32, i: usize, x: u32| -> bool {
        let m = inputs[i];
        let face = corners(&m.positions, m.triangles[x as usize]);
        corners(positions, triangles[f as usize])
            .iter()
            .all(|&p| point_on_triangle(p, face))
    };
    let counts = |f: u32, g: u32| -> bool {
        let (Some(Some((i, x))), Some(Some((j, y)))) = (src.get(f as usize), src.get(g as usize))
        else {
            return true;
        };
        if i != j || x == y {
            return true;
        }
        let m = inputs[*i];
        !(input_faces_intersect(
            &m.positions,
            m.triangles[*x as usize],
            m.triangles[*y as usize],
        ) && on_source(f, *i, *x)
            && on_source(g, *j, *y))
    };
    check_intersections(positions, triangles, &counts, ctx)
}

/// Whether `p` lies in closed triangle `t` (exactly: in its plane, and
/// inside or on it). A degenerate `t` contains nothing.
fn point_on_triangle(p: [f64; 3], t: [[f64; 3]; 3]) -> bool {
    let [a, b, c] = t;
    let Some(k) = drop_axis(a, b, c) else {
        return false;
    };
    o3(a, b, c, p) == 0.0 && point_in_tri_2d(p, t, k)
}

/// Whether faces `a` and `b` of an input intersect (by the same rule as
/// [`validate_solid`]). A degenerate face counts as touching whatever it
/// meets: it is the input's own defect.
fn input_faces_intersect(p: &[[f64; 3]], a: [u32; 3], b: [u32; 3]) -> bool {
    let degenerate = |t: [u32; 3]| {
        let [x, y, z] = corners(p, t);
        t[0] == t[1] || t[1] == t[2] || t[0] == t[2] || drop_axis(x, y, z).is_none()
    };
    if degenerate(a) || degenerate(b) {
        return true;
    }
    intersects(p, a, b)
}

/// Which faces intersect another face of the same mesh (by the rule of
/// [`validate_solid`], step 4). Indices must be in range; degenerate faces
/// are tested as they are.
pub fn self_intersecting_faces(
    positions: &[[f64; 3]],
    triangles: &[[u32; 3]],
    ctx: &RunContext,
) -> Result<Vec<bool>, SolidError> {
    let n = positions.len() as u64;
    if let Some(face) = triangles
        .iter()
        .position(|t| t.iter().any(|&v| u64::from(v) >= n))
    {
        return Err(SolidError::IndexOutOfRange { face: face as u32 });
    }
    let flags: Vec<AtomicBool> = (0..triangles.len())
        .map(|_| AtomicBool::new(false))
        .collect();
    if triangles.is_empty() {
        return Ok(Vec::new());
    }
    let mark = |f: u32, g: u32| -> bool {
        flags[f as usize].store(true, Ordering::Relaxed);
        flags[g as usize].store(true, Ordering::Relaxed);
        false
    };
    check_intersections(positions, triangles, &mark, ctx)?;
    Ok(flags.into_iter().map(AtomicBool::into_inner).collect())
}

/// Whether some edge is used by only one face.
pub(crate) fn has_open_edge(triangles: &[[u32; 3]]) -> bool {
    matches!(
        check_edges(triangles, &RunContext::none()),
        Err(SolidError::NotClosed { .. })
    )
}

fn c3(p: [f64; 3]) -> Coord3D<f64> {
    Coord3D {
        x: p[0],
        y: p[1],
        z: p[2],
    }
}

fn o3(a: [f64; 3], b: [f64; 3], c: [f64; 3], d: [f64; 3]) -> f64 {
    orient3d(c3(a), c3(b), c3(c), c3(d))
}

/// `p` with axis `drop` removed.
fn proj(p: [f64; 3], drop: usize) -> Coord<f64> {
    let (x, y) = match drop {
        0 => (p[1], p[2]),
        1 => (p[2], p[0]),
        _ => (p[0], p[1]),
    };
    Coord { x, y }
}

fn o2(a: [f64; 3], b: [f64; 3], c: [f64; 3], drop: usize) -> f64 {
    orient2d(proj(a, drop), proj(b, drop), proj(c, drop))
}

/// The axis to drop so the triangle's projection keeps the most area
/// (exact orientations, so a non-degenerate triangle never projects to a
/// segment). `None` for a degenerate triangle.
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

/// `orient2d` of three 2D points.
pub(crate) fn o2_coords(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    orient2d(
        Coord { x: a[0], y: a[1] },
        Coord { x: b[0], y: b[1] },
        Coord { x: c[0], y: c[1] },
    )
}

/// Whether a face of `a` meets a face of `b` (exactly, touching counts).
/// Degenerate faces are tested as they are.
pub(crate) fn operands_meet(
    a: &KernelMesh,
    b: &KernelMesh,
    ctx: &RunContext,
) -> Result<bool, SolidError> {
    if a.triangles.is_empty() || b.triangles.is_empty() {
        return Ok(false);
    }
    let n = a.triangles.len() as u32;
    let base = a.positions.len() as u32;
    let mut positions = a.positions.clone();
    positions.extend_from_slice(&b.positions);
    let mut triangles = a.triangles.clone();
    triangles.extend(b.triangles.iter().map(|t| t.map(|v| v + base)));
    let across = |f: u32, g: u32| (f < n) != (g < n);
    match check_intersections(&positions, &triangles, &across, ctx) {
        Ok(()) => Ok(false),
        Err(SolidError::SelfIntersecting { .. }) => Ok(true),
        Err(e) => Err(e),
    }
}

fn corners(p: &[[f64; 3]], t: [u32; 3]) -> [[f64; 3]; 3] {
    t.map(|v| p[v as usize])
}

/// Which faces are degenerate: a repeated vertex, or collinear corners.
fn degenerate_faces(
    positions: &[[f64; 3]],
    triangles: &[[u32; 3]],
    ctx: &RunContext,
) -> Result<Vec<bool>, SolidError> {
    let bad = triangles
        .par_iter()
        .map(|t| {
            let [a, b, c] = corners(positions, *t);
            t[0] == t[1] || t[1] == t[2] || t[0] == t[2] || drop_axis(a, b, c).is_none()
        })
        .collect();
    ctx.checkpoint()?;
    Ok(bad)
}

fn check_edges(triangles: &[[u32; 3]], ctx: &RunContext) -> Result<(), SolidError> {
    // (low, high, forward): forward when the face runs low → high.
    let mut edges: Vec<(u32, u32, bool)> = triangles
        .par_iter()
        .flat_map_iter(|t| {
            (0..3).map(move |i| {
                let (u, v) = (t[i], t[(i + 1) % 3]);
                (u.min(v), u.max(v), u < v)
            })
        })
        .collect();
    ctx.checkpoint()?;
    par_sort_unstable_by_key(&mut edges, ctx, |&e| e)?;

    let mut open: Option<([u32; 2], usize)> = None;
    let mut non_manifold: Option<([u32; 2], usize, usize)> = None;
    let mut misoriented: Option<([u32; 2], usize)> = None;
    let mut ticker = ctx.ticker();
    for group in edges.chunk_by(|a, b| a.0 == b.0 && a.1 == b.1) {
        ticker.tick()?;
        let edge = [group[0].0, group[0].1];
        let forward = group.iter().filter(|e| e.2).count();
        match group.len() {
            1 => open.get_or_insert((edge, 0)).1 += 1,
            2 if forward != 1 => misoriented.get_or_insert((edge, 0)).1 += 1,
            2 => {}
            faces => non_manifold.get_or_insert((edge, faces, 0)).2 += 1,
        }
    }
    if let Some((edge, boundary_edges)) = open {
        return Err(SolidError::NotClosed {
            edge,
            boundary_edges,
        });
    }
    if let Some((edge, faces, edges)) = non_manifold {
        return Err(SolidError::NotManifold { edge, faces, edges });
    }
    if let Some((edge, edges)) = misoriented {
        return Err(SolidError::InconsistentOrientation { edge, edges });
    }
    Ok(())
}

/// More cells than this and a face is tested against every overlapping
/// face directly instead of being entered into the grid.
const MAX_CELLS_PER_FACE: u64 = 64;

#[derive(Clone, Copy)]
struct Bounds {
    lo: [f64; 3],
    hi: [f64; 3],
}

impl Bounds {
    fn of(c: &[[f64; 3]; 3]) -> Bounds {
        let mut b = Bounds { lo: c[0], hi: c[0] };
        for p in &c[1..] {
            for (k, &x) in p.iter().enumerate() {
                b.lo[k] = b.lo[k].min(x);
                b.hi[k] = b.hi[k].max(x);
            }
        }
        b
    }

    /// Closed overlap: touching boxes overlap.
    fn overlaps(&self, o: &Bounds) -> bool {
        (0..3).all(|k| self.lo[k] <= o.hi[k] && o.lo[k] <= self.hi[k])
    }
}

struct Grid {
    origin: [f64; 3],
    cell: f64,
    dims: [u64; 3],
}

impl Grid {
    fn coord(&self, x: f64, k: usize) -> u64 {
        let i = ((x - self.origin[k]) / self.cell).floor();
        (i.max(0.0) as u64).min(self.dims[k] - 1)
    }

    fn range(&self, b: &Bounds) -> ([u64; 3], [u64; 3]) {
        let lo = [0, 1, 2].map(|k| self.coord(b.lo[k], k));
        let hi = [0, 1, 2].map(|k| self.coord(b.hi[k], k));
        (lo, hi)
    }

    fn key(&self, c: [u64; 3]) -> u64 {
        (c[2] * self.dims[1] + c[1]) * self.dims[0] + c[0]
    }
}

/// The smallest intersecting pair and how many pairs intersect.
type Found = Option<([u32; 2], usize)>;

fn merge_found(a: Found, b: Found) -> Found {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some((p, n)), Some((q, m))) => Some((p.min(q), n + m)),
    }
}

/// Step 4: every pair of faces that intersect, decided exactly, is passed
/// to `counts`; the pairs it returns `true` for are reported.
fn check_intersections(
    positions: &[[f64; 3]],
    triangles: &[[u32; 3]],
    counts: &(dyn Fn(u32, u32) -> bool + Sync),
    ctx: &RunContext,
) -> Result<(), SolidError> {
    let bounds: Vec<Bounds> = triangles
        .par_iter()
        .map(|t| Bounds::of(&corners(positions, *t)))
        .collect();
    ctx.checkpoint()?;
    let mut all = bounds[0];
    let mut extent_sum = 0.0;
    for b in &bounds {
        for k in 0..3 {
            all.lo[k] = all.lo[k].min(b.lo[k]);
            all.hi[k] = all.hi[k].max(b.hi[k]);
        }
        extent_sum += (0..3).map(|k| b.hi[k] - b.lo[k]).fold(0.0, f64::max);
    }
    let size = (0..3).map(|k| all.hi[k] - all.lo[k]).fold(0.0, f64::max);
    // Cells about the size of an average face, at most 2^20 per axis and
    // about 8 faces' worth of cells overall.
    let mean = extent_sum / bounds.len() as f64;
    let max_cells = (8 * bounds.len() as u64).max(1);
    let mut cell = mean.max(size * 1e-6).max(f64::MIN_POSITIVE);
    let dims_for = |cell: f64| {
        [0, 1, 2].map(|k| (((all.hi[k] - all.lo[k]) / cell).floor() as u64 + 1).min(1 << 20))
    };
    let mut dims = dims_for(cell);
    while dims[0].saturating_mul(dims[1]).saturating_mul(dims[2]) > max_cells {
        cell *= 1.5;
        dims = dims_for(cell);
    }
    let grid = Grid {
        origin: all.lo,
        cell,
        dims,
    };

    // Grid entries, and the faces too big for the grid.
    let per_face: Vec<Option<Vec<(u64, u32)>>> = bounds
        .par_iter()
        .enumerate()
        .map(|(f, b)| {
            let (lo, hi) = grid.range(b);
            let count = (0..3).map(|k| hi[k] - lo[k] + 1).product::<u64>();
            if count > MAX_CELLS_PER_FACE {
                return None;
            }
            let mut out = Vec::with_capacity(count as usize);
            for z in lo[2]..=hi[2] {
                for y in lo[1]..=hi[1] {
                    for x in lo[0]..=hi[0] {
                        out.push((grid.key([x, y, z]), f as u32));
                    }
                }
            }
            Some(out)
        })
        .collect();
    ctx.checkpoint()?;
    let big: Vec<u32> = per_face
        .iter()
        .enumerate()
        .filter(|(_, e)| e.is_none())
        .map(|(f, _)| f as u32)
        .collect();
    let mut entries: Vec<(u64, u32)> = per_face.into_iter().flatten().flatten().collect();
    par_sort_unstable_by_key(&mut entries, ctx, |&e| e)?;
    let cells: Vec<&[(u64, u32)]> = entries.chunk_by(|a, b| a.0 == b.0).collect();

    let test = |f: u32, g: u32| -> bool {
        intersects(positions, triangles[f as usize], triangles[g as usize]) && counts(f, g)
    };

    let in_cells: Found = cells
        .par_iter()
        .map(|cell_faces| -> Result<Found, Cancelled> {
            ctx.checkpoint()?;
            let key = cell_faces[0].0;
            let mut found: Found = None;
            for (i, &(_, f)) in cell_faces.iter().enumerate() {
                for &(_, g) in &cell_faces[i + 1..] {
                    let (bf, bg) = (&bounds[f as usize], &bounds[g as usize]);
                    if !bf.overlaps(bg) {
                        continue;
                    }
                    // Test each pair once: in the cell holding the low
                    // corner of the two boxes' overlap.
                    let corner = [0, 1, 2].map(|k| grid.coord(bf.lo[k].max(bg.lo[k]), k));
                    if grid.key(corner) != key {
                        continue;
                    }
                    if test(f, g) {
                        found = merge_found(found, Some(([f.min(g), f.max(g)], 1)));
                    }
                }
            }
            Ok(found)
        })
        .try_reduce(|| None, |a, b| Ok(merge_found(a, b)))?;

    let is_big = {
        let mut v = vec![false; triangles.len()];
        for &f in &big {
            v[f as usize] = true;
        }
        v
    };
    let from_big: Found = big
        .par_iter()
        .map(|&f| -> Result<Found, Cancelled> {
            let mut found: Found = None;
            let bf = &bounds[f as usize];
            let mut ticker = ctx.ticker();
            for (g, bg) in bounds.iter().enumerate() {
                ticker.tick()?;
                let g = g as u32;
                // Big–big pairs once, from the lower index.
                if g == f || (is_big[g as usize] && g < f) || !bf.overlaps(bg) {
                    continue;
                }
                if test(f, g) {
                    found = merge_found(found, Some(([f.min(g), f.max(g)], 1)));
                }
            }
            Ok(found)
        })
        .try_reduce(|| None, |a, b| Ok(merge_found(a, b)))?;

    match merge_found(in_cells, from_big) {
        Some((faces, pairs)) => Err(SolidError::SelfIntersecting { faces, pairs }),
        None => Ok(()),
    }
}

/// Whether faces `a` and `b` meet anywhere other than at what they share
/// by vertex index. Both are non-degenerate.
fn intersects(p: &[[f64; 3]], a: [u32; 3], b: [u32; 3]) -> bool {
    let shared: Vec<(usize, usize)> = (0..3)
        .flat_map(|i| (0..3).filter(move |&j| a[i] == b[j]).map(move |j| (i, j)))
        .collect();
    let (ca, cb) = (corners(p, a), corners(p, b));
    match shared.len() {
        0 => tri_tri(ca, cb),
        1 => {
            let (i, j) = shared[0];
            shared_vertex(rot(ca, i), rot(cb, j))
        }
        2 => {
            // The corners not shared.
            let i = (0..3).find(|i| shared.iter().all(|s| s.0 != *i));
            let j = (0..3).find(|j| shared.iter().all(|s| s.1 != *j));
            match (i, j) {
                (Some(i), Some(j)) => shared_edge(ca[shared[0].0], ca[shared[1].0], ca[i], cb[j]),
                // A face naming one vertex twice is degenerate, refused earlier.
                _ => true,
            }
        }
        // The same three vertices: coincident faces overlap.
        _ => true,
    }
}

/// `c` rotated so corner `i` comes first.
fn rot(c: [[f64; 3]; 3], i: usize) -> [[f64; 3]; 3] {
    [c[i], c[(i + 1) % 3], c[(i + 2) % 3]]
}

/// Closed triangles with no shared vertex: they intersect exactly when an
/// edge of one meets the other (for non-coplanar triangles the
/// intersection's endpoints lie on edges; coplanar containment puts the
/// inner triangle's edges inside the outer one).
fn tri_tri(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> bool {
    (0..3).any(|i| seg_tri(a[i], a[(i + 1) % 3], b))
        || (0..3).any(|i| seg_tri(b[i], b[(i + 1) % 3], a))
}

/// Closed segment `pq` against closed triangle `t`.
fn seg_tri(p: [f64; 3], q: [f64; 3], t: [[f64; 3]; 3]) -> bool {
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
    // The segment meets the plane at one point: is it in the triangle?
    let t1 = o3(p, q, a, b);
    let t2 = o3(p, q, b, c);
    let t3 = o3(p, q, c, a);
    (t1 >= 0.0 && t2 >= 0.0 && t3 >= 0.0) || (t1 <= 0.0 && t2 <= 0.0 && t3 <= 0.0)
}

fn point_in_tri_2d(p: [f64; 3], t: [[f64; 3]; 3], k: usize) -> bool {
    let [a, b, c] = t;
    let (d1, d2, d3) = (o2(a, b, p, k), o2(b, c, p, k), o2(c, a, p, k));
    (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0)
}

/// `r` on segment `pq`, given the three are collinear.
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

/// Faces sharing exactly vertex `a[0] == b[0]`: they meet elsewhere when
/// an opposite edge meets the other face, or an edge from the shared
/// vertex runs into the other face (lying in its plane, inside its corner).
fn shared_vertex(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> bool {
    if seg_tri(a[1], a[2], b) || seg_tri(b[1], b[2], a) {
        return true;
    }
    edge_into(a, b) || edge_into(b, a)
}

/// Whether an edge of `a` from the shared corner `a[0]` runs into `b`.
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

#[cfg(test)]
mod tests {
    use super::*;

    const O: [f64; 3] = [0.0, 0.0, 0.0];
    const X: [f64; 3] = [1.0, 0.0, 0.0];
    const Y: [f64; 3] = [0.0, 1.0, 0.0];

    #[test]
    fn disjoint_and_crossing_triangles() {
        let a = [O, X, Y];
        assert!(!tri_tri(
            a,
            [[0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [0.0, 1.0, 1.0]]
        ));
        // Pierces a's interior.
        assert!(tri_tri(
            a,
            [[0.2, 0.2, -1.0], [0.2, 0.2, 1.0], [0.3, -1.0, 0.0]]
        ));
        // Touches a's edge at one point.
        assert!(tri_tri(
            a,
            [[0.5, 0.0, 0.0], [0.5, -1.0, 1.0], [0.5, -1.0, -1.0]]
        ));
        // Coplanar and inside.
        assert!(tri_tri(
            a,
            [[0.1, 0.1, 0.0], [0.2, 0.1, 0.0], [0.1, 0.2, 0.0]]
        ));
        // Coplanar and apart.
        assert!(!tri_tri(
            a,
            [[1.0, 1.0, 0.0], [2.0, 1.0, 0.0], [1.0, 2.0, 0.0]]
        ));
    }

    #[test]
    fn faces_sharing_a_vertex() {
        let a = [O, X, Y];
        // Only the corner in common.
        assert!(!shared_vertex(a, [O, [-1.0, 0.0, 0.0], [0.0, -1.0, 0.0]]));
        assert!(!shared_vertex(a, [O, [0.0, 0.0, 1.0], [-1.0, 0.0, 1.0]]));
        // The other face passes through a.
        assert!(shared_vertex(a, [O, [0.5, 0.5, 1.0], [0.5, 0.5, -1.0]]));
        // Coplanar, overlapping corners.
        assert!(shared_vertex(a, [O, [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0]]));
        // An edge from the corner runs into a, in its plane.
        assert!(shared_vertex(a, [O, [0.5, 0.5, 0.0], [0.5, 0.5, 1.0]]));
        // Along a's edge, outward from the corner: overlap on a segment.
        assert!(shared_vertex(a, [O, [2.0, 0.0, 0.0], [0.0, 0.0, 1.0]]));
        // Collinear with a's edge but pointing away: no overlap.
        assert!(!shared_vertex(a, [O, [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]));
    }

    #[test]
    fn faces_sharing_an_edge() {
        // A fold (same side, coplanar) overlaps; a flat or bent pair doesn't.
        assert!(shared_edge(O, X, Y, [0.5, 0.5, 0.0]));
        assert!(!shared_edge(O, X, Y, [0.5, -0.5, 0.0]));
        assert!(!shared_edge(O, X, Y, [0.5, 0.5, 0.1]));
    }
}
