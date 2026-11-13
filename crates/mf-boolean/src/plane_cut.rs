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
//! - **Each crossing edge is split once**, keyed by its vertex ids, so
//!   both faces using the edge share the cut point; it is interpolated
//!   from the end nearer the plane (exact distances for an axis-aligned
//!   plane, `orient3d` otherwise), so a crossing an ulp from a vertex
//!   rounds onto that vertex. For an axis-aligned normal the cut point's
//!   axis coordinate is set to the plane's exactly, so horizontal and
//!   vertical caps are exactly flat.
//! - **Rounding never refuses a cut** (owner 2026-11-13: a whole ring of
//!   vertices an ulp above the plane). Where rounded cut points would
//!   make the section or the side pieces fold — two section points at one
//!   place, a piece with collinear corners, a spike or Z-kink in the
//!   section, two pieces back to back, a cap triangle along a side edge —
//!   the points at fault are merged where they truly are (round one input
//!   vertex, within rounding), each case decided by exact predicates
//!   only: no tolerance, and the plane is never moved.
//! - **Caps** close the boundary loops left on the plane by genuinely
//!   crossing faces. They are triangulated by a constrained Delaunay
//!   triangulation (`spade`, D3) in the plane's exact projection, with
//!   nesting by orientation (a winding number per triangle), so a hollow
//!   model's section is an annulus. Loops that touch at a point (two
//!   shells meeting there, or one loop pinched) are split there, by exact
//!   angular order, and capped each on its own. A section whose loops
//!   genuinely cross — the input faces the plane passes through intersect
//!   one another — is refused as [`CutError::SectionSelfIntersects`] (Make
//!   solid fixes the model). What's left, a plane lying along a mesh edge
//!   or face closer than its coordinates can separate where no merge
//!   closes the section cleanly, is refused as
//!   [`CutError::SectionDegenerate`] rather than written as a part that
//!   isn't closed and manifold.
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
    /// The section couldn't be closed near `at` though the model's surface
    /// doesn't cross itself there: the plane lies along the surface
    /// (through a mesh edge or face) closer than the coordinates' own
    /// rounding can separate. Refused by name rather than leaving a part
    /// that isn't closed and manifold.
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
    match cut_half_core(mesh, plane, keep, cap, exclude, ctx) {
        Err(CutError::SectionSelfIntersects { at }) => {
            Err(if crossing_faces_intersect(mesh, plane, ctx)? {
                CutError::SectionSelfIntersects { at }
            } else {
                CutError::SectionDegenerate { at }
            })
        }
        other => other,
    }
}

