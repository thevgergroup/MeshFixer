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
//! - **On-plane vertices break ties with a fixed symbolic perturbation**
//!   (simulation of simplicity, [`SOS_SIDE`]): for deciding which side
//!   *owns* a face, a zero side counts as [`SOS_SIDE`], always, whichever
//!   side is being kept. A face with every corner on the plane (a cube's
//!   top face, cut exactly along it) is then a real face of the
//!   [`SOS_SIDE`] part, unchanged, not a cap — the other part simply
//!   doesn't reach the plane there. This is what makes a plane that lies
//!   exactly on mesh faces never refused (owner 2026-11-12): coplanar
//!   faces resolve to one side consistently, so the section stays
//!   well-defined even with nothing strictly crossing. A plane that only
//!   grazes an edge or a lone vertex, with every face around it owned
//!   by the other side, keeps nothing real either way: that's still
//!   [`CutError::MissesObject`] (its `grazed` flag says which case it
//!   was, for a caller like `mesh.split_plane` that only cares that
//!   *something* was kept).
//! - **The section's identity is topological.** Each crossing edge is
//!   cut once, keyed by its vertex ids: one cut vertex per crossing edge,
//!   shared by both faces on it, plus the input vertices on the plane,
//!   which *are* their edges' crossings. Nothing is identified by its
//!   coordinates. A cut point is interpolated once per edge, from the end
//!   nearer the plane (exact distances for an axis-aligned plane,
//!   `orient3d` otherwise), the same for both sides; for an axis-aligned
//!   normal its axis coordinate is the plane's exactly, so horizontal and
//!   vertical caps are exactly flat.
//! - **Classification and construction agree** (split-harden,
//!   2026-11-13). Where the plane passes a vertex closer than its
//!   coordinates resolve, a cut point on an edge from it can't lie
//!   strictly between the edge's ends (its offset rounds away in some
//!   coordinate), and a piece of a face there folds or flattens. Where
//!   both hold — exact tests on the f64 points, no tolerance — that
//!   vertex is on the plane for this cut, as if `orient3d` had said zero
//!   (the owner's scan, whose equator ring sits 1.2e-15 above the plane;
//!   a box corner a few ulps off a plane through it). Decided from both
//!   sides' pieces, so the same for either side. The plane is never
//!   moved.
//! - **Loops are traced on the topology.** A cut point has one way in
//!   and one way out; an on-plane vertex the section passes more than
//!   once pairs each way in with the next way out round the input mesh's
//!   own fan of faces there, across the gap the cap fills. Two cut points
//!   next to each other on the section at exactly the same f64 point are
//!   one point (the edge between them collapsed, only where that keeps
//!   the part manifold).
//! - **Caps** close the boundary loops. They are triangulated by a
//!   constrained Delaunay triangulation (`spade`, D3) in the plane's
//!   exact projection, with nesting by orientation (a winding number per
//!   triangle), so a hollow model's section is an annulus. Loops touching
//!   at an on-plane vertex (two shells meeting there, or one loop
//!   pinched) share it. Where rounding still folds the section (a spike
//!   or kink an ulp across) and the input faces the plane passes through
//!   don't genuinely cross, the cap cuts the folded point off as an ear
//!   and triangulates the rest: valid input is never refused. A section
//!   whose loops genuinely cross — the input faces the plane passes
//!   through intersect one another — is refused as
//!   [`CutError::SectionSelfIntersects`] (Make solid fixes the model).
//!   Every edge the cut made is checked to have exactly two faces, wound
//!   opposite ways, before a part is returned
//!   ([`CutError::SectionDegenerate`] would name a fault in this code;
//!   it is never how valid input is refused).
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
    /// A stable identity for this region (Phase 1b, cross-section
    /// picking): the lowest input-mesh [`FaceId`] among the crossing
    /// faces that contributed to the region's outer loop. Input face
    /// ids never change for faces away from the plane, so this id is
    /// the same across a replay of the same cut, letting a caller name
    /// one region (`mesh.split_plane`'s `regions` param) by this value
    /// alone, recorded once and replayed exactly.
    pub seed_face: FaceId,
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
    /// Each `section_regions` entry's own input (pre-cut) crossing
    /// faces, same length and order as `section_regions` (Phase 1b:
    /// [`cut_half_with_regions`]'s pass 1 uses this to build its
    /// exclusion set). Empty whenever `section_regions` is (uncapped,
    /// or no boundary loops at all).
    pub region_crossing_faces: Vec<HashSet<FaceId>>,
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
    /// `grazed` is true when the plane touches the model's surface
    /// exactly (some vertex has a real side of exactly zero) even
    /// though nothing is strictly on this side: a box's top face, cut
    /// exactly along it, `MissesObject`s on the "above" side with
    /// `grazed: true`, not a genuine total miss. A caller content with
    /// one side existing (`keep: both`) reads this as "nothing to keep
    /// here", not a refusal; a caller that asked for just this side
    /// still refuses, by name.
    #[error("the plane doesn't cut the object: all of it is on one side")]
    MissesObject { grazed: bool },
    /// The section's loops genuinely cross near `at`: the input faces the
    /// plane crosses there intersect one another (the model's surface
    /// crosses itself, which Make solid fixes).
    #[error("the section crosses itself near {at:?}: the model's surface crosses itself there")]
    SectionSelfIntersects { at: [f64; 3] },
    /// A part the cut built near `at` failed its own closed-and-manifold
    /// check (an edge it made without exactly two faces wound opposite
    /// ways). The cut's construction guarantees that check on valid
    /// closed input (split-harden, 2026-11-13: the section is built on
    /// the mesh's topology, never refused for rounding), so this names a
    /// fault in the cut itself, refused rather than written as a part that
    /// isn't closed and manifold.
    #[error(
        "the cut lies along the model's surface near {at:?}, closer than its coordinates can separate"
    )]
    SectionDegenerate { at: [f64; 3] },
    /// Phase 1b, the bounded cut plane: a section region's outer loop
    /// straddles the bound's edge (some of it inside, some outside) —
    /// ambiguous which way it should go, so the cut refuses by name
    /// rather than guessing. `seed_face` is that region's stable
    /// identity ([`SectionRegion::seed_face`]).
    #[error("a cut region crosses the cut area's edge")]
    RegionCrossesBound { seed_face: FaceId },
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

/// The fixed side a zero (on-plane) vertex counts as, for deciding which
/// side owns a face that doesn't otherwise cross the plane (simulation of
/// simplicity): always [`Side::Below`]'s sign, whichever side is being
/// kept. A face every one of whose corners is on the plane then belongs
/// entirely to the Below part, as a real face, never duplicated into
/// Above and never left to a cap that would otherwise never get made (no
/// genuinely crossing face means no boundary loop to cap it with).
const SOS_SIDE: i8 = -1;

/// What one face chunk contributes to a half cut.
#[derive(Default)]
struct ChunkScan {
    /// Faces wholly on the discarded side, or lying wholly in the plane
    /// and owned by the other side (see [`SOS_SIDE`]).
    removed: Vec<FaceId>,
    /// Faces with corners strictly on both real sides (never on-plane
    /// only: an all-zero face is owned by [`SOS_SIDE`] directly, above).
    crossing: Vec<(FaceId, [u32; 3])>,
    /// Directed edges in the plane of kept faces: `(from, to, face)`.
    plane_edges: Vec<(u32, u32, FaceId)>,
    /// On-plane vertices a kept face uses.
    used_on_plane: Vec<u32>,
    /// How many faces this chunk keeps whole or splits (not removed):
    /// whether this side ends up with any real content at all. A vertex
    /// — even a whole edge — exactly on the plane with every face
    /// around it owned by the other side isn't enough (owner
    /// 2026-11-12): nothing would actually be kept.
    kept_faces: usize,
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
    cut_half_with(mesh, plane, keep, true, ctx)
}

/// As [`cut_half`], but a vase or bowl (owner 2026-11-12: "Cap the
/// cut") can ask for `cap: false`: the part is left open at the cut —
/// no cap faces, no section regions, a real boundary where the plane
/// crossed it, exactly as an opening the model already had. Finding the
/// cut and splitting crossing faces happen the same either way; only
/// step 4 (closing the boundary with a cap) is skipped.
pub fn cut_half_with(
    mesh: &Mesh,
    plane: &Plane,
    keep: Side,
    cap: bool,
    ctx: &RunContext,
) -> Result<HalfCut, CutError> {
    cut_half_impl(mesh, plane, keep, cap, None, ctx)
}

