//! The exact plane cut with capped parts (D11, MF-MODEL-003, Phase 1
//! Task 12). Our own code, not the boolean kernel: a cut that crosses a
//! 30M-triangle scan costs O(n) classification plus local work at the
//! section.
//!
//! - **Sides are exact.** The plane is the one through three f64 points
//!   built from `Plane` ([`exact_points`]); each vertex's side
//!   is the sign of Shewchuk's `orient3d` against them (D2). A vertex with
//!   sign zero is *on* the plane and is shared by the faces either side and
//!   the cap: it is never duplicated or moved.
//! - **Each crossing edge is split once**, from its lower vertex id to its
//!   higher, so both faces using the edge share the cut point. For an
//!   axis-aligned normal the cut point's axis coordinate is set to the
//!   plane's exactly, so horizontal and vertical caps are exactly flat.
//! - **Faces lying in the plane** (all three corners on it) are removed
//!   from both parts; the caps cover their area again. This keeps one rule
//!   for steps and ledges that sit exactly on the cut.
//! - **Caps** close the boundary loops left on the plane. They are
//!   triangulated by a constrained Delaunay triangulation (`spade`, D3)
//!   with nesting by orientation (a winding number per triangle), so a
//!   hollow model's section is an annulus. A section whose loops cross,
//!   touch or pinch in the plane is refused as
//!   [`CutError::SectionSelfIntersects`]: "move the plane slightly".
//!
//! Faces away from the plane keep their ids, indices and positions, bit for
//! bit, in both parts: each part is a [`MeshEdit`] of the input (MF-OP-003).
//!
//! Caps use the same section triangulation as the boolean window
//! (`section.rs`, `triangulate_section`).

use std::collections::{HashMap, HashSet};

use mf_geometry::{CHUNK, FaceId, GeometryError, Mesh, Provenance, Sources, VertexId};
use mf_runtime::{Cancelled, RunContext};
use rayon::prelude::*;
use robust::{Coord, Coord3D, orient2d, orient3d};

/// Progress stages of [`cut_half`] and [`plane_cut`].
pub const CHECKING: &str = "checking the model";
pub const FINDING_THE_CUT: &str = "finding the cut";
pub const CUTTING: &str = "cutting";
pub const CAPPING: &str = "capping the cut";

/// A cutting plane: a point on it and its normal (any length, not zero).
/// "Above" is the side the normal points to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane {
    pub point: [f64; 3],
    pub normal: [f64; 3],
}

/// A right-handed frame on a plane: `u × v = normal`, all unit length. A
/// point `p` on the plane has plane coordinates `((p − origin)·u, (p −
/// origin)·v)`, and counter-clockwise in those coordinates is
/// counter-clockwise seen from the side the normal points to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaneFrame {
    pub origin: [f64; 3],
    pub u: [f64; 3],
    pub v: [f64; 3],
    pub normal: [f64; 3],
}

/// One connected piece of a section: its outer loop counter-clockwise and
/// its holes clockwise, in `frame`'s plane coordinates. P1-M3's peg
/// placement works on these.
#[derive(Debug, Clone, PartialEq)]
pub struct SectionRegion {
    pub outer: Vec<[f64; 2]>,
    pub holes: Vec<Vec<[f64; 2]>>,
    pub frame: PlaneFrame,
}

/// Which side of the plane a part is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Above,
    Below,
}

/// One side of a cut, closed by its cap.
#[derive(Debug, Clone)]
pub struct HalfCut {
    /// An edit of the input: kept faces keep their ids.
    pub mesh: Mesh,
    /// How `mesh` differs from the input. Created faces name their input
    /// faces in [`Sources::faces`].
    pub provenance: Provenance,
    /// The cap's faces, in `mesh`.
    pub cap_faces: Vec<FaceId>,
    /// The section this part's cap covers.
    pub section_regions: Vec<SectionRegion>,
}

/// Both sides of a cut through one plane.
#[derive(Debug, Clone)]
pub struct CutParts {
    pub above: (Mesh, Provenance),
    pub below: (Mesh, Provenance),
    pub section_regions: Vec<SectionRegion>,
    pub above_cap_faces: Vec<FaceId>,
    pub below_cap_faces: Vec<FaceId>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CutError {
    #[error("the plane's normal is zero or not a finite direction")]
    InvalidPlane,
    #[error(
        "the model isn't closed ({boundary_edges} open edges, {non_manifold_edges} edges shared \
         by more than two faces, {inconsistent_edges} edges between faces facing opposite ways)"
    )]
    NotClosed {
        boundary_edges: usize,
        non_manifold_edges: usize,
        inconsistent_edges: usize,
    },
    #[error("the plane doesn't cut the object: all of it is on one side")]
    MissesObject,
    #[error("the section crosses or touches itself near {at:?}")]
    SectionSelfIntersects { at: [f64; 3] },
    #[error(transparent)]
    Geometry(#[from] GeometryError),
    #[error("cancelled by the user")]
    Cancelled,
}