/// Whether the input faces the plane passes through (a corner on each
/// side, or on it) intersect one another, by the exact test
/// [`crate::validate_solid`] uses: what tells a section that genuinely
/// crosses itself (the model's surface crosses itself there; Make solid
/// fixes it) from one that only rounding couldn't close. Only run once a
/// section has failed.
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
    let island_favours_keep = if excluded_island.is_empty() {
        false
    } else {
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
    let face_chunks = mesh.face_chunk_count();
    let span = ctx.span(CUTTING, 0.0, 0.8, face_chunks);
    let scans: Vec<ChunkScan> = (0..face_chunks)
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
                        // same on-plane edge (both use the same `tri[i]
                        // -> tri[i+1]` convention as the loop below).
                        // What's left of a closed, already-capped
                        // region is nothing to retrace — only a
                        // genuinely open boundary (where no such
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
        .collect::<Result<_, _>>()?;
    span.finish();
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

    let axis = axis_of(frame.normal);
    // Cut points, by edge; their positions by `id - slots` (the edit
    // appends vertices after the input's slots, in order).
    let mut cut_points: HashMap<(u32, u32), u32> = HashMap::new();
    let mut new_positions: Vec<[f64; 3]> = Vec::new();
    // Each cut point's anchor (by `id - slots`): the edge end nearer the
    // plane, which it was interpolated from, and the fraction of the
    // edge it lies from it.
    let mut anchors: Vec<(u32, f64)> = Vec::new();
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
    let mut cut = |edit: &mut mf_geometry::MeshEdit, p: u32, q: u32| -> u32 {
        let (lo, hi) = (p.min(q), p.max(q));
        *cut_points.entry((lo, hi)).or_insert_with(|| {
            let (x, y) = (mesh.corner_position(lo), mesh.corner_position(hi));
            let (dx, dy) = (distance(x), distance(y));
            let point = edge_cut_point(&frame, axis, x, y, dx, dy);
            // The end it was interpolated from, and how far along.
            let (near, dn, df) = if dx <= dy { (lo, dx, dy) } else { (hi, dy, dx) };
            anchors.push((near, if dn + df > 0.0 { dn / (dn + df) } else { 0.5 }));
            new_positions.push(point);
            let id = edit.add_vertex(point).0;
            debug_assert_eq!(id as usize, slots + new_positions.len() - 1);
            id
        })
    };
    let mut pieces: Vec<[u32; 3]> = Vec::new();
    let mut piece_faces: Vec<FaceId> = Vec::new();
    // A crossing face whose lone corner is discarded keeps a quad, `[x1,
    // m1, m2, x2]` round the face; which diagonal splits it is chosen
    // once its cut points are final (below).
    let mut quads: Vec<([u32; 4], [u32; 3], FaceId)> = Vec::new();
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
                    quads.push(([x1, m1, m2, x2], tri, f));
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
        .filter(|v| !used_on_plane.contains(v) && !island_vertices.contains(v))
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
    // Merge loop points that the section would otherwise see as two
    // different points at one place (owner 2026-11-13: the demo scan's
    // equator ring sits 1.2e-15 above the plane, so each ring vertex
    // has two crossing edges whose cut points round to within an ulp of
    // it — sometimes to the same point, sometimes not). The section is
    // triangulated in the plane's exact projection ([`Projection`]), so
    // "at one place" means exactly that: the same projected coordinates,
    // bit for bit. Two such points are merged only when they are
    // *related* — cut points of edges sharing an input vertex, or a cut
    // point and that vertex itself — so the merge is local to one
    // vertex's fan, where the true section really does pass within
    // rounding of that vertex. Unrelated points at one place (separate
    // shells touching there) stay distinct vertices: the section touches
    // itself there, which the cap's triangulation handles as a touch
    // ([`crate::section::triangulate_section_touching`]), not a fault.
    // A cut point whose computed position *is* a kept endpoint's, bit for
    // bit, is that endpoint. Canonical id: the lowest in each merged set
    // (an input vertex wherever one is involved), independent of hash
    // order (D23's determinism). No tolerance anywhere: exact equality.
    let proj = Projection::new(frame.normal);
    let mut ends: HashMap<u32, (u32, u32)> = HashMap::with_capacity(cut_points.len());
    for (&(lo, hi), &id) in &cut_points {
        ends.insert(id, (lo, hi));
    }
    let mut ids: Vec<u32> = cut_points.values().copied().collect();
    ids.extend(used_on_plane.iter().copied());
    ids.sort_unstable();
    let mut parent: HashMap<u32, u32> = HashMap::new();
    fn find(parent: &mut HashMap<u32, u32>, v: u32) -> u32 {
        let mut r = v;
        while let Some(&p) = parent.get(&r) {
            if p == r {
                break;
            }
            r = p;
        }
        parent.insert(v, r);
        r
    }
    // Never joins two input vertices (each set's root is its lowest id,
    // so an input vertex wherever one is in it): only cut points are
    // ever merged away. Returns whether it merged.
    let input = slots as u32;
    let union = |parent: &mut HashMap<u32, u32>, a: u32, b: u32| -> bool {
        let (ra, rb) = (find(parent, a), find(parent, b));
        if ra == rb || (ra < input && rb < input) {
            return false;
        }
        parent.insert(ra.max(rb), ra.min(rb));
        true
    };
    let input_ends = |v: u32| -> [u32; 2] { ends.get(&v).map_or([v, v], |&(lo, hi)| [lo, hi]) };
    for &id in &ids {
        if let Some(&(lo, hi)) = ends.get(&id) {
            for e in [lo, hi] {
                if side[e as usize] == keep_sign
                    && pos(e).map(f64::to_bits) == pos(id).map(f64::to_bits)
                {
                    union(&mut parent, id, e);
                }
            }
        }
    }
    let mut by_place: HashMap<[u64; 2], Vec<u32>> = HashMap::new();
    for &id in &ids {
        by_place.entry(proj.key(pos(id))).or_default().push(id);
    }
    let mut places: Vec<&Vec<u32>> = by_place.values().filter(|g| g.len() > 1).collect();
    places.sort_unstable();
    for group in places {
        for (i, &a) in group.iter().enumerate() {
            let ea = input_ends(a);
            for &b in &group[i + 1..] {
                let eb = input_ends(b);
                if ea.iter().any(|v| eb.contains(v)) {
                    union(&mut parent, a, b);
                }
            }
        }
    }
    // The kept pieces under the merges so far; then, while any is
    // exactly degenerate (collinear corners, `degenerate`), merge the cut
    // point that makes it so into its anchor, and rebuild. A cut point
    // lies on its own edge, so it can only be collinear with a kept edge
    // of its face where it has rounded onto that edge's end — its anchor,
    // the end nearer the plane (owner 2026-11-13: an edge parallel to a
    // tilted plane an ulp from it, whose cut point rounded exactly onto
    // the line through the kept edge, 1 ulp from its end). Merging it
    // there is the crossing at that vertex, which is where it is to
    // within rounding. Of a degenerate piece's cut points anchored at one
    // of its own kept corners, the one nearest its anchor goes first;
    // repeated until no piece is degenerate. Exact predicates only.
    let anchor =
        |v: u32| -> Option<(u32, f64)> { (v >= input).then(|| anchors[(v - input) as usize]) };
    // Cut points by the kept end of their edge, for the closure below.
    let mut by_kept_end: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&(lo, hi), &x) in &cut_points {
        let end = if side[lo as usize] == keep_sign {
            lo
        } else {
            hi
        };
        by_kept_end.entry(end).or_default().push(x);
    }
    for cuts in by_kept_end.values_mut() {
        cuts.sort_unstable();
    }
    let (remap, pieces, piece_faces) = loop {
        // Closure round a kept vertex: each of its crossing edges gives
        // a piece with a side edge from it to that edge's cut point, two
        // faces either side. Once one of those cut points is merged into
        // it, or two of them into each other, those side edges coincide
        // (the vertex and a cut point beside it both on the section, or
        // one edge from it now bordered by four pieces): so every cut
        // point on its edges merges into it — it is where they all
        // cross, to within rounding, as the merge that started this
        // says.
        let mut merged = false;
        let mut ends: Vec<u32> = by_kept_end.keys().copied().collect();
        ends.sort_unstable();
        for v in ends {
            let cuts = &by_kept_end[&v];
            let roots: Vec<u32> = cuts.iter().map(|&x| find(&mut parent, x)).collect();
            let mut distinct = roots.clone();
            distinct.sort_unstable();
            distinct.dedup();
            if roots.contains(&v) || distinct.len() < roots.len() {
                for &x in cuts {
                    merged |= union(&mut parent, x, v);
                }
            }
        }
        if merged {
            continue;
        }
        let mut remap: HashMap<u32, u32> = HashMap::new();
        for &id in &ids {
            let r = find(&mut parent, id);
            if r != id {
                remap.insert(id, r);
            }
        }
        let canon = |v: u32| -> u32 { remap.get(&v).copied().unwrap_or(v) };
        let mut kept: Vec<[u32; 3]> = pieces.iter().map(|t| t.map(canon)).collect();
        let mut kept_faces = piece_faces.clone();
        // Each kept quad, split by the diagonal that keeps both triangles
        // facing the way the input face did (exact `orient2d` in the
        // face's own dominant projection). Rounding can put a cut point
        // exactly in line with — or just past — its neighbour (owner
        // 2026-11-13: on the demo's combined scan, a cut point an ulp
        // from the equator vertex it lies under made the quad reflex
        // there, and the fixed diagonal folded one triangle over the
        // other): only one diagonal is valid then, and it is the one
        // used. Merged corners leave a triangle.
        for &(quad, tri, f) in &quads {
            let quad = quad.map(canon);
            let mut ring: Vec<u32> = Vec::with_capacity(4);
            for v in quad {
                if ring.last() != Some(&v) && ring.first() != Some(&v) {
                    ring.push(v);
                }
            }
            match ring.len() {
                4 => {
                    let [x1, m1, m2, x2] = [ring[0], ring[1], ring[2], ring[3]];
                    let facing = face_facing(tri.map(&pos));
                    let a = [[x1, m1, m2], [x1, m2, x2]];
                    let b = [[x1, m1, x2], [m1, m2, x2]];
                    let ok = |pair: &[[u32; 3]; 2]| pair.iter().all(|t| facing(t.map(&pos)) > 0);
                    let pair = if !ok(&a) && ok(&b) { b } else { a };
                    kept.extend(pair);
                    kept_faces.extend([f, f]);
                }
                3 => {
                    kept.push([ring[0], ring[1], ring[2]]);
                    kept_faces.push(f);
                }
                _ => {}
            }
        }
        // A piece with two corners merged onto one (the crossing landed
        // essentially at a vertex it already touched) has no area:
        // dropped, not an error — the face's other piece(s), and its
        // neighbours across the now-shared point, still exactly cover
        // the kept side.
        let mut keep_piece: Vec<bool> = kept
            .iter()
            .map(|t| t[0] != t[1] && t[1] != t[2] && t[2] != t[0])
            .collect();
        // Two pieces on the same three points, wound opposite ways (two
        // faces' slivers round one near-plane vertex, merged onto the
        // same cut points): a closed pillow with no inside, whose faces
        // cancel exactly — as do the section edges they each left.
        // Dropped in pairs, they leave every edge with the two faces it
        // had besides them.
        let mut by_points: HashMap<[u32; 3], Vec<(usize, bool)>> = HashMap::new();
        for (k, t) in kept.iter().enumerate() {
            if keep_piece[k] {
                let r = if t[0] <= t[1] && t[0] <= t[2] {
                    0
                } else if t[1] <= t[2] {
                    1
                } else {
                    2
                };
                let (a, b, c) = (t[r], t[(r + 1) % 3], t[(r + 2) % 3]);
                by_points
                    .entry([a, b.min(c), b.max(c)])
                    .or_default()
                    .push((k, b < c));
            }
        }
        for group in by_points.values() {
            let fwd = group.iter().filter(|g| g.1);
            let back = group.iter().filter(|g| !g.1);
            for (f, b) in fwd.zip(back) {
                keep_piece[f.0] = false;
                keep_piece[b.0] = false;
            }
        }
        // A section point the section folds back at — `a → b → c`
        // exactly collinear in the plane's exact projection, reversing at
        // `b` — is a spike rounding made: consecutive section points come
        // from one face, so they lie round one input vertex, and their
        // true order along the section was lost where coordinates rounded
        // together (owner 2026-11-13: three cut points with one x after
        // rounding but distinct y near zero, where ulps are tiny). `b`
        // merges into whichever neighbour lies between it and the other.
        // On the section's own edges: netted against their reverses, as
        // `boundary_loops` traces them.
        let mut net: HashMap<(u32, u32), i64> = HashMap::new();
        for &(u, w, _) in &plane_edges {
            let (u, w) = (canon(u), canon(w));
            if u != w {
                *net.entry((u, w)).or_insert(0) += 1;
                *net.entry((w, u)).or_insert(0) -= 1;
            }
        }
        let mut next: HashMap<u32, (u32, i64)> = HashMap::new();
        let mut prev: HashMap<u32, (u32, i64)> = HashMap::new();
        for (&(u, w), &n) in &net {
            if n > 0 {
                next.entry(u).or_insert((w, 0)).1 += n;
                prev.entry(w).or_insert((u, 0)).1 += n;
            }
        }
        let mut tips: Vec<u32> = next.keys().copied().collect();
        tips.sort_unstable();
        for b in tips {
            let (Some(&(c, 1)), Some(&(a, 1))) = (next.get(&b), prev.get(&b)) else {
                continue;
            };
            let (pa, pb, pc) = (proj.p2(pos(a)), proj.p2(pos(b)), proj.p2(pos(c)));
            let q = |p: [f64; 2]| Coord { x: p[0], y: p[1] };
            if a == c || orient2d(q(pa), q(pb), q(pc)) != 0.0 {
                continue;
            }
            let i = if pa[0] != pb[0] { 0 } else { 1 };
            let j = if pc[0] != pb[0] { 0 } else { 1 };
            if pa[i] == pb[i] || pc[j] == pb[j] {
                continue;
            }
            // Collinear: compare along one axis the line isn't flat in.
            let k = if pa[0] != pb[0] && pc[0] != pb[0] {
                0
            } else {
                1
            };
            let (up_a, up_c) = (pa[k] > pb[k], pc[k] > pb[k]);
            if up_a != up_c {
                continue;
            }
            // Reversing at `b`: the neighbour nearer it is between.
            let to = if (pc[k] <= pa[k]) == up_a { c } else { a };
            merged |= union(&mut parent, b, to);
        }
        if merged {
            continue;
        }
        // The same fault, not quite collinear: a section edge `a → b` and
        // the one after next, `c → d`, meet — the short edge `b → c`
        // between them (two cut points round one vertex) came out
        // pointing back the way the section came, a Z-kink rounding made
        // (owner 2026-11-13: two cut points an ulp apart, the second on
        // the wrong side of the edge before them). `b` and `c` merge.
        let mut starts: Vec<u32> = next.keys().copied().collect();
        starts.sort_unstable();
        for a in starts {
            let Some(&(b, 1)) = next.get(&a) else {
                continue;
            };
            let Some(&(c, 1)) = next.get(&b) else {
                continue;
            };
            let Some(&(d, 1)) = next.get(&c) else {
                continue;
            };
            if a == c || a == d || b == d {
                continue;
            }
            let p = |v: u32| proj.p2(pos(v));
            if segments_meet(p(a), p(b), p(c), p(d)) {
                merged |= union(&mut parent, b, c);
            }
        }
        if merged {
            continue;
        }
        let mut stuck = None;
        for (k, t) in kept.iter().enumerate() {
            if !keep_piece[k] || !degenerate(t.map(&pos)) {
                continue;
            }
            let pick = t
                .iter()
                .filter_map(|&v| anchor(v).map(|(near, s)| (s, v, near)))
                .filter(|&(_, _, near)| side[near as usize] == keep_sign && t.contains(&near))
                .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            match pick {
                Some((_, v, near)) if union(&mut parent, v, near) => merged = true,
                _ => stuck = Some(t[0]),
            }
            keep_piece[k] = false;
        }
        if !merged {
            if let Some(v) = stuck {
                return Err(CutError::SectionSelfIntersects { at: pos(v) });
            }
            let mut pieces = Vec::with_capacity(kept.len());
            let mut faces = Vec::with_capacity(kept.len());
            for ((t, f), keep) in kept.into_iter().zip(kept_faces).zip(keep_piece) {
                if keep {
                    pieces.push(t);
                    faces.push(f);
                }
            }
            break (remap, pieces, faces);
        }
    };
    let canon = |v: u32| -> u32 { remap.get(&v).copied().unwrap_or(v) };
    for edge in &mut plane_edges {
        edge.0 = canon(edge.0);
        edge.1 = canon(edge.1);
    }
    // A merged pair that was one loop edge leaves a zero-length edge:
    // nothing to trace or cap there.
    plane_edges.retain(|e| e.0 != e.1);
    // A merged-away cut point is now unused (every reference to it was
    // remapped to its canonical twin): remove it so it isn't left as a
    // dangling, never-referenced vertex in the output. Only cut points
    // are ever merged away: an input vertex is always its set's lowest.
    let mut merged_away: Vec<u32> = remap.keys().copied().collect();
    merged_away.sort_unstable();
    debug_assert!(merged_away.iter().all(|&v| v as usize >= slots));
    for &v in &merged_away {
        edit.remove_vertex(VertexId(v))?;
    }
    ctx.checkpoint()?;

    // 4. The boundary left in the plane, as loops: still traced and
    // checked for a genuine fault (crossing, touching or pinching)
    // even with `cap: false` — a vase's open rim is still exactly one
    // well-defined loop per opening, just left uncapped, not however
    // the ambiguous section `SectionSelfIntersects` refuses.
    let span = ctx.span(CAPPING, 0.0, 1.0, 1);
    let capped = 'capped: {
        let loops = boundary_loops(&plane_edges, &pos, proj, keep == Side::Above)?;
        if !cap || loops.is_empty() {
            break 'capped None;
        }
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
        // Triangulated in the plane's exact projection, not the frame's
        // coordinates: `to_2d` subtracts the plane's point and rotates,
        // which rounds, and can land two distinct cut points on one 2D
        // point (owner 2026-11-13: at the default plane point x = 2.5, two
        // cut points an ulp apart at x = -7.045 both became -9.545).
        // Dropping a coordinate is exact, so the triangulation sees
        // exactly the points the mesh has.
        let exact: Vec<Vec<[f64; 2]>> = oriented
            .iter()
            .map(|l| l.iter().map(|&(v, _)| proj.p2(pos(v))).collect())
            .collect();
        ctx.checkpoint()?;
        let tris = cap_triangles(&exact)
            .map_err(|k| CutError::SectionSelfIntersects { at: pos(flat[k].0) })?;
        Some((oriented, flat, tris))
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
    if let Some((oriented, flat, tris)) = capped {
        let loops2d: Vec<Vec<[f64; 2]>> = oriented
            .iter()
            .map(|l| l.iter().map(|&(v, _)| frame.to_2d(pos(v))).collect())
            .collect();
        ctx.checkpoint()?;
        let mut cap: Vec<([u32; 3], Vec<FaceId>)> = tris
            .into_iter()
            .map(|t| {
                let mut tri = t.map(|k| flat[k].0);
                if keep == Side::Above {
                    tri.swap(1, 2);
                }
                let mut sources: Vec<FaceId> = t.iter().map(|&k| flat[k].1).collect();
                sources.sort_unstable();
                sources.dedup();
                (tri, sources)
            })
            .collect();
        // The side edges joining two section points other than along the
        // section itself: a cap triangle along one of these would give
        // that edge four faces. They exist only where an input vertex is
        // on the section (on the plane, or merged as a crossing above):
        // pieces' edges, and kept whole faces with two such corners —
        // where the kept side lies along the plane within rounding, an
        // ε-thin slab whose two faces the cap's triangulation would
        // otherwise naturally retrace (owner 2026-11-13: a box face
        // horizontal in world space but ulp-noisy in a tilted object's
        // own frame, cut along it).
        let on_section: HashSet<u32> = flat.iter().map(|&(v, _)| v).collect();
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
                return Err(CutError::SectionSelfIntersects { at: pos(u) });
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
                    .ok_or(CutError::SectionSelfIntersects { at: pu })?;
                let (p, q, r) = (t[i], t[(i + 1) % 3], t[(i + 2) % 3]);
                cap[k] = ([p, m, r], sources.clone());
                more.push(([m, q, r], sources));
            }
            cap.extend(more);
        }
        // Every edge the cut made (one end a new vertex) or that joins two
        // section points must have exactly two faces, wound opposite
        // ways: checked here, on the pieces, the cap and the whole faces
        // between section points, so a section that rounding left
        // impossible to close is refused by name, never written as a part
        // that isn't closed and manifold.
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
        section_regions = regions(&loops2d, &oriented, frame);
        // Each region's own crossing faces, same order as
        // `section_regions`: the outer loops (positive signed area, the
        // same filter `regions()` applies) and `section_regions` share
        // that relative order.
        let outer_loop_indices: Vec<usize> = (0..loops2d.len())
            .filter(|&i| signed_area(&loops2d[i]) > 0.0)
            .collect();
        region_crossing_faces = outer_loop_indices
            .iter()
            .map(|&i| oriented[i].iter().map(|&(_, f)| f).collect())
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