/// A bounded cut area (Phase 1b, "Limit to area"): a disc or rectangle
/// in the plane's own 2D frame, centred at the frame's origin (the
/// caller positions it by choosing the plane's point). Only a region
/// whose outer loop lies entirely inside the bound is cut; one entirely
/// outside is left whole (as if unticked); one straddling the edge is
/// refused by name ([`CutError::RegionCrossesBound`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Bound {
    Disc { radius: f64 },
    Rect { width: f64, height: f64 },
}

/// How one [`SectionRegion`]'s outer loop relates to a [`Bound`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundCheck {
    AllInside,
    AllOutside,
    Crosses,
}

impl Bound {
    /// Whether `[x, y]` (plane frame coordinates) is strictly inside.
    /// Exactly on the edge is never "inside" (Phase 1b: a region that
    /// only touches the bound's edge, with no point strictly inside,
    /// must not read as cleanly contained).
    fn strictly_inside(&self, p: [f64; 2]) -> bool {
        match *self {
            Bound::Disc { radius } => p[0] * p[0] + p[1] * p[1] < radius * radius,
            Bound::Rect { width, height } => p[0].abs() < width / 2.0 && p[1].abs() < height / 2.0,
        }
    }

    /// Whether `[x, y]` is strictly outside (the mirror of
    /// [`Self::strictly_inside`] with the inequality reversed — a point
    /// exactly on the edge is neither strictly inside nor strictly
    /// outside, which is what makes a loop that only touches the edge
    /// classify as [`BoundCheck::Crosses`], never [`BoundCheck::
    /// AllInside`] or [`BoundCheck::AllOutside`]).
    fn strictly_outside(&self, p: [f64; 2]) -> bool {
        match *self {
            Bound::Disc { radius } => p[0] * p[0] + p[1] * p[1] > radius * radius,
            Bound::Rect { width, height } => p[0].abs() > width / 2.0 || p[1].abs() > height / 2.0,
        }
    }
}

/// Classifies `region`'s outer loop against `bound` (Phase 1b):
/// [`BoundCheck::AllInside`] only when every point is strictly inside;
/// [`BoundCheck::AllOutside`] only when every point is strictly outside;
/// [`BoundCheck::Crosses`] otherwise (including a loop that only ever
/// touches the bound's edge, with no point strictly on either side).
pub fn classify_region(region: &SectionRegion, bound: &Bound) -> BoundCheck {
    let all_in = region.outer.iter().all(|&p| bound.strictly_inside(p));
    if all_in {
        return BoundCheck::AllInside;
    }
    let all_out = region.outer.iter().all(|&p| bound.strictly_outside(p));
    if all_out {
        return BoundCheck::AllOutside;
    }
    BoundCheck::Crosses
}

/// As [`cut_half_with`], restricted to named regions and/or a bound
/// (Phase 1b: cross-section picking and the bounded cut plane).
///
/// - `regions`: `None` or `Some(&[])` cuts every region (today's
///   behaviour). `Some(seeds)` cuts only the regions whose
///   [`SectionRegion::seed_face`] is in `seeds`; every other region is
///   left whole — the plane simply doesn't cut the object there, so
///   that part of the model stays in one piece across the plane,
///   uncapped and unseamed (not a smaller capped remainder). A seed
///   face that doesn't match any real region (stale or unknown) is
///   silently inert: nothing matches it, so nothing is cut because of
///   it — never a panic.
/// - `bound`: when given, a region whose outer loop lies entirely
///   inside the bound ([`BoundCheck::AllInside`]) is still subject to
///   `regions`; one entirely outside ([`BoundCheck::AllOutside`]) is
///   left whole, exactly like an unticked region; one that straddles
///   the bound's edge ([`BoundCheck::Crosses`]) refuses the whole call
///   with [`CutError::RegionCrossesBound`], naming that region's seed
///   face.
///
/// Implementation note: finding which regions exist, and which input
/// faces belong to each, needs the cut's own boundary loops — not known
/// until every crossing face is classified. So when either filter is
/// active this cuts the mesh twice: pass 1 ([`cut_half_impl`] with no
/// exclusion) to learn each region's seed face and member crossing
/// faces ([`HalfCut::region_crossing_faces`]); pass 2, the one result
/// returned, with the excluded regions' crossing faces turned into
/// "kept whole" or "removed" rather than split. Acceptable for Phase
/// 1b; a future pass could avoid the first cut if profiling on a large
/// mesh ever shows this matters.
pub fn cut_half_with_regions(
    mesh: &Mesh,
    plane: &Plane,
    keep: Side,
    cap: bool,
    regions: Option<&[FaceId]>,
    bound: Option<&Bound>,
    ctx: &RunContext,
) -> Result<HalfCut, CutError> {
    let no_regions_filter = regions.is_none_or(<[FaceId]>::is_empty);
    if no_regions_filter && bound.is_none() {
        return cut_half_impl(mesh, plane, keep, cap, None, ctx);
    }
    // Pass 1, cap forced on regardless of `cap` (section regions are
    // only ever computed while capping), purely to learn each region's
    // membership.
    let learn = cut_half_impl(mesh, plane, keep, true, None, ctx)?;
    let mut excluded: HashSet<FaceId> = HashSet::new();
    for (region, crossing_faces) in learn
        .section_regions
        .iter()
        .zip(&learn.region_crossing_faces)
    {
        let bound_check = bound.map(|b| classify_region(region, b));
        if let Some(BoundCheck::Crosses) = bound_check {
            return Err(CutError::RegionCrossesBound {
                seed_face: region.seed_face,
            });
        }
        let outside_bound = bound_check == Some(BoundCheck::AllOutside);
        let not_ticked =
            !no_regions_filter && !regions.is_some_and(|seeds| seeds.contains(&region.seed_face));
        if outside_bound || not_ticked {
            excluded.extend(crossing_faces.iter().copied());
        }
    }
    if excluded.is_empty() {
        return cut_half_impl(mesh, plane, keep, cap, None, ctx);
    }
    cut_half_impl(mesh, plane, keep, cap, Some(&excluded), ctx)
}

fn cut_half_impl(
    mesh: &Mesh,
    plane: &Plane,
    keep: Side,
    cap: bool,
    exclude: Option<&HashSet<FaceId>>,
    ctx: &RunContext,
) -> Result<HalfCut, CutError> {
    // `SectionSelfIntersects` is only ever raised once
    // `crossing_faces_intersect` has said the input genuinely crosses
    // (`cap_triangles`).
    cut_half_core(mesh, plane, keep, cap, exclude, ctx)
}

/// Whether the input faces the plane passes through (a corner on each
/// side, or on it) intersect one another, by the exact test
/// [`crate::validate_solid`] uses: what tells a section that genuinely
/// crosses itself (the model's surface crosses itself there; Make solid
/// fixes it) from one that rounding folded (which the cap repairs). Only
/// run once a section's triangulation has failed.
fn crossing_faces_intersect(
    mesh: &Mesh,
    plane: &Plane,
    ctx: &RunContext,
) -> Result<bool, CutError> {
    let [a, b, c] = exact_points(plane).map(c3);
    let side = |v: u32| {
        let o = orient3d(a, b, c, c3(mesh.corner_position(v)));
        if o < 0.0 {
            1
        } else if o > 0.0 {
            -1
        } else {
            0
        }
    };
    let faces: Vec<[u32; 3]> = (0..mesh.face_chunk_count())
        .into_par_iter()
        .flat_map_iter(|k| {
            mesh.faces_in_chunk(k)
                .filter(|&(_, t)| {
                    let s = t.map(side);
                    s.contains(&0) || (s.contains(&1) && s.contains(&-1))
                })
                .map(|(_, t)| t)
                .collect::<Vec<_>>()
        })
        .collect();
    let mut index: HashMap<u32, u32> = HashMap::new();
    let mut positions: Vec<[f64; 3]> = Vec::new();
    let triangles: Vec<[u32; 3]> = faces
        .iter()
        .map(|t| {
            t.map(|v| {
                *index.entry(v).or_insert_with(|| {
                    positions.push(mesh.corner_position(v));
                    positions.len() as u32 - 1
                })
            })
        })
        .collect();
    match crate::validate::self_intersecting_faces(&positions, &triangles, ctx) {
        Ok(flags) => Ok(flags.into_iter().any(|f| f)),
        Err(crate::SolidError::Cancelled) => Err(CutError::Cancelled),
        Err(_) => Ok(false),
    }
}