impl From<Cancelled> for CutError {
    fn from(_: Cancelled) -> Self {
        CutError::Cancelled
    }
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

fn normalized(a: [f64; 3]) -> Option<[f64; 3]> {
    let len = dot(a, a).sqrt();
    (len > 0.0 && len.is_finite()).then(|| a.map(|c| c / len))
}

/// The axis an exactly axis-aligned normal points along, if it is one.
fn axis_of(n: [f64; 3]) -> Option<usize> {
    let nonzero: Vec<usize> = (0..3).filter(|&i| n[i] != 0.0).collect();
    (nonzero.len() == 1).then(|| nonzero[0])
}

impl PlaneFrame {
    /// The frame of `plane`, or `None` for a zero or non-finite normal or
    /// point. For an axis-aligned normal, `u` and `v` are axis-aligned
    /// too (exactly).
    pub fn new(plane: &Plane) -> Option<PlaneFrame> {
        if !plane.point.iter().all(|c| c.is_finite()) {
            return None;
        }
        let normal = normalized(plane.normal)?;
        let normal = match axis_of(plane.normal) {
            // Exactly ±1 on the axis, so nothing downstream rounds.
            Some(i) => {
                let mut n = [0.0; 3];
                n[i] = plane.normal[i].signum();
                n
            }
            None => normal,
        };
        // The helper axis least aligned with the normal (lowest index on a
        // tie) gives a well-conditioned `u`.
        let mut helper_axis = 0;
        for i in 1..3 {
            if normal[i].abs() < normal[helper_axis].abs() {
                helper_axis = i;
            }
        }
        let mut helper = [0.0; 3];
        helper[helper_axis] = 1.0;
        let u = normalized(cross(normal, helper))?;
        let v = cross(normal, u);
        Some(PlaneFrame {
            origin: plane.point,
            u,
            v,
            normal,
        })
    }

    pub fn to_2d(&self, p: [f64; 3]) -> [f64; 2] {
        let d = sub(p, self.origin);
        [dot(d, self.u), dot(d, self.v)]
    }