/// Whether segments `a–b` and `c–d` share any point (crossing, touching
/// or overlapping), by exact `orient2d` and exact coordinate comparisons.
fn segments_meet(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    let q = |p: [f64; 2]| Coord { x: p[0], y: p[1] };
    let o = |p: [f64; 2], r: [f64; 2], s: [f64; 2]| {
        let v = orient2d(q(p), q(r), q(s));
        if v > 0.0 {
            1
        } else if v < 0.0 {
            -1
        } else {
            0
        }
    };
    // `p` on segment `r–s`, given the three collinear.
    let within = |p: [f64; 2], r: [f64; 2], s: [f64; 2]| {
        (0..2).all(|i| p[i] >= r[i].min(s[i]) && p[i] <= r[i].max(s[i]))
    };
    let (o1, o2, o3, o4) = (o(a, b, c), o(a, b, d), o(c, d, a), o(c, d, b));
    if o1 * o2 < 0 && o3 * o4 < 0 {
        return true;
    }
    (o1 == 0 && within(c, a, b))
        || (o2 == 0 && within(d, a, b))
        || (o3 == 0 && within(a, c, d))
        || (o4 == 0 && within(b, c, d))
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
/// traced into loops of `(vertex, source face)`. A point the section
/// passes through more than once (loops touching, or one pinched) is
/// split there: each way in is paired with a way out by exact angular
/// order (`region_left`: the region the loops bound is on their left).
/// Only a dead end — no way out of a point, which a closed input can't
/// give — is refused.
fn boundary_loops(
    edges: &[(u32, u32, FaceId)],
    pos: &dyn Fn(u32) -> [f64; 3],
    proj: Projection,
    region_left: bool,
) -> Result<Vec<Vec<(u32, FaceId)>>, CutError> {
    let bad = |v: u32| CutError::SectionSelfIntersects { at: pos(v) };
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
    // Where a loop passes through one point twice (it touches itself
    // there, or two loops meet there), each way in is paired with the
    // way out that keeps the region on the same side without crossing:
    // seen round the point, the first one turning from the way in
    // towards the region's side (clockwise from the reversed incoming
    // edge when the region is on the left). Exact `orient2d` on the
    // exact projection, ties broken by vertex and face id (a fixed
    // symbolic perturbation): the loops come out split at the touch,
    // never refused.
    let p2 = |v: u32| {
        let q = proj.p2(pos(v));
        Coord { x: q[0], y: q[1] }
    };
    let turn = |a: u32, b: u32, c: u32| -> i8 {
        let o = orient2d(p2(a), p2(b), p2(c));
        let o = if region_left { o } else { -o };
        if o > 0.0 {
            1
        } else if o < 0.0 {
            -1
        } else {
            0
        }
    };
    // Same direction from `v`, given `a`, `b`, `v` collinear (and
    // neither at `v`): exact coordinate comparisons.
    let same_way = |v: u32, a: u32, b: u32| -> bool {
        let (pv, pa, pb) = (proj.p2(pos(v)), proj.p2(pos(a)), proj.p2(pos(b)));
        let i = if pa[0] != pv[0] { 0 } else { 1 };
        (pa[i] > pv[i]) == (pb[i] > pv[i])
    };
    // Where `w` comes, turning from `v → u` towards the region: 0 strictly
    // turning that way, 1 straight back along `v → u`'s opposite, 2 the
    // far side, 3 straight back towards `u`.
    let class = |u: u32, v: u32, w: u32| -> u8 {
        match turn(v, u, w) {
            -1 => 0,
            1 => 2,
            _ if same_way(v, u, w) => 3,
            _ => 1,
        }
    };
    let pick = |u: u32, v: u32, ks: &[usize]| -> usize {
        let mut best = ks[0];
        for &k in &ks[1..] {
            let (w, b) = (boundary[k].1, boundary[best].1);
            let (cw, cb) = (class(u, v, w), class(u, v, b));
            let better = if cw != cb {
                cw < cb
            } else {
                match if cw == 0 || cw == 2 { turn(v, b, w) } else { 0 } {
                    // `w` comes before `b` turning towards the region.
                    1 => true,
                    -1 => false,
                    _ => (w, boundary[k].2) < (b, boundary[best].2),
                }
            };
            if better {
                best = k;
            }
        }
        best
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
            let next = if ks.len() == 1 { ks[0] } else { pick(u, v, ks) };
            if next == first {
                break;
            }
            k = next;
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

    fn key(&self, p: [f64; 3]) -> [u64; 2] {
        self.p2(p).map(f64::to_bits)
    }
}

/// The cap's triangles (D3), from the one section triangulation shared
/// with the boolean window ([`crate::triangulate_section`]), or the index
/// of a point near the fault.
fn cap_triangles(loops: &[Vec<[f64; 2]>]) -> Result<Vec<[usize; 3]>, usize> {
    crate::section::triangulate_section_touching(loops)
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
/// `oriented`, indexed exactly as `loops`, carries each loop vertex's
/// source face so each region's [`SectionRegion::seed_face`] (the lowest
/// id among them) is a stable identity (Phase 1b).
fn regions(
    loops: &[Vec<[f64; 2]>],
    oriented: &[Vec<(u32, FaceId)>],
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
            seed_face: oriented[i]
                .iter()
                .map(|&(_, f)| f)
                .min()
                .unwrap_or(FaceId(0)), // allow-swallow: an outer loop always has at least one vertex, so this never actually falls back
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