fn cut_half_core(
    mesh: &Mesh,
    plane: &Plane,
    keep: Side,
    cap: bool,
    exclude: Option<&HashSet<FaceId>>,
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
    ctx.checkpoint()?;

    // 1b. Phase 1b (region picking/bound): an excluded region's whole
    // connected island — every face reachable from its crossing faces
    // by a shared vertex, not just the crossing faces themselves — is
    // kept entirely unmodified, on whichever side has the majority of
    // its vertices (a deterministic, side-agnostic computation: both
    // the `Side::Above` and `Side::Below` calls for the same `exclude`
    // set compute the same island and the same majority side, so the
    // island appears whole on exactly one of the two resulting parts,
    // never duplicated into both and never split between them).
    let excluded_island: HashSet<FaceId> = match exclude {
        Some(seeds) if !seeds.is_empty() => expand_island(mesh, seeds),
        _ => HashSet::new(),
    };
    let island_favours_keep = |side: &[i8]| -> bool {
        if excluded_island.is_empty() {
            return false;
        }
        let mut majority = 0_i64;
        for &f in &excluded_island {
            if let Some(tri) = mesh.face(f) {
                for v in tri {
                    majority += i64::from(side[v as usize]);
                }
            }
        }
        // Fixed, side-independent tie-break (never consults
        // `keep_sign`) so the `Side::Above` and `Side::Below` calls for
        // the same `exclude` set always agree on which single side gets
        // the island whole: positive majority (more vertices on
        // [`Side::Above`]'s side) keeps it on Above; negative on
        // Below; an exact tie (a symmetric island) always resolves to
        // Above, by convention.
        let island_side_sign: i8 = if majority < 0 { -1 } else { 1 };
        island_side_sign == keep_sign
    };

    // 2. Classify faces, per chunk, in parallel; concatenated in face order.
    let scan_faces =
        |side: &[i8], island_favours_keep: bool| -> Result<Vec<ChunkScan>, Cancelled> {
            let face_chunks = mesh.face_chunk_count();
            let span = ctx.span(CUTTING, 0.0, 0.8, face_chunks);
            let scans = (0..face_chunks)
                .into_par_iter()
                .map(|k| -> Result<ChunkScan, Cancelled> {
                    span.step(k)?;
                    let mut out = ChunkScan::default();
                    for (f, tri) in mesh.faces_in_chunk(k) {
                        if excluded_island.contains(&f) {
                            // Phase 1b: this face's region is being left whole
                            // — never split, never a source of boundary/plane
                            // edges, regardless of which side(s) its own
                            // corners happen to be on. It goes wholly to the
                            // side `island_favours_keep` already chose (the
                            // same choice both the Above and Below calls make
                            // independently, so the island lands on exactly
                            // one of the two final parts).
                            if island_favours_keep {
                                out.kept_faces += 1;
                            } else {
                                out.removed.push(f);
                            }
                            continue;
                        }
                        let s = tri.map(|v| side[v as usize]);
                        if s == [0, 0, 0] {
                            // Lying wholly in the plane: owned by SOS_SIDE,
                            // unchanged (a real face there, not a cap), removed
                            // otherwise. Never duplicated, never left to a cap
                            // that genuinely crossing faces might not exist to
                            // form.
                            if keep_sign == SOS_SIDE {
                                out.kept_faces += 1;
                                out.used_on_plane.extend_from_slice(&tri);
                                // Its own three edges, in the triangle's stored
                                // winding: each exactly cancels, in
                                // `boundary_loops`, the reverse edge a
                                // neighbouring kept side face emits along the
                                // same on-plane edge. What's left of a closed,
                                // already-capped region is nothing to retrace —
                                // only a genuinely open boundary (where no such
                                // neighbour exists) survives to be capped.
                                for i in 0..3 {
                                    out.plane_edges.push((tri[i], tri[(i + 1) % 3], f));
                                }
                            } else {
                                out.removed.push(f);
                            }
                            continue;
                        }
                        let kept = s.contains(&keep_sign);
                        let discarded = s.contains(&-keep_sign);
                        if kept && discarded {
                            out.kept_faces += 1;
                            out.crossing.push((f, tri));
                        } else if discarded {
                            out.removed.push(f);
                        } else {
                            out.kept_faces += 1;
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
                .collect::<Result<Vec<_>, _>>()?;
            span.finish();
            Ok(scans)
        };
    let mut favours = island_favours_keep(&side);
    let mut scans = scan_faces(&side, favours)?;

    // 2b. Where the plane crosses each crossing edge: computed once per
    // edge, from its own two ends (`edge_cut_point`, interpolated from the
    // end nearer the plane), so every face on the edge, on either side
    // and in either part, gets the same point. Which faces cross is the
    // same whichever side is kept, so this is too.
    let axis = axis_of(frame.normal);
    // Each vertex's distance from the plane, up to one positive scale
    // shared by every vertex: exactly `p[axis] - plane` for an
    // axis-aligned plane (rounded once at most), otherwise the adaptive-
    // exact `orient3d` determinant itself, whose sign is the vertex's
    // exact side and whose magnitude is accurate even for a vertex an
    // ulp from the plane (where a dot product's own rounding swamps the
    // true distance).
    let distance = |p: [f64; 3]| -> f64 {
        match axis {
            Some(i) => (p[i] - frame.origin[i]).abs(),
            None => orient3d(a, b, c, c3(p)).abs(),
        }
    };
    let mut cut_at: HashMap<(u32, u32), [f64; 3]> = HashMap::new();
    for scan in &scans {
        for &(_, tri) in &scan.crossing {
            for i in 0..3 {
                let (p, q) = (tri[i], tri[(i + 1) % 3]);
                if side[p as usize] * side[q as usize] < 0 {
                    cut_at.entry((p.min(q), p.max(q))).or_insert_with(|| {
                        let (x, y) = (
                            mesh.corner_position(p.min(q)),
                            mesh.corner_position(p.max(q)),
                        );
                        edge_cut_point(&frame, axis, x, y, distance(x), distance(y))
                    });
                }
            }
        }
    }
    // 2c. Classification and construction agree. A crossing face is split
    // into a piece on each side (a triangle and a quad, or two triangles
    // where a corner is on the plane), and with the cut points' f64
    // positions every piece must still face the way the face does (exact
    // `orient2d` in the face's own projection). Rounding can break that
    // only where the plane passes a vertex closer than its coordinates
    // resolve: the cut point on an edge from it then can't lie strictly
    // between the edge's ends — its offset from that end rounds away in
    // some coordinate in which the ends differ — and the piece folds or
    // flattens (the owner's scan, its equator ring 1.2e-15 above the
    // plane; a box corner a few ulps off a plane through it). Where both
    // hold, that end is where the plane crosses: it is *on* the plane for
    // this cut, exactly as if `orient3d` had said zero, every edge at it
    // is cut at it, and the faces round it resolve by `SOS_SIDE` like any
    // on-plane vertex. Exact tests on the edges' own f64 points only —
    // no tolerance, the plane not moved — and both sides' pieces checked,
    // so the same vertices for either side. Making a vertex on-plane only
    // ever removes crossings (an edge from it is cut at the vertex itself,
    // not at a new point): repeated until no piece folds, it ends.
    let lost: HashMap<(u32, u32), u32> = cut_at
        .iter()
        .filter_map(|(&(lo, hi), p)| {
            let (x, y) = (mesh.corner_position(lo), mesh.corner_position(hi));
            let strictly_between =
                (0..3).all(|i| x[i] == y[i] || (p[i] > x[i].min(y[i]) && p[i] < x[i].max(y[i])));
            // The end it was interpolated from (`edge_cut_point`'s choice).
            (!strictly_between)
                .then(|| ((lo, hi), if distance(x) <= distance(y) { lo } else { hi }))
        })
        .collect();
    let mut onto_any = false;
    if !lost.is_empty() {
        let crossing: Vec<[u32; 3]> = scans
            .iter()
            .flat_map(|s| s.crossing.iter().map(|&(_, t)| t))
            .collect();
        loop {
            let mut onto: Vec<u32> = Vec::new();
            for &tri in &crossing {
                let s = tri.map(|v| side[v as usize]);
                if !(s.contains(&1) && s.contains(&-1)) {
                    continue;
                }
                let edge = |p: u32, q: u32| (p.min(q), p.max(q));
                let near: Vec<u32> = (0..3)
                    .filter_map(|i| lost.get(&edge(tri[i], tri[(i + 1) % 3])).copied())
                    .collect();
                if !near.is_empty() && !pieces_face_their_face(mesh, tri, &s, &cut_at) {
                    onto.extend(near);
                }
            }
            onto.retain(|&v| side[v as usize] != 0);
            onto.sort_unstable();
            onto.dedup();
            if onto.is_empty() {
                break;
            }
            for &v in &onto {
                side[v as usize] = 0;
            }
            onto_any = true;
        }
    }
    if onto_any {
        favours = island_favours_keep(&side);
        scans = scan_faces(&side, favours)?;
    }
    let island_favours_keep = favours;
    let side = side;
    // Nothing kept, by actual face content — not merely a vertex or
    // edge exactly on the plane with every face around it owned by the
    // other side (owner 2026-11-12: that's still nothing to cut here).
    // `grazed` (the plane touches the model's surface exactly somewhere,
    // whether or not it's this side) tells the caller whether this is a
    // "this side is empty, but the cut is otherwise valid" result, or a
    // genuine total miss.
    if scans.iter().all(|s| s.kept_faces == 0) {
        let grazed = (0..slots).into_par_iter().any(|v| side[v] == 0 && live(v));
        return Err(CutError::MissesObject { grazed });
    }

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
    // A kept-whole island face's own vertices must survive even on
    // the discarded side's sign (Phase 1b): the face still references
    // them, whichever side of the plane they happen to sit on.
    let island_vertices: HashSet<u32> = if excluded_island.is_empty() || !island_favours_keep {
        HashSet::new()
    } else {
        excluded_island
            .iter()
            .filter_map(|&f| mesh.face(f))
            .flatten()
            .collect()
    };
    let discarded: Vec<u32> = (0..slots)
        .into_par_iter()
        .filter(|&v| side[v] == -keep_sign && live(v) && !island_vertices.contains(&(v as u32)))
        .map(|v| v as u32)
        .collect();
    for &v in &discarded {
        tick.tick()?;
        edit.remove_vertex(VertexId(v))?;
    }
    ctx.checkpoint()?;

    // Cut points: one vertex per crossing edge, keyed by the edge (its
    // identity is the edge, never its coordinates); positions by `id -
    // slots` (the edit appends vertices after the input's slots, in
    // order). An edge with an end on the plane isn't cut: that end is
    // its crossing.
    let mut cut_points: HashMap<(u32, u32), u32> = HashMap::new();
    let mut new_positions: Vec<[f64; 3]> = Vec::new();
    let mut cut = |edit: &mut mf_geometry::MeshEdit, p: u32, q: u32| -> u32 {
        let key = (p.min(q), p.max(q));
        *cut_points.entry(key).or_insert_with(|| {
            // Every edge crossing now crossed before 2c (making a vertex
            // on-plane only removes crossings), so its point is known.
            let point = cut_at[&key];
            new_positions.push(point);
            let id = edit.add_vertex(point).0;
            debug_assert_eq!(id as usize, slots + new_positions.len() - 1);
            id
        })
    };
    let mut pieces: Vec<[u32; 3]> = Vec::new();
    let mut piece_faces: Vec<FaceId> = Vec::new();
    // A crossing face whose lone corner is discarded keeps a quad, `[x1,
    // m1, m2, x2]` round the face, split below by whichever diagonal
    // keeps both triangles facing the way the face did.
    let mut quads: Vec<([u32; 4], [u32; 3], FaceId)> = Vec::new();
    for scan in &scans {
        for &(f, tri) in &scan.crossing {
            tick.tick()?;
            let s = tri.map(|v| side[v as usize]);
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
                piece_faces.push(f);
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
                    piece_faces.push(f);
                    plane_edges.push((x1, x2, f));
                } else {
                    quads.push(([x1, m1, m2, x2], tri, f));
                    plane_edges.push((x2, x1, f));
                }
            }
        }
    }
    // On-plane vertices that only faces lying in the plane used go with
    // those faces.
    let on_plane_unused: Vec<u32> = (0..slots)
        .into_par_iter()
        .filter(|&v| side[v] == 0 && live(v))
        .map(|v| v as u32)
        .filter(|v| !used_on_plane.contains(v) && !island_vertices.contains(v))
        .collect();
    for &v in &on_plane_unused {
        edit.remove_vertex(VertexId(v))?;
    }
    // An input vertex this close to the plane is on it for this cut
    // (`side[v] == 0`, SOS), but its own stored position is whatever the
    // input gave it — an input built from trigonometry (a sphere's own
    // vertices, say) is essentially never *exactly* zero in the plane's
    // own distance, only within rounding of it. Left alone, the cap
    // (and the kept side's own rim) would carry that vertex a few ulps
    // off the plane it's supposedly cut along (owner 2026-11-13: caught
    // by an exact `orient3d` check against the plane itself, on a cut
    // near a sphere's pole). Snapped here onto the plane exactly, the
    // same way a new cut point already is in `edge_cut_point`: moving
    // an on-plane vertex changes nothing about which faces it's in or
    // which side they're on (the decision that put it here), only its
    // position by at most a few ulps.
    for &v in &used_on_plane {
        if island_vertices.contains(&v) {
            continue;
        }
        let p = mesh.corner_position(v);
        let snapped = match axis {
            Some(i) => std::array::from_fn(|k| if k == i { frame.origin[i] } else { p[k] }),
            None => {
                let d: f64 = (0..3)
                    .map(|k| (p[k] - frame.origin[k]) * frame.normal[k])
                    .sum();
                std::array::from_fn(|k| p[k] - d * frame.normal[k])
            }
        };
        if snapped != p {
            edit.move_vertex(VertexId(v), snapped)?;
        }
    }
    let input = slots as u32;
    let pos = |v: u32| -> [f64; 3] {
        if v < input {
            mesh.corner_position(v)
        } else {
            new_positions[(v - input) as usize]
        }
    };
    // Each kept quad, split by the diagonal that keeps both triangles
    // facing the way the input face did (exact `orient2d` in the face's
    // own dominant projection): rounding can put a cut point exactly in
    // line with — or just past — its neighbour (owner 2026-11-13), and
    // then only one diagonal is valid. Either is valid topologically.
    for &([x1, m1, m2, x2], tri, f) in &quads {
        let facing = face_facing(tri.map(&pos));
        let ok = |pair: &[[u32; 3]; 2]| pair.iter().all(|t| facing(t.map(&pos)) > 0);
        let first = [[x1, m1, m2], [x1, m2, x2]];
        let second = [[x1, m1, x2], [m1, m2, x2]];
        let pair = if !ok(&first) && ok(&second) {
            second
        } else {
            first
        };
        pieces.extend(pair);
        piece_faces.extend([f, f]);
    }

    // 3b. Zero-length section edges. Two cut points next to each other
    // on the section that came out at exactly the same f64 point (the
    // section passes a vertex closer than an ulp, as on the owner's scan
    // whose equator ring sits 1.2e-15 above the plane: two crossings
    // either side of a ring vertex round to one point) are one point:
    // the edge between them is collapsed, the piece along it (which has
    // no area) goes. Only where the collapse keeps the part manifold —
    // the two points' only common neighbour is that piece's third corner
    // (the link condition) — otherwise both stay, a valid part with a
    // zero-area face. Exact equality only; topology decides the rest.
    let mut remap: HashMap<u32, u32> = HashMap::new();
    {
        let mut pairs: Vec<(u32, u32)> = plane_edges
            .iter()
            .filter(|&&(u, w, _)| {
                u >= input && w >= input && pos(u).map(f64::to_bits) == pos(w).map(f64::to_bits)
            })
            .map(|&(u, w, _)| (u.min(w), u.max(w)))
            .collect();
        pairs.sort_unstable();
        pairs.dedup();
        if !pairs.is_empty() {
            let mut around: HashMap<u32, Vec<usize>> = HashMap::new();
            for (k, t) in pieces.iter().enumerate() {
                for &v in t {
                    if v >= input {
                        around.entry(v).or_default().push(k);
                    }
                }
            }
            let mut dead = vec![false; pieces.len()];
            let root = |remap: &HashMap<u32, u32>, mut v: u32| {
                while let Some(&r) = remap.get(&v) {
                    v = r;
                }
                v
            };
            for (u, w) in pairs {
                let (ru, rw) = (root(&remap, u), root(&remap, w));
                if ru == rw {
                    continue;
                }
                let live_around = |v: u32| -> Vec<usize> {
                    around[&v].iter().copied().filter(|&k| !dead[k]).collect()
                };
                let (tu, tw) = (live_around(ru), live_around(rw));
                let shared: Vec<usize> = tu.iter().copied().filter(|k| tw.contains(k)).collect();
                let [along] = shared[..] else {
                    continue;
                };
                let neighbours = |ts: &[usize], v: u32| -> HashSet<u32> {
                    ts.iter()
                        .flat_map(|&k| pieces[k])
                        .filter(|&x| x != v)
                        .collect()
                };
                let (nu, nw) = (neighbours(&tu, ru), neighbours(&tw, rw));
                let opposite: HashSet<u32> = pieces[along]
                    .into_iter()
                    .filter(|&x| x != ru && x != rw)
                    .collect();
                let common: HashSet<u32> = nu
                    .intersection(&nw)
                    .copied()
                    .filter(|&x| x != ru && x != rw)
                    .collect();
                if common != opposite {
                    continue;
                }
                let (keep_id, gone) = (ru.min(rw), ru.max(rw));
                dead[along] = true;
                for &k in &tw.iter().chain(&tu).copied().collect::<Vec<_>>() {
                    for v in &mut pieces[k] {
                        if *v == gone {
                            *v = keep_id;
                        }
                    }
                }
                // `gone` is a cut point, so it is in a piece: it has an entry.
                if let Some(moved) = around.remove(&gone) {
                    around.entry(keep_id).or_default().extend(moved);
                }
                remap.insert(gone, keep_id);
            }
            let mut kept = Vec::with_capacity(pieces.len());
            let mut kept_faces = Vec::with_capacity(pieces.len());
            for ((t, f), d) in pieces.into_iter().zip(piece_faces).zip(dead) {
                if !d {
                    kept.push(t);
                    kept_faces.push(f);
                }
            }
            pieces = kept;
            piece_faces = kept_faces;
            let flat: Vec<(u32, u32)> = remap.keys().map(|&v| (v, root(&remap, v))).collect();
            remap = flat.into_iter().collect();
        }
    }
    let canon = |v: u32| -> u32 { remap.get(&v).copied().unwrap_or(v) };
    for edge in &mut plane_edges {
        edge.0 = canon(edge.0);
        edge.1 = canon(edge.1);
    }
    plane_edges.retain(|e| e.0 != e.1);
    let mut merged_away: Vec<u32> = remap.keys().copied().collect();
    merged_away.sort_unstable();
    for &v in &merged_away {
        edit.remove_vertex(VertexId(v))?;
    }
    ctx.checkpoint()?;
    let proj = Projection::new(frame.normal);

    // 4. The boundary left in the plane, as loops, traced on the cut's own
    // topology (still traced with `cap: false`: a vase's open rim is still
    // exactly one well-defined loop per opening, just left uncapped).
    let span = ctx.span(CAPPING, 0.0, 1.0, 1);
    let loops = boundary_loops(&plane_edges, mesh, &side, &pos, ctx)?;
    let capped = if !cap || loops.is_empty() {
        None
    } else {
        // Counter-clockwise outer loops seen from above: the part below's
        // boundary runs the other way round its cap.
        let oriented: Vec<Vec<(u32, FaceId)>> = loops
            .into_iter()
            .map(|mut l| {
                if keep == Side::Below {
                    l.reverse();
                }
                l
            })
            .collect();
        ctx.checkpoint()?;
        let mut genuine = || crossing_faces_intersect(mesh, plane, ctx);
        let tris = cap_triangles(&oriented, &pos, proj, &mut genuine)?;
        Some((oriented, tris))
    };
    for (tri, f) in pieces.iter().zip(&piece_faces) {
        tick.tick()?;
        edit.add_face(
            *tri,
            Sources {
                faces: vec![*f],
                ..Sources::default()
            },
        )?;
    }
    let mut cap_faces = Vec::new();
    let mut section_regions = Vec::new();
    let mut region_crossing_faces: Vec<HashSet<FaceId>> = Vec::new();
    if let Some((oriented, tris)) = capped {
        let loops2d: Vec<Vec<[f64; 2]>> = oriented
            .iter()
            .map(|l| l.iter().map(|&(v, _)| frame.to_2d(pos(v))).collect())
            .collect();
        ctx.checkpoint()?;
        let mut cap: Vec<([u32; 3], Vec<FaceId>)> = tris
            .into_iter()
            .map(|(mut tri, sources)| {
                if keep == Side::Above {
                    tri.swap(1, 2);
                }
                (tri, sources)
            })
            .collect();
        // The side edges joining two section points other than along the
        // section itself: a cap triangle along one of these would give
        // that edge four faces. They exist only where an input vertex is
        // on the section (on the plane): pieces' edges, and kept whole
        // faces with two such corners — where the kept side lies along
        // the plane within rounding, an ε-thin slab whose two faces the
        // cap's triangulation would otherwise naturally retrace (owner
        // 2026-11-13: a box face horizontal in world space but ulp-noisy
        // in a tilted object's own frame, cut along it).
        let on_section: HashSet<u32> = oriented.iter().flatten().map(|&(v, _)| v).collect();
        let originals: HashSet<u32> = on_section.iter().copied().filter(|&v| v < input).collect();
        let mut side_edges: HashSet<(u32, u32)> = HashSet::new();
        let whole: Vec<[u32; 3]> = if originals.len() >= 2 {
            (0..mesh.face_chunk_count())
                .into_par_iter()
                .flat_map_iter(|k| {
                    mesh.faces_in_chunk(k)
                        .filter(|&(f, t)| {
                            t.iter().filter(|v| originals.contains(v)).count() >= 2
                                && edit.is_face_live(f)
                        })
                        .map(|(_, t)| t)
                        .collect::<Vec<_>>()
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut note = |t: [u32; 3]| {
            for i in 0..3 {
                let (u, w) = (t[i], t[(i + 1) % 3]);
                if on_section.contains(&u) && on_section.contains(&w) {
                    side_edges.insert((u.min(w), u.max(w)));
                }
            }
        };
        for &t in &pieces {
            note(t);
        }
        for &t in &whole {
            note(t);
        }
        // What no flip clears (both of a cap quad's diagonals are side
        // edges: the ε-thin slab folded) is split at its midpoint, in the
        // cap: the four new edges all meet a new vertex, so none can be a
        // side edge.
        for (u, w) in flip_off_side_edges(&mut cap, &side_edges) {
            let ts: Vec<usize> = (0..cap.len())
                .filter(|&k| cap[k].0.contains(&u) && cap[k].0.contains(&w))
                .collect();
            let [t1, t2] = ts[..] else {
                return Err(CutError::SectionDegenerate { at: pos(u) });
            };
            let (pu, pw) = (pos(u), pos(w));
            let m = edit
                .add_vertex(std::array::from_fn(|i| pu[i] + (pw[i] - pu[i]) * 0.5))
                .0;
            let mut more = Vec::with_capacity(2);
            for k in [t1, t2] {
                let (t, sources) = cap[k].clone();
                let i = (0..3)
                    .find(|&i| {
                        let (p, q) = (t[i], t[(i + 1) % 3]);
                        (p == u && q == w) || (p == w && q == u)
                    })
                    .ok_or(CutError::SectionDegenerate { at: pu })?;
                let (p, q, r) = (t[i], t[(i + 1) % 3], t[(i + 2) % 3]);
                cap[k] = ([p, m, r], sources.clone());
                more.push(([m, q, r], sources));
            }
            cap.extend(more);
        }
        // The invariant the construction guarantees, checked: every edge
        // the cut made (one end a new vertex) or that joins two section
        // points has exactly two faces, wound opposite ways — on the
        // pieces, the cap and the whole faces between section points. A
        // failure here is a fault in this code, named, never written as a
        // part that isn't closed and manifold.
        let mut edges: HashMap<(u32, u32), (i32, i32)> = HashMap::new();
        let made = |a: u32, b: u32| {
            a >= input || b >= input || (on_section.contains(&a) && on_section.contains(&b))
        };
        for t in pieces
            .iter()
            .chain(cap.iter().map(|(t, _)| t))
            .chain(&whole)
        {
            for i in 0..3 {
                let (a, b) = (t[i], t[(i + 1) % 3]);
                if made(a, b) {
                    let e = edges.entry((a.min(b), a.max(b))).or_insert((0, 0));
                    e.0 += 1;
                    e.1 += if a < b { 1 } else { -1 };
                }
            }
        }
        let mut bad: Vec<u32> = edges
            .iter()
            .filter(|&(_, &(n, net))| n != 2 || net != 0)
            .map(|(&(a, _), _)| a)
            .collect();
        bad.sort_unstable();
        if let Some(&v) = bad.first() {
            return Err(CutError::SectionDegenerate { at: pos(v) });
        }
        for (tri, sources) in cap {
            cap_faces.push(edit.add_face(
                tri,
                Sources {
                    faces: sources,
                    ..Sources::default()
                },
            )?);
        }
        let faces = loop_faces(mesh, &oriented, input, ctx)?;
        section_regions = regions(&loops2d, &faces, frame);
        // Each region's own crossing faces, same order as
        // `section_regions`: the outer loops (positive signed area, the
        // same filter `regions()` applies) and `section_regions` share
        // that relative order.
        let outer_loop_indices: Vec<usize> = (0..loops2d.len())
            .filter(|&i| signed_area(&loops2d[i]) > 0.0)
            .collect();
        region_crossing_faces = outer_loop_indices
            .iter()
            .map(|&i| faces[i].iter().copied().collect())
            .collect();
    }
    span.finish();
    let (mesh, provenance) = edit.finish()?;
    Ok(HalfCut {
        mesh,
        provenance,
        cap_faces,
        section_regions,
        region_crossing_faces,
    })
}

/// Flips cap triangles' shared edges off `side_edges` (undirected,
/// `(low, high)`): an edge both a cap's interior edge and a side edge
/// would have four faces. Each such edge, shared by cap triangles `[u,
/// w, a]` and `[w, u, b]`, is replaced by `a–b` (the quad's other
/// diagonal), when that is neither a side edge nor already in the cap.
/// Purely combinatorial: orientation is kept, the cap's boundary is
/// untouched. Deterministic (lowest edge first). Returns the conflicts
/// no flip could clear, lowest first.
fn flip_off_side_edges(
    cap: &mut [([u32; 3], Vec<FaceId>)],
    side_edges: &HashSet<(u32, u32)>,
) -> Vec<(u32, u32)> {
    let key = |u: u32, w: u32| (u.min(w), u.max(w));
    let mut by_edge: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (k, (t, _)) in cap.iter().enumerate() {
        for i in 0..3 {
            by_edge
                .entry(key(t[i], t[(i + 1) % 3]))
                .or_default()
                .push(k);
        }
    }
    let mut conflicts: Vec<(u32, u32)> = by_edge
        .iter()
        .filter(|(e, ts)| ts.len() == 2 && side_edges.contains(e))
        .map(|(&e, _)| e)
        .collect();
    if conflicts.is_empty() {
        return Vec::new();
    }
    conflicts.sort_unstable();
    conflicts.reverse();
    let mut budget = 4 * cap.len() + 16;
    let mut stuck: Vec<(u32, u32)> = Vec::new();
    while let Some(e) = conflicts.pop() {
        if budget == 0 {
            stuck.push(e);
            stuck.append(&mut conflicts);
            break;
        }
        budget -= 1;
        let Some(ts) = by_edge.get(&e).filter(|ts| ts.len() == 2).cloned() else {
            continue;
        };
        // `t1` runs u → w, `t2` w → u.
        let directed =
            |t: [u32; 3], from: u32, to: u32| (0..3).any(|i| t[i] == from && t[(i + 1) % 3] == to);
        let (mut t1, mut t2) = (ts[0], ts[1]);
        let (u, w) = if directed(cap[t1].0, e.0, e.1) {
            (e.0, e.1)
        } else {
            (e.1, e.0)
        };
        if !directed(cap[t1].0, u, w) {
            std::mem::swap(&mut t1, &mut t2);
        }
        let third = |t: [u32; 3]| t.into_iter().find(|&v| v != u && v != w);
        let (Some(a), Some(b)) = (third(cap[t1].0), third(cap[t2].0)) else {
            stuck.push(e);
            continue;
        };
        let ab = key(a, b);
        if a == b || side_edges.contains(&ab) || by_edge.contains_key(&ab) {
            stuck.push(e);
            continue;
        }
        let mut sources: Vec<FaceId> = cap[t1].1.iter().chain(&cap[t2].1).copied().collect();
        sources.sort_unstable();
        sources.dedup();
        cap[t1] = ([u, b, a], sources.clone());
        cap[t2] = ([b, w, a], sources);
        by_edge.remove(&e);
        by_edge.insert(ab, vec![t1, t2]);
        for (edge, from, to) in [(key(u, b), t2, t1), (key(w, a), t1, t2)] {
            if let Some(ts) = by_edge.get_mut(&edge) {
                for t in ts.iter_mut() {
                    if *t == from {
                        *t = to;
                    }
                }
            }
        }
        // A flip can unblock one that was stuck.
        conflicts.append(&mut stuck);
    }
    // Still conflicts: a flip since may have cleared some.
    stuck.retain(|e| by_edge.get(e).is_some_and(|ts| ts.len() == 2));
    stuck.sort_unstable();
    stuck.dedup();
    stuck
}

/// Where the plane crosses the edge `lo`→`hi` (vertex ids ascending, so
/// both faces on the edge get the same point), given each end's
/// distance from the plane (`dx`, `dy`, same scale, both nonzero: the
/// ends are on opposite sides). Interpolated from the end **nearer the
/// plane**, by `s = d_near / (d_near + d_far) <= 1/2` (no cancellation:
/// a sum of magnitudes): a crossing an ulp from a vertex is that vertex
/// plus an offset smaller than an ulp, which rounds onto the vertex
/// itself, consistently for every edge round it (owner 2026-11-13: the
/// cut points of one near-plane vertex's edges, interpolated from
/// whichever end had the lower id with distances from rounded dot
/// products, came out in the wrong order along the section, a spike
/// doubling back on itself). On an axis-aligned plane the axis
/// coordinate is the plane's, exactly.
fn edge_cut_point(
    frame: &PlaneFrame,
    axis: Option<usize>,
    x: [f64; 3],
    y: [f64; 3],
    dx: f64,
    dy: f64,
) -> [f64; 3] {
    let (near, far, dn, df) = if dx <= dy {
        (x, y, dx, dy)
    } else {
        (y, x, dy, dx)
    };
    let s = if dn + df > 0.0 { dn / (dn + df) } else { 0.5 };
    let mut point: [f64; 3] = std::array::from_fn(|i| near[i] + s * (far[i] - near[i]));
    if let Some(i) = axis {
        point[i] = frame.origin[i];
    }
    point
}

/// Whether both sides' pieces of crossing face `tri` (sides `s`, with
/// a corner on each real side), built with the cut points `cut_at`, face
/// the way the face does: exact `orient2d` in the face's own projection
/// ([`face_facing`]). A corner on the plane gives two triangles; a lone
/// corner a triangle and a quad, which faces right if either diagonal
/// splits it into two triangles that do.
fn pieces_face_their_face(
    mesh: &Mesh,
    tri: [u32; 3],
    s: &[i8; 3],
    cut_at: &HashMap<(u32, u32), [f64; 3]>,
) -> bool {
    let at = |p: u32, q: u32| cut_at[&(p.min(q), p.max(q))];
    let corner = |v: u32| mesh.corner_position(v);
    let facing = face_facing(tri.map(corner));
    let up = |t: [[f64; 3]; 3]| facing(t) > 0;
    if let Some(z) = (0..3).find(|&i| s[i] == 0) {
        let (zv, p, q) = (tri[z], tri[(z + 1) % 3], tri[(z + 2) % 3]);
        let x = at(p, q);
        return up([corner(zv), corner(p), x]) && up([corner(zv), x, corner(q)]);
    }
    let lone = (0..3)
        .find(|&i| s[i] != s[(i + 1) % 3] && s[i] != s[(i + 2) % 3])
        .expect("a crossing face with no corner on the plane has a lone corner");
    let (l, m1, m2) = (tri[lone], tri[(lone + 1) % 3], tri[(lone + 2) % 3]);
    let (x1, x2) = (at(l, m1), at(l, m2));
    let (pm1, pm2) = (corner(m1), corner(m2));
    up([corner(l), x1, x2])
        && ((up([x1, pm1, pm2]) && up([x1, pm2, x2])) || (up([x1, pm1, x2]) && up([pm1, pm2, x2])))
}

/// How a triangle faces relative to `reference` (an input face's
/// corners): +1 the same way, -1 flipped, 0 collinear, by exact
/// `orient2d` in the projection that drops the reference's dominant
/// normal axis.
fn face_facing(reference: [[f64; 3]; 3]) -> impl Fn([[f64; 3]; 3]) -> i8 {
    let [a, b, c] = reference;
    let n = cross(sub(b, a), sub(c, a));
    let mut k = 0;
    for i in 1..3 {
        if n[i].abs() > n[k].abs() {
            k = i;
        }
    }
    let (i, j) = ((k + 1) % 3, (k + 2) % 3);
    let orient = move |t: [[f64; 3]; 3]| -> f64 {
        let q = t.map(|p| Coord { x: p[i], y: p[j] });
        orient2d(q[0], q[1], q[2])
    };
    let sign = orient(reference).signum();
    move |t| {
        let o = orient(t) * sign;
        if o > 0.0 {
            1
        } else if o < 0.0 {
            -1
        } else {
            0
        }
    }
}

/// Every face reachable from `seeds` by a shared vertex (Phase 1b: an
/// excluded region's whole connected island — not just its crossing
/// faces — so the region's entire appendage, both the part above the
/// plane and the part below it, is kept together rather than split by
/// vertex side). A flood fill over a vertex→faces adjacency built once
/// for `mesh`.
fn expand_island(mesh: &Mesh, seeds: &HashSet<FaceId>) -> HashSet<FaceId> {
    let mut by_vertex: HashMap<u32, Vec<FaceId>> = HashMap::new();
    for (f, tri) in mesh.faces() {
        for v in tri {
            by_vertex.entry(v).or_default().push(f);
        }
    }
    let mut island: HashSet<FaceId> = HashSet::new();
    let mut queue: Vec<FaceId> = seeds.iter().copied().collect();
    while let Some(f) = queue.pop() {
        if !island.insert(f) {
            continue;
        }
        let Some(tri) = mesh.face(f) else { continue };
        for v in tri {
            for &neighbour in by_vertex.get(&v).into_iter().flatten() {
                if !island.contains(&neighbour) {
                    queue.push(neighbour);
                }
            }
        }
    }
    island
}

/// The directed plane edges that don't cancel against a reverse edge,
/// traced into loops of `(vertex, source face)`, on the cut's own
/// topology. Every cut point is on one crossing edge, whose two faces
/// give it exactly one way in and one way out. Only an input vertex on
/// the plane can be passed more than once (loops touching there, or one
/// pinched); each way in is then paired with the way out that the input
/// mesh's own fan of faces round the vertex puts next, turning the way
/// the faces wind: a kept part's boundary leaves a vertex along the first
/// face of each wedge of kept faces round it and arrives along the last,
/// so the next way out after a way in is across the gap the cap fills.
/// No coordinates: the pairing is the mesh's own (a fixed rule, the same
/// whatever rounding did to the points).
fn boundary_loops(
    edges: &[(u32, u32, FaceId)],
    mesh: &Mesh,
    side: &[i8],
    pos: &dyn Fn(u32) -> [f64; 3],
    ctx: &RunContext,
) -> Result<Vec<Vec<(u32, FaceId)>>, CutError> {
    let bad = |v: u32| CutError::SectionDegenerate { at: pos(v) };
    // Each directed edge, netted against its reverse (an on-plane edge
    // between two kept faces, or a kept face and an in-plane face,
    // appears once each way and isn't boundary).
    let mut count: HashMap<(u32, u32), Vec<FaceId>> = HashMap::with_capacity(edges.len());
    for &(u, v, f) in edges {
        count.entry((u, v)).or_default().push(f);
    }
    let mut boundary: Vec<(u32, u32, FaceId)> = Vec::with_capacity(count.len());
    for (&(u, v), faces) in &count {
        let back = count.get(&(v, u)).map_or(0, Vec::len);
        let mut faces = faces.clone();
        faces.sort_unstable();
        boundary.extend(faces.into_iter().skip(back).map(|f| (u, v, f)));
    }
    boundary.sort_unstable_by_key(|&(u, v, f)| (u, v, f));
    let mut out: HashMap<u32, Vec<usize>> = HashMap::with_capacity(boundary.len());
    for (k, &(u, _, _)) in boundary.iter().enumerate() {
        out.entry(u).or_default().push(k);
    }
    let mut multi: Vec<u32> = out
        .iter()
        .filter(|(_, ks)| ks.len() > 1)
        .map(|(&v, _)| v)
        .collect();
    multi.sort_unstable();
    // For each such vertex, its input faces in winding order round it:
    // face `(v, a, b)` is followed by the face `(v, b, c)`. `(vertex,
    // face) -> (fan, index in it, fan length)`; a vertex where two
    // sheets meet has one fan per sheet.
    let mut fan: HashMap<(u32, FaceId), (usize, usize, usize)> = HashMap::new();
    if !multi.is_empty() {
        if let Some(&v) = multi
            .iter()
            .find(|&&v| v as usize >= side.len() || side[v as usize] != 0)
        {
            // A cut point has exactly one way in and out.
            return Err(bad(v));
        }
        let wanted: HashSet<u32> = multi.iter().copied().collect();
        let faces: Vec<(FaceId, [u32; 3])> = (0..mesh.face_chunk_count())
            .into_par_iter()
            .flat_map_iter(|k| {
                mesh.faces_in_chunk(k)
                    .filter(|(_, t)| {
                        t.iter()
                            .any(|&v| side[v as usize] == 0 && wanted.contains(&v))
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        ctx.checkpoint()?;
        let mut by_vertex: HashMap<u32, Vec<(FaceId, u32, u32)>> = HashMap::new();
        for (f, t) in faces {
            for i in 0..3 {
                if wanted.contains(&t[i]) {
                    by_vertex
                        .entry(t[i])
                        .or_default()
                        .push((f, t[(i + 1) % 3], t[(i + 2) % 3]));
                }
            }
        }
        for (v, mut around) in by_vertex {
            around.sort_unstable();
            let starting: HashMap<u32, usize> = around
                .iter()
                .enumerate()
                .map(|(i, &(_, a, _))| (a, i))
                .collect();
            let mut seen = vec![false; around.len()];
            let mut sheet = 0;
            for start in 0..around.len() {
                if seen[start] {
                    continue;
                }
                let mut ring = Vec::new();
                let mut i = start;
                loop {
                    seen[i] = true;
                    ring.push(i);
                    match starting.get(&around[i].2) {
                        Some(&j) if j == start => break,
                        Some(&j) if !seen[j] => i = j,
                        _ => return Err(bad(v)),
                    }
                }
                for (index, &i) in ring.iter().enumerate() {
                    fan.insert((v, around[i].0), (sheet, index, ring.len()));
                }
                sheet += 1;
            }
        }
    }
    // Where boundary edge `k` meets `v` in `v`'s fan: `(sheet, place,
    // places)`, three places per face — its edge to its next corner (0),
    // inside it (1), its edge from its previous corner (2).
    let place = |k: usize, v: u32| -> Option<(usize, usize, usize)> {
        let (u, w, f) = boundary[k];
        let &(sheet, index, len) = fan.get(&(v, f))?;
        let t = mesh.face(f)?;
        let r = t.iter().position(|&x| x == v)?;
        let (next, prev) = (t[(r + 1) % 3], t[(r + 2) % 3]);
        let sub = if u == v {
            if w == next { 0 } else { 1 }
        } else if u == prev {
            2
        } else {
            1
        };
        Some((sheet, 3 * index + sub, 3 * len))
    };
    let pick = |k_in: usize, v: u32, ks: &[usize]| -> Result<usize, CutError> {
        let (sheet, at, places) = place(k_in, v).ok_or_else(|| bad(v))?;
        let mut best: Option<(usize, usize)> = None;
        for &k in ks {
            let (s, p, _) = place(k, v).ok_or_else(|| bad(v))?;
            if s != sheet {
                continue;
            }
            let d = (p + places - at) % places;
            if d != 0 && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, k));
            }
        }
        best.map(|(_, k)| k).ok_or_else(|| bad(v))
    };
    let mut used = vec![false; boundary.len()];
    let mut loops = Vec::new();
    for first in 0..boundary.len() {
        if used[first] {
            continue;
        }
        let mut l = Vec::new();
        let mut k = first;
        loop {
            if used[k] {
                return Err(bad(boundary[k].0));
            }
            used[k] = true;
            let (u, v, f) = boundary[k];
            l.push((u, f));
            let ks = out.get(&v).ok_or_else(|| bad(v))?;
            let next = if ks.len() == 1 {
                ks[0]
            } else {
                pick(k, v, ks)?
            };
            if next == first {
                break;
            }
            k = next;
        }
        if l.len() < 3 {
            return Err(bad(l[0].0));
        }
        loops.push(l);
    }
    Ok(loops)
}

/// The plane's exact 2D projection: drop the normal's dominant
/// coordinate (lowest axis on a tie), keeping the other two in the order
/// that makes counter-clockwise seen from the normal's side
/// counter-clockwise here too. Exact (no arithmetic at all), so distinct
/// points stay distinct and every `orient2d` on them is the true one.
#[derive(Debug, Clone, Copy)]
struct Projection {
    i: usize,
    j: usize,
}

impl Projection {
    fn new(normal: [f64; 3]) -> Projection {
        let mut k = 0;
        for a in 1..3 {
            if normal[a].abs() > normal[k].abs() {
                k = a;
            }
        }
        let (i, j) = ((k + 1) % 3, (k + 2) % 3);
        if normal[k] < 0.0 {
            Projection { i: j, j: i }
        } else {
            Projection { i, j }
        }
    }

    fn p2(&self, p: [f64; 3]) -> [f64; 2] {
        [p[self.i], p[self.j]]
    }
}

/// A cap triangle: its corners and the input faces it closes.
type CapTriangle = ([u32; 3], Vec<FaceId>);

/// The cap's triangles (D3) over the oriented `loops`, as `(corners,
/// source faces)`, wound like the loops (each loop edge used in its own
/// direction).
///
/// The section triangulation shared with the boolean window
/// ([`crate::section::triangulate_section_touching`], a constrained
/// Delaunay triangulation with exact predicates, loops touching at a
/// point allowed) on the plane's exact projection. That triangulation
/// sees the cut points' rounded coordinates, and rounding can fold a
/// valid section where it passes within an ulp of a vertex: two points
/// at one place, a spike, a kink back across itself. The section's own
/// topology is never in doubt (the loops come from the mesh), so a fold
/// is not a reason to refuse: if the input faces the plane passes
/// through don't genuinely cross one another (`genuine`, exact; checked
/// once, at the first fold — if they do, the cut is refused as
/// [`CutError::SectionSelfIntersects`], Make solid next), the triangle at
/// the point the triangulation reported is cut off its loop as an ear
/// (combinatorially always valid: two loop edges and a new diagonal that
/// isn't an edge yet), and the rest is triangulated again. Folds are a
/// few points round one vertex, so this ends after a few ears.
fn cap_triangles(
    loops: &[Vec<(u32, FaceId)>],
    pos: &dyn Fn(u32) -> [f64; 3],
    proj: Projection,
    genuine: &mut dyn FnMut() -> Result<bool, CutError>,
) -> Result<Vec<CapTriangle>, CutError> {
    let sources = |pts: &[(u32, FaceId)]| -> Vec<FaceId> {
        let mut s: Vec<FaceId> = pts.iter().map(|p| p.1).collect();
        s.sort_unstable();
        s.dedup();
        s
    };
    let key = |u: u32, w: u32| (u.min(w), u.max(w));
    let mut work: Vec<Vec<(u32, FaceId)>> = loops.to_vec();
    let mut out: Vec<CapTriangle> = Vec::new();
    // Edges of the ears cut off so far.
    let mut ear_edges: HashSet<(u32, u32)> = HashSet::new();
    let mut checked = false;
    loop {
        work.retain(|l| !l.is_empty());
        if work.is_empty() {
            return Ok(out);
        }
        let flat: Vec<(u32, FaceId)> = work.iter().flatten().copied().collect();
        let exact: Vec<Vec<[f64; 2]>> = work
            .iter()
            .map(|l| l.iter().map(|&(v, _)| proj.p2(pos(v))).collect())
            .collect();
        let ids: Vec<u32> = flat.iter().map(|p| p.0).collect();
        let fault = match crate::section::triangulate_section_touching(&exact, &ids) {
            Ok(tris) => {
                for t in tris {
                    let pts = t.map(|k| flat[k as usize]);
                    out.push((pts.map(|p| p.0), sources(&pts)));
                }
                return Ok(out);
            }
            Err((_, k)) => k.min(flat.len() - 1),
        };
        if !checked {
            if genuine()? {
                return Err(CutError::SectionSelfIntersects {
                    at: pos(flat[fault].0),
                });
            }
            checked = true;
        }
        let (mut li, mut i) = (0, fault);
        while i >= work[li].len() {
            i -= work[li].len();
            li += 1;
        }
        let loop_edges: HashSet<(u32, u32)> = work
            .iter()
            .flat_map(|l| (0..l.len()).map(move |j| key(l[j].0, l[(j + 1) % l.len()].0)))
            .collect();
        let l = &mut work[li];
        let n = l.len();
        if n == 3 {
            let pts = [l[0], l[1], l[2]];
            out.push((pts.map(|p| p.0), sources(&pts)));
            for j in 0..3 {
                ear_edges.insert(key(pts[j].0, pts[(j + 1) % 3].0));
            }
            l.clear();
            continue;
        }
        let ear = |j: usize| -> [(u32, FaceId); 3] { [l[(j + n - 1) % n], l[j], l[(j + 1) % n]] };
        let clippable = |j: usize| {
            let [p, c, q] = ear(j).map(|x| x.0);
            p != c
                && c != q
                && p != q
                && !loop_edges.contains(&key(p, q))
                && !ear_edges.contains(&key(p, q))
        };
        // The reported point, else the nearest one along its loop that
        // can be cut off.
        let Some(j) = (0..n)
            .flat_map(|d| [(i + d) % n, (i + n - d % n) % n])
            .find(|&j| clippable(j))
        else {
            return Err(CutError::SectionDegenerate { at: pos(l[i].0) });
        };
        let pts = ear(j);
        out.push((pts.map(|p| p.0), sources(&pts)));
        for k in 0..3 {
            ear_edges.insert(key(pts[k].0, pts[(k + 1) % 3].0));
        }
        l.remove(j);
    }
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

/// Each loop's input faces, for its region's identity
/// ([`SectionRegion::seed_face`]) and membership
/// ([`HalfCut::region_crossing_faces`]): the face each loop edge came
/// from and, for an edge between two input vertices (on the plane, so
/// along an input edge), also the face across that edge. A crossing face
/// gives the same edge to both sides, but an on-plane edge comes from a
/// face on the kept side only; with the face across it too, a region is
/// named by the same faces whichever side reads it (split-harden: a
/// sphere's equator on the plane, its Above loop from the upper band's
/// faces and its Below loop from the lower band's).
fn loop_faces(
    mesh: &Mesh,
    loops: &[Vec<(u32, FaceId)>],
    input: u32,
    ctx: &RunContext,
) -> Result<Vec<Vec<FaceId>>, CutError> {
    // The input edges along the section, both ways round.
    let mut along: HashSet<(u32, u32)> = HashSet::new();
    for l in loops {
        for (k, &(u, _)) in l.iter().enumerate() {
            let w = l[(k + 1) % l.len()].0;
            if u < input && w < input {
                along.insert((w, u));
                along.insert((u, w));
            }
        }
    }
    let mut across: HashMap<(u32, u32), FaceId> = HashMap::new();
    if !along.is_empty() {
        let found: Vec<((u32, u32), FaceId)> = (0..mesh.face_chunk_count())
            .into_par_iter()
            .flat_map_iter(|k| {
                let along = &along;
                mesh.faces_in_chunk(k).flat_map(move |(f, t)| {
                    (0..3)
                        .map(move |i| (t[i], t[(i + 1) % 3]))
                        .filter(|e| along.contains(e))
                        .map(move |e| (e, f))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        ctx.checkpoint()?;
        across.extend(found);
    }
    Ok(loops
        .iter()
        .map(|l| {
            let mut faces: Vec<FaceId> = Vec::with_capacity(l.len());
            for (k, &(u, f)) in l.iter().enumerate() {
                faces.push(f);
                let w = l[(k + 1) % l.len()].0;
                // The face whose own edge runs the other way: the other
                // side's face along this edge (whichever way the loop was
                // turned, one of the two directions is `f`'s own).
                for e in [(w, u), (u, w)] {
                    if let Some(&g) = across.get(&e).filter(|&&g| g != f) {
                        faces.push(g);
                    }
                }
            }
            faces.sort_unstable();
            faces.dedup();
            faces
        })
        .collect())
}

/// The section's regions: each counter-clockwise loop with the clockwise
/// loops directly inside it (the smallest outer loop containing them).
/// `faces`, indexed exactly as `loops`, holds each loop's input faces
/// ([`loop_faces`]), so each region's [`SectionRegion::seed_face`] (the
/// lowest id among them) is a stable identity (Phase 1b), the same from
/// either side.
fn regions(
    loops: &[Vec<[f64; 2]>],
    faces: &[Vec<FaceId>],
    frame: PlaneFrame,
) -> Vec<SectionRegion> {
    let areas: Vec<f64> = loops.iter().map(|l| signed_area(l)).collect();
    let outers: Vec<usize> = (0..loops.len()).filter(|&i| areas[i] > 0.0).collect();
    let mut out: Vec<SectionRegion> = outers
        .iter()
        .map(|&i| SectionRegion {
            outer: loops[i].clone(),
            holes: Vec::new(),
            frame,
            seed_face: faces[i].iter().copied().min().unwrap_or(FaceId(0)), // allow-swallow: an outer loop always has at least one vertex, so this never actually falls back
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