    pub fn to_3d(&self, q: [f64; 2]) -> [f64; 3] {
        std::array::from_fn(|i| self.origin[i] + q[0] * self.u[i] + q[1] * self.v[i])
    }
}

/// Three f64 points that *define* the cut plane for the exact side test,
/// counter-clockwise seen from the side the normal points to: the plane's
/// point `a`, and `a + s·(n × e)` and `a + s·(n × (n × e))`, from the
/// normal as given (not normalised) and a helper axis `e`, with `s` a
/// power of two (exact) large enough that the offsets dwarf `a`. For a
/// normal with small whole-number components, such as (1, 1, 0), every
/// product is exact, so a vertex exactly on the intended plane is exactly
/// on the defining one; otherwise rounding tilts it by about an ulp.
pub fn exact_points(plane: &Plane) -> [[f64; 3]; 3] {
    let n = plane.normal;
    let mut helper_axis = 0;
    for i in 1..3 {
        if n[i].abs() < n[helper_axis].abs() {
            helper_axis = i;
        }
    }
    let mut helper = [0.0; 3];
    helper[helper_axis] = 1.0;
    let u = cross(n, helper);
    let v = cross(n, u);
    let reach = plane.point.iter().fold(1.0_f64, |m, c| m.max(c.abs()));
    let size = u
        .iter()
        .chain(&v)
        .fold(0.0_f64, |m, c| m.max(c.abs()))
        .max(f64::MIN_POSITIVE);
    let s = (reach / size).log2().ceil().exp2() * 4.0;
    let a = plane.point;
    let b = std::array::from_fn(|i| a[i] + u[i] * s);
    let c = std::array::from_fn(|i| a[i] + v[i] * s);
    [a, b, c]
}

fn c3(p: [f64; 3]) -> Coord3D<f64> {
    Coord3D {
        x: p[0],
        y: p[1],
        z: p[2],
    }
}

/// Refuses an open, non-manifold or inconsistently oriented model, which
/// has no inside to cap (`operand_not_closed`). Uses the mesh's cached
/// topology when there is one.
pub fn check_closed(mesh: &Mesh, ctx: &RunContext) -> Result<(), CutError> {
    ctx.progress.report(CHECKING, 0.0);
    ctx.checkpoint()?;
    let t = mesh.topology_with(ctx)?;
    ctx.progress.report(CHECKING, 1.0);
    if t.boundary_edges > 0 || !t.non_manifold_edges.is_empty() || t.inconsistent_edges > 0 {
        return Err(CutError::NotClosed {
            boundary_edges: t.boundary_edges,
            non_manifold_edges: t.non_manifold_edges.len(),
            inconsistent_edges: t.inconsistent_edges,
        });
    }
    Ok(())
}

/// Cuts a closed mesh through `plane` and caps both parts (gap 0). See the
/// module docs.
pub fn plane_cut(mesh: &Mesh, plane: Plane, ctx: &RunContext) -> Result<CutParts, CutError> {
    check_closed(mesh, ctx)?;
    let above = cut_half(mesh, &plane, Side::Above, ctx)?;
    let below = cut_half(mesh, &plane, Side::Below, ctx)?;
    Ok(CutParts {
        above: (above.mesh, above.provenance),
        below: (below.mesh, below.provenance),
        section_regions: below.section_regions,
        above_cap_faces: above.cap_faces,
        below_cap_faces: below.cap_faces,
    })
}

/// What one face chunk contributes to a half cut.
#[derive(Default)]
struct ChunkScan {
    /// Faces wholly on the discarded side, or lying in the plane.
    removed: Vec<FaceId>,
    /// Faces with corners strictly on both sides.
    crossing: Vec<(FaceId, [u32; 3])>,
    /// Directed edges in the plane of kept faces: `(from, to, face)`.
    plane_edges: Vec<(u32, u32, FaceId)>,
    /// On-plane vertices a kept face uses.
    used_on_plane: Vec<u32>,
}

/// One side of the cut of a **closed** mesh (the caller checks with
/// [`check_closed`]), capped. `keep` is the side kept; its cap faces away
/// from it.
pub fn cut_half(
    mesh: &Mesh,
    plane: &Plane,
    keep: Side,
    ctx: &RunContext,
) -> Result<HalfCut, CutError> {
    let frame = PlaneFrame::new(plane).ok_or(CutError::InvalidPlane)?;
    let [a, b, c] = exact_points(plane).map(c3);
    let keep_sign: i8 = match keep {
        Side::Above => 1,
        Side::Below => -1,
    };

    // 1. Every vertex's side, exactly.
    let slots = mesh.vertex_slots() as usize;
    let chunks = slots.div_ceil(CHUNK).max(1);
    let span = ctx.span(FINDING_THE_CUT, 0.0, 1.0, chunks);
    let mut side = vec![0_i8; slots];
    side.par_chunks_mut(CHUNK)
        .enumerate()
        .try_for_each(|(k, out)| -> Result<(), Cancelled> {
            span.step(k)?;
            for (i, s) in out.iter_mut().enumerate() {
                let v = (k * CHUNK + i) as u32;
                if !mesh.is_vertex_live(VertexId(v)) {
                    continue;
                }
                // Positive when the point is below the plane (a, b, c are
                // counter-clockwise seen from above).
                let o = orient3d(a, b, c, c3(mesh.corner_position(v)));
                *s = if o < 0.0 {
                    1
                } else if o > 0.0 {
                    -1
                } else {
                    0
                };
            }
            Ok(())
        })?;
    span.finish();
    let live = |v: usize| mesh.is_vertex_live(VertexId(v as u32));
    let any_kept = (0..slots)
        .into_par_iter()
        .any(|v| side[v] == keep_sign && live(v));
    let any_discarded = (0..slots)
        .into_par_iter()
        .any(|v| side[v] == -keep_sign && live(v));
    ctx.checkpoint()?;
    if !any_kept || !any_discarded {
        return Err(CutError::MissesObject);
    }

    // 2. Classify faces, per chunk, in parallel; concatenated in face order.
    let face_chunks = mesh.face_chunk_count();
    let span = ctx.span(CUTTING, 0.0, 0.8, face_chunks);
    let scans: Vec<ChunkScan> = (0..face_chunks)
        .into_par_iter()
        .map(|k| -> Result<ChunkScan, Cancelled> {
            span.step(k)?;
            let mut out = ChunkScan::default();
            for (f, tri) in mesh.faces_in_chunk(k) {
                let s = tri.map(|v| side[v as usize]);
                let kept = s.contains(&keep_sign);
                let discarded = s.contains(&-keep_sign);
                if kept && discarded {
                    out.crossing.push((f, tri));
                } else if discarded || s == [0, 0, 0] {
                    out.removed.push(f);
                } else {
                    for i in 0..3 {
                        if s[i] == 0 {
                            out.used_on_plane.push(tri[i]);
                            if s[(i + 1) % 3] == 0 {
                                out.plane_edges.push((tri[i], tri[(i + 1) % 3], f));
                            }
                        }
                    }
                }
            }
            Ok(out)
        })
        .collect::<Result<_, _>>()?;
    span.finish();

    // 3. Remove the discarded side and the crossing faces; split each
    // crossing face's kept side into pieces.
    let mut edit = mesh.edit();
    let mut plane_edges: Vec<(u32, u32, FaceId)> = Vec::new();
    let mut used_on_plane: HashSet<u32> = HashSet::new();
    let mut tick = ctx.ticker();
    for scan in &scans {
        for &f in &scan.removed {
            tick.tick()?;
            edit.remove_face(f)?;
        }
        for &(f, _) in &scan.crossing {
            edit.remove_face(f)?;
        }
        plane_edges.extend_from_slice(&scan.plane_edges);
        used_on_plane.extend(scan.used_on_plane.iter().copied());
    }
    ctx.checkpoint()?;
    let discarded: Vec<u32> = (0..slots)
        .into_par_iter()
        .filter(|&v| side[v] == -keep_sign && live(v))
        .map(|v| v as u32)
        .collect();
    for &v in &discarded {
        tick.tick()?;
        edit.remove_vertex(VertexId(v))?;
    }
    ctx.checkpoint()?;

    let axis = axis_of(frame.normal);
    // Cut points, by edge; their positions by `id - slots` (the edit
    // appends vertices after the input's slots, in order).
    let mut cut_points: HashMap<(u32, u32), u32> = HashMap::new();
    let mut new_positions: Vec<[f64; 3]> = Vec::new();
    let mut cut = |edit: &mut mf_geometry::MeshEdit, p: u32, q: u32| -> u32 {
        let (lo, hi) = (p.min(q), p.max(q));
        *cut_points.entry((lo, hi)).or_insert_with(|| {
            let point = edge_cut_point(
                &frame,
                axis,
                mesh.corner_position(lo),
                mesh.corner_position(hi),
            );
            new_positions.push(point);
            let id = edit.add_vertex(point).0;
            debug_assert_eq!(id as usize, slots + new_positions.len() - 1);
            id
        })
    };
    let mut pieces: Vec<[u32; 3]> = Vec::new();
    let mut piece_faces: Vec<FaceId> = Vec::new();
    for scan in &scans {
        for &(f, tri) in &scan.crossing {
            tick.tick()?;
            let s = tri.map(|v| side[v as usize]);
            let before = pieces.len();
            if let Some(z) = (0..3).find(|&i| s[i] == 0) {
                // One corner on the plane, the other two either side.
                let (zv, p, q) = (tri[z], tri[(z + 1) % 3], tri[(z + 2) % 3]);
                used_on_plane.insert(zv);
                let x = cut(&mut edit, p, q);
                if s[(z + 1) % 3] == keep_sign {
                    pieces.push([zv, p, x]);
                    plane_edges.push((x, zv, f));
                } else {
                    pieces.push([zv, x, q]);
                    plane_edges.push((zv, x, f));
                }
            } else {
                // One corner alone on its side.
                let lone = (0..3)
                    .find(|&i| s[i] != s[(i + 1) % 3] && s[i] != s[(i + 2) % 3])
                    .expect("a crossing face with no corner on the plane has a lone corner");
                let (l, m1, m2) = (tri[lone], tri[(lone + 1) % 3], tri[(lone + 2) % 3]);
                let x1 = cut(&mut edit, l, m1);
                let x2 = cut(&mut edit, l, m2);
                if s[lone] == keep_sign {
                    pieces.push([l, x1, x2]);
                    plane_edges.push((x1, x2, f));
                } else {
                    pieces.push([x1, m1, m2]);
                    pieces.push([x1, m2, x2]);
                    plane_edges.push((x2, x1, f));
                }
            }
            piece_faces.extend(std::iter::repeat_n(f, pieces.len() - before));
        }
    }
    // On-plane vertices that only faces lying in the plane used go with
    // those faces.
    let on_plane_unused: Vec<u32> = (0..slots)
        .into_par_iter()
        .filter(|&v| side[v] == 0 && live(v))
        .map(|v| v as u32)
        .filter(|v| !used_on_plane.contains(v))
        .collect();
    for &v in &on_plane_unused {
        edit.remove_vertex(VertexId(v))?;
    }
    let pos = |v: u32| -> [f64; 3] {
        if (v as usize) < slots {
            mesh.corner_position(v)
        } else {
            new_positions[v as usize - slots]
        }
    };
    for (tri, f) in pieces.iter().zip(&piece_faces) {
        tick.tick()?;
        if degenerate(tri.map(pos)) {
            return Err(CutError::SectionSelfIntersects { at: pos(tri[0]) });
        }
        edit.add_face(
            *tri,
            Sources {
                faces: vec![*f],
                ..Sources::default()
            },
        )?;
    }
    ctx.checkpoint()?;

    // 4. The boundary left in the plane, as loops.
    let span = ctx.span(CAPPING, 0.0, 1.0, 1);
    let loops = boundary_loops(&plane_edges, &pos)?;
    let mut cap_faces = Vec::new();
    let mut section_regions = Vec::new();
    if !loops.is_empty() {
        // Counter-clockwise outer loops seen from above: the part below's
        // boundary runs the other way round its cap.
        let oriented: Vec<Vec<(u32, FaceId)>> = loops
            .into_iter()
            .map(|mut l| {
                if keep == Side::Below {
                    l.reverse();
                    // Reversed, each vertex's outgoing edge's source is the
                    // next one's; the cap only needs the set per vertex.
                }
                l
            })
            .collect();
        let flat: Vec<(u32, FaceId)> = oriented.iter().flatten().copied().collect();
        let loops2d: Vec<Vec<[f64; 2]>> = oriented
            .iter()
            .map(|l| l.iter().map(|&(v, _)| frame.to_2d(pos(v))).collect())
            .collect();
        ctx.checkpoint()?;
        let tris = cap_triangles(&loops2d)
            .map_err(|k| CutError::SectionSelfIntersects { at: pos(flat[k].0) })?;
        ctx.checkpoint()?;
        for t in tris {
            let mut tri = t.map(|k| flat[k].0);
            if keep == Side::Above {
                tri.swap(1, 2);
            }
            let mut sources: Vec<FaceId> = t.iter().map(|&k| flat[k].1).collect();
            sources.sort_unstable();
            sources.dedup();
            cap_faces.push(edit.add_face(
                tri,
                Sources {
                    faces: sources,
                    ..Sources::default()
                },
            )?);
        }
        section_regions = regions(&loops2d, frame);
    }
    span.finish();
    let (mesh, provenance) = edit.finish()?;
    Ok(HalfCut {
        mesh,
        provenance,
        cap_faces,
        section_regions,
    })
}

/// Where the plane crosses the edge `lo`→`hi` (vertex ids ascending, so
/// both faces on the edge get the same point). On an axis-aligned plane
/// the axis coordinate is the plane's, exactly.
fn edge_cut_point(frame: &PlaneFrame, axis: Option<usize>, x: [f64; 3], y: [f64; 3]) -> [f64; 3] {
    let dx = dot(sub(x, frame.origin), frame.normal);
    let dy = dot(sub(y, frame.origin), frame.normal);
    let denom = dx - dy;
    let t = if denom != 0.0 {
        (dx / denom).clamp(0.0, 1.0)
    } else {
        0.5
    };
    let mut point: [f64; 3] = std::array::from_fn(|i| x[i] + t * (y[i] - x[i]));
    if let Some(i) = axis {
        point[i] = frame.origin[i];
    }
    point
}

/// True if a triangle has no area: its corners are collinear (exactly:
/// every axis projection is), including a repeated position.
fn degenerate(p: [[f64; 3]; 3]) -> bool {
    let proj = |i: usize, j: usize| {
        orient2d(
            Coord {
                x: p[0][i],
                y: p[0][j],
            },
            Coord {
                x: p[1][i],
                y: p[1][j],
            },
            Coord {
                x: p[2][i],
                y: p[2][j],
            },
        ) == 0.0
    };
    proj(0, 1) && proj(1, 2) && proj(2, 0)
}

/// The directed plane edges that don't cancel against a reverse edge,
/// traced into loops of `(vertex, source face)`. A doubled edge, a vertex
/// with two ways out (two loops pinched together) or a dead end makes the
/// section ambiguous.
fn boundary_loops(
    edges: &[(u32, u32, FaceId)],
    pos: &dyn Fn(u32) -> [f64; 3],
) -> Result<Vec<Vec<(u32, FaceId)>>, CutError> {
    let bad = |v: u32| CutError::SectionSelfIntersects { at: pos(v) };
    let mut count: HashMap<(u32, u32), (u32, FaceId)> = HashMap::with_capacity(edges.len());
    for &(u, v, f) in edges {
        let e = count.entry((u, v)).or_insert((0, f));
        e.0 += 1;
        if e.0 > 1 {
            return Err(bad(u));
        }
    }
    let mut boundary: Vec<(u32, u32, FaceId)> = count
        .iter()
        .filter(|((u, v), _)| !count.contains_key(&(*v, *u)))
        .map(|(&(u, v), &(_, f))| (u, v, f))
        .collect();
    boundary.sort_unstable_by_key(|&(u, v, _)| (u, v));
    let mut next: HashMap<u32, (u32, FaceId)> = HashMap::with_capacity(boundary.len());
    for &(u, v, f) in &boundary {
        if next.insert(u, (v, f)).is_some() {
            return Err(bad(u));
        }
    }
    let mut seen: HashSet<u32> = HashSet::with_capacity(boundary.len());
    let mut loops = Vec::new();
    for &(start, _, _) in &boundary {
        if seen.contains(&start) {
            continue;
        }
        let mut l = Vec::new();
        let mut at = start;
        loop {
            if !seen.insert(at) {
                return Err(bad(at));
            }
            let &(to, f) = next.get(&at).ok_or_else(|| bad(at))?;
            l.push((at, f));
            at = to;
            if at == start {
                break;
            }
        }
        loops.push(l);
    }
    Ok(loops)
}

/// The cap's triangles (D3), from the one section triangulation shared
/// with the boolean window ([`crate::triangulate_section`]), or the index
/// of a point near the fault.
fn cap_triangles(loops: &[Vec<[f64; 2]>]) -> Result<Vec<[usize; 3]>, usize> {
    crate::section::triangulate_section_at(loops)
        .map(|t| t.into_iter().map(|t| t.map(|i| i as usize)).collect())
        .map_err(|(_, k)| k)
}

fn signed_area(l: &[[f64; 2]]) -> f64 {
    let n = l.len();
    (0..n)
        .map(|i| {
            let (a, b) = (l[i], l[(i + 1) % n]);
            a[0] * b[1] - a[1] * b[0]
        })
        .sum::<f64>()
        / 2.0
}

fn contains(l: &[[f64; 2]], p: [f64; 2]) -> bool {
    let n = l.len();
    let mut inside = false;
    for i in 0..n {
        let (a, b) = (l[i], l[(i + 1) % n]);
        if (a[1] > p[1]) != (b[1] > p[1]) {
            let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if p[0] < x {
                inside = !inside;
            }
        }
    }
    inside
}

/// The section's regions: each counter-clockwise loop with the clockwise
/// loops directly inside it (the smallest outer loop containing them).
fn regions(loops: &[Vec<[f64; 2]>], frame: PlaneFrame) -> Vec<SectionRegion> {
    let areas: Vec<f64> = loops.iter().map(|l| signed_area(l)).collect();
    let outers: Vec<usize> = (0..loops.len()).filter(|&i| areas[i] > 0.0).collect();
    let mut out: Vec<SectionRegion> = outers
        .iter()
        .map(|&i| SectionRegion {
            outer: loops[i].clone(),
            holes: Vec::new(),
            frame,
        })
        .collect();
    for (h, l) in loops.iter().enumerate() {
        if areas[h] > 0.0 {
            continue;
        }
        let owner = outers
            .iter()
            .enumerate()
            .filter(|&(_, &o)| contains(&loops[o], l[0]))
            .min_by(|a, b| areas[*a.1].total_cmp(&areas[*b.1]))
            .map(|(k, _)| k);
        // The cap's winding check has already proved every hole lies in
        // an outer loop.
        if let Some(k) = owner {
            out[k].holes.push(l.clone());
        }
    }
    out
}
