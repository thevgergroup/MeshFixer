//! Connectors across a cut (the Split workflow's step 3, owner
//! 2026-11-11): `split.place_connectors`, and the joint geometry and
//! live checks the Split panel shares with it.
//!
//! - **A joint** is what a cut (`mesh.split_plane`, `mesh.split_box`)
//!   records for each cut face pair: piece `a` and piece `b`, the cut's
//!   mid-plane point, its unit normal from `a` into `b`, and the gap
//!   between the two caps (`a`'s cap at `point − normal·gap/2`, `b`'s at
//!   `point + normal·gap/2`). [`JointFrame`] adds an in-plane `u` (and
//!   `v = normal × u`) so a connector's place on the cap is 2D.
//! - **A placement** ([`Placement`]) is one connector from the connector
//!   registry (`joint.dowel`, `joint.peg`, `joint.dovetail`,
//!   `joint.magnet`, `joint.insert`, see `mf_parts::connectors::joint`):
//!   the side it was placed on (its solid cap), its 2D position, its turn
//!   in the plane and its own parameters in mm. Its counterpart on the
//!   other side is the same generated connector seen from across the
//!   cut: a rigid placement of the connector's local frame, never a
//!   mirror, so it can't come out inside-out.
//! - **`split.place_connectors`** builds every placement's parts at once
//!   and applies them in one command: holes and pockets subtracted, pegs
//!   and tails unioned (`mesh.boolean`'s engine, as `connector.join`),
//!   loose dowels laid out on the plate as their own objects (D21).
//!   Every value it reads is recorded, so replay is Exact.
//! - **The checks** ([`check_placement`]) are the live warnings the
//!   panel shows per connector, each with a one-click fix ([`fix`]):
//!   breakout (the hole's minimum distance to the piece's outer surface,
//!   `mf_geometry::minimum_distance_within`, and its column from floor to
//!   cap), edge distance on the cut face, a dowel no longer than its two
//!   holes, and the fit check (`mf_parts::check_fit`) against the Fit
//!   class and Machine profile.

use std::collections::{BTreeMap, HashMap};

use glam::{DMat3, DMat4, DVec3};
use mf_geometry::{Bvh, FaceId, Mesh, Provenance, Sources};
use mf_parts::{
    Compensation, ConnectorPart, FitClass, FitExpectation, JoinKind, MachineProfile,
    MatingSurfaces, Side, check_fit,
};
use mf_spec::Params;
use serde::{Deserialize, Serialize};

use super::failed;
use crate::layout::{Footprint, Plate, layout_on_plate};
use crate::{
    Cancelled, DocOutput, ObjectChange, OpError, OpOutput, OperandInput, Operation, RunContext,
    Target, needs_document, unsupported_target,
};

/// Which piece of a joint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JointSide {
    A,
    B,
}

impl JointSide {
    pub fn other(self) -> JointSide {
        match self {
            JointSide::A => JointSide::B,
            JointSide::B => JointSide::A,
        }
    }

    fn index(self) -> usize {
        match self {
            JointSide::A => 0,
            JointSide::B => 1,
        }
    }
}

/// A joint's plane, as a frame: `point` on the mid-plane, `normal` from
/// piece `a` into piece `b`, `u` and `v` in the plane (`u × v = normal`),
/// and the gap between the caps. World space, document units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JointFrame {
    pub point: DVec3,
    pub normal: DVec3,
    pub u: DVec3,
    pub v: DVec3,
    pub gap: f64,
}

impl JointFrame {
    /// The frame through `point` with `normal`, its `u` the part of
    /// `u_hint` in the plane (any in-plane direction if that's zero).
    pub fn new(point: DVec3, normal: DVec3, u_hint: DVec3, gap: f64) -> Option<JointFrame> {
        let n = normal.try_normalize()?;
        let u = (u_hint - n * n.dot(u_hint))
            .try_normalize()
            .unwrap_or_else(|| n.any_orthonormal_vector());
        Some(JointFrame {
            point,
            normal: n,
            u,
            v: n.cross(u),
            gap,
        })
    }

    /// The default in-plane `u` for a normal (what the Split panel
    /// records for a new joint).
    pub fn default_u(normal: DVec3) -> DVec3 {
        let n = normal.normalize_or(DVec3::Z);
        // The world axis least along the normal, made in-plane: for a
        // horizontal cut that's world X, so the cap's 2D axes read like
        // the plate's.
        let axes = [DVec3::X, DVec3::Y, DVec3::Z];
        let a = axes
            .into_iter()
            .min_by(|a, b| a.dot(n).abs().total_cmp(&b.dot(n).abs()))
            .unwrap_or(DVec3::X); // allow-swallow: the array is never empty
        (a - n * n.dot(a)).normalize_or(n.any_orthonormal_vector())
    }

    /// `uv` on the mid-plane (world).
    pub fn at(&self, uv: [f64; 2]) -> DVec3 {
        self.point + self.u * uv[0] + self.v * uv[1]
    }

    /// `p`'s 2D position in the plane.
    pub fn to_2d(&self, p: DVec3) -> [f64; 2] {
        let d = p - self.point;
        [d.dot(self.u), d.dot(self.v)]
    }

    /// The cap's plane point for `side`.
    pub fn cap_point(&self, side: JointSide) -> DVec3 {
        match side {
            JointSide::A => self.point - self.normal * self.gap / 2.0,
            JointSide::B => self.point + self.normal * self.gap / 2.0,
        }
    }

    /// The cap's outward normal (out of `side`'s piece, towards the
    /// other).
    pub fn outward(&self, side: JointSide) -> DVec3 {
        match side {
            JointSide::A => self.normal,
            JointSide::B => -self.normal,
        }
    }
}

/// One connector on a joint: the registry connector, the side it was
/// placed on (its solid cap), where (2D in the joint frame, document
/// units), its turn in the plane (degrees, from `u` towards `v`), and its
/// own parameters (mm; `gap` comes from the joint and isn't set here).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub connector: String,
    pub on: JointSide,
    pub at: [f64; 2],
    #[serde(default)]
    pub angle: f64,
    pub params: BTreeMap<String, f64>,
}

impl Placement {
    pub fn param(&self, name: &str) -> Option<f64> {
        self.params.get(name).copied()
    }
}

fn find_connector(id: &str) -> Option<&'static dyn mf_parts::Connector> {
    mf_parts::connectors().into_iter().find(|c| c.id() == id)
}

/// The placement's full, validated connector parameters (its spec's
/// defaults for anything not set), with the joint's gap (`gap_mm`).
pub fn placement_params(p: &Placement, gap_mm: f64) -> Result<Params, OpError> {
    let connector = find_connector(&p.connector)
        .filter(|_| mf_parts::connectors::joint::JOINT_CONNECTORS.contains(&p.connector.as_str()))
        .ok_or_else(|| {
            failed(
                "unknown_connector",
                format!("'{}' isn't a connector that goes across a cut", p.connector),
            )
        })?;
    let mut raw: BTreeMap<String, String> = p
        .params
        .iter()
        .filter(|(k, _)| k.as_str() != "gap")
        .map(|(k, v)| (k.clone(), v.to_string()))
        .collect();
    raw.insert("gap".into(), gap_mm.max(0.0).to_string());
    if p.connector == "joint.dovetail"
        && !raw.contains_key("groove_a")
        && !raw.contains_key("groove_b")
    {
        // A placement saved before `groove_a`/`groove_b` existed (D23)
        // has only the old single symmetric `groove_length`: split it
        // in two here, before validation fills `groove_a`/`groove_b`
        // with their own (unrelated) spec defaults, so an old save
        // keeps its old shape instead of silently becoming two 100 mm
        // reaches. `length_allowance`'s own validated default (0.4 mm)
        // is used by `generate` either way; the half-length only needs
        // `groove_length`'s own default (200 mm) when that, too, was
        // never set.
        let half = raw
            .get("groove_length")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(200.0)
            / 2.0;
        raw.insert("groove_a".into(), half.to_string());
        raw.insert("groove_b".into(), half.to_string());
    }
    connector.spec().validate(&raw).map_err(|e| {
        failed(
            "invalid_connector_params",
            format!("{}'s parameters: {e}", p.connector),
        )
    })
}

/// The connector's local frame in the world: `mm` document units per
/// millimetre (the connector is generated in mm).
pub fn placement_to_world(frame: &JointFrame, p: &Placement, mm: f64) -> DMat4 {
    let (s, c) = p.angle.to_radians().sin_cos();
    let x = frame.u * c + frame.v * s;
    let z = frame.outward(p.on);
    let y = z.cross(x);
    let r = DMat3::from_cols(x * mm, y * mm, z * mm);
    let mut m = DMat4::from_mat3(r);
    m.w_axis = frame.at(p.at).extend(1.0);
    m
}

/// `mesh` with every vertex moved by `m` (a rigid move and a uniform
/// scale: never a mirror, so the winding stays outward).
pub fn transformed(mesh: &Mesh, m: &DMat4) -> Mesh {
    let (p, t) = mesh.to_arrays();
    let p = p
        .into_iter()
        .map(|q| m.transform_point3(DVec3::from(q)).to_array())
        .collect();
    Mesh::new(p, t).expect("a moved valid mesh is valid")
}

/// A 3MF row-vector placement as a column-vector matrix.
fn mat(t: &[f64; 12]) -> DMat4 {
    DMat4::from_cols_array(&[
        t[0], t[1], t[2], 0.0, t[3], t[4], t[5], 0.0, t[6], t[7], t[8], 0.0, t[9], t[10], t[11],
        1.0,
    ])
}

/// A cut face (cap) in its joint's 2D frame: its triangles and its
/// outline (the edges only one cap face uses).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CapRegion {
    pub triangles: Vec<[[f64; 2]; 3]>,
    pub boundary: Vec<[[f64; 2]; 2]>,
    /// How far off its plane a face may be and still count as on it.
    pub tol: f64,
}

/// The ray `p + t·d` (`t ≥ 0`) meets segment `a`-`b` at parameter `t`, if
/// it does (not parallel, and the crossing is within the segment).
fn ray_seg(p: [f64; 2], d: [f64; 2], a: [f64; 2], b: [f64; 2]) -> Option<f64> {
    let e = [b[0] - a[0], b[1] - a[1]];
    let denom = d[0] * e[1] - d[1] * e[0];
    if denom.abs() < 1e-12 {
        return None;
    }
    let ap = [a[0] - p[0], a[1] - p[1]];
    let t = (ap[0] * e[1] - ap[1] * e[0]) / denom;
    let u = (ap[0] * d[1] - ap[1] * d[0]) / denom;
    (t >= 0.0 && (0.0..=1.0).contains(&u)).then_some(t)
}

fn seg_dist(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> (f64, [f64; 2]) {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let q = [a[0] + t * dx, a[1] + t * dy];
    (((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt(), q)
}

impl CapRegion {
    pub fn is_empty(&self) -> bool {
        self.triangles.is_empty()
    }

    pub fn contains(&self, p: [f64; 2]) -> bool {
        self.triangles.iter().any(|t| {
            let s = |a: [f64; 2], b: [f64; 2]| {
                (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
            };
            let (d0, d1, d2) = (s(t[0], t[1]), s(t[1], t[2]), s(t[2], t[0]));
            (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0) || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0)
        })
    }

    /// The nearest point of the outline to `p`, and its distance.
    pub fn nearest_boundary(&self, p: [f64; 2]) -> Option<(f64, [f64; 2])> {
        self.boundary
            .iter()
            .map(|s| seg_dist(p, s[0], s[1]))
            .min_by(|a, b| a.0.total_cmp(&b.0))
    }

    /// Distance from `p` to the outline: positive inside the cap,
    /// negative outside it.
    pub fn edge_distance(&self, p: [f64; 2]) -> f64 {
        let d = self.nearest_boundary(p).map_or(0.0, |(d, _)| d);
        if self.contains(p) { d } else { -d }
    }

    /// The bounds of the cap: `(min, max)`.
    pub fn bounds(&self) -> Option<([f64; 2], [f64; 2])> {
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for t in &self.triangles {
            for p in t {
                for k in 0..2 {
                    lo[k] = lo[k].min(p[k]);
                    hi[k] = hi[k].max(p[k]);
                }
            }
        }
        lo[0].is_finite().then_some((lo, hi))
    }

    /// How far from `p` the outline is, straight along unit direction
    /// `d` (the first crossing with `t > 0`; `None` if the ray never
    /// meets it, which can only happen if `p` is already outside, or the
    /// outline is degenerate). A dovetail's groove reach on one side is
    /// this, from its centre along its slide direction (and the other
    /// side, along `-d`): "the boundary of the solid cap's outline in a
    /// given direction" the live check and the drag-to-place UI both
    /// need.
    pub fn ray_boundary(&self, p: [f64; 2], d: [f64; 2]) -> Option<f64> {
        self.boundary
            .iter()
            .filter_map(|s| ray_seg(p, d, s[0], s[1]))
            .filter(|t| *t > 1e-9)
            .fold(None, |best, t| Some(best.map_or(t, |b: f64| b.min(t))))
    }

    /// The cap's area.
    pub fn area(&self) -> f64 {
        self.triangles
            .iter()
            .map(|t| {
                ((t[1][0] - t[0][0]) * (t[2][1] - t[0][1])
                    - (t[2][0] - t[0][0]) * (t[1][1] - t[0][1]))
                    .abs()
                    / 2.0
            })
            .sum()
    }
}

/// Whether `face`'s world corners `w` lie on `side`'s cap plane, facing
/// out of it.
fn on_cap(w: [DVec3; 3], frame: &JointFrame, side: JointSide, tol: f64) -> bool {
    let p = frame.cap_point(side);
    let out = frame.outward(side);
    if w.iter().any(|c| (*c - p).dot(out).abs() > tol) {
        return false;
    }
    (w[1] - w[0])
        .cross(w[2] - w[0])
        .try_normalize()
        .is_some_and(|n| n.dot(out) > 0.99)
}

/// How close to its cap plane a face must be to count as on it (a cap is
/// exactly on its plane in object space; this only absorbs the
/// placement's rounding, relative to how far from the origin it is).
fn cap_tolerance(frame: &JointFrame, t: &DMat4) -> f64 {
    let far = frame.point.abs().max_element() + t.w_axis.truncate().abs().max_element();
    1e-6 * (1.0 + far)
}

/// `side`'s cut face on `mesh` (placed by `transform`): every face on its
/// cap plane, facing out of the piece, in the joint's 2D frame. With
/// `bvh` (of `mesh`), an axis-aligned cap only visits the faces near its
/// plane.
pub fn cap_region(
    mesh: &Mesh,
    bvh: Option<&Bvh>,
    transform: &[f64; 12],
    frame: &JointFrame,
    side: JointSide,
    ctx: &RunContext,
) -> Result<CapRegion, Cancelled> {
    let t = mat(transform);
    let tol = cap_tolerance(frame, &t);
    let world = |c: u32| t.transform_point3(DVec3::from(mesh.corner_position(c)));
    let mut faces: Vec<(FaceId, [u32; 3])> = Vec::new();
    let mut tick = ctx.ticker();
    let mut visit = |f: FaceId| {
        if let Some(c) = mesh.face(f)
            && on_cap(c.map(world), frame, side, tol)
        {
            faces.push((f, c));
        }
    };
    // The cap plane in object space, if it's axis-aligned there.
    let inv = t.inverse();
    let p_obj = inv.transform_point3(frame.cap_point(side));
    let n_obj = (DMat3::from_mat4(t).transpose() * frame.outward(side)).normalize_or_zero();
    let axis = (0..3).find(|&k| n_obj[k].abs() > 1.0 - 1e-9);
    match (bvh, axis) {
        (Some(bvh), Some(k)) => {
            let big = f64::MAX / 4.0;
            let mut lo = [-big; 3];
            let mut hi = [big; 3];
            let slack = tol / t.x_axis.truncate().length().max(1e-12) * 4.0;
            lo[k] = p_obj[k] - slack;
            hi[k] = p_obj[k] + slack;
            bvh.for_each_overlapping(lo, hi, &mut visit);
        }
        _ => {
            for (f, _) in mesh.faces() {
                tick.tick()?;
                visit(f);
            }
        }
    }
    faces.sort_by_key(|(f, _)| *f);
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    let mut region = CapRegion {
        tol,
        ..CapRegion::default()
    };
    for (_, c) in &faces {
        region.triangles.push(c.map(|v| frame.to_2d(world(v))));
        for i in 0..3 {
            let (a, b) = (c[i], c[(i + 1) % 3]);
            *edges.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let mut once: Vec<(u32, u32)> = edges
        .into_iter()
        .filter(|(_, n)| *n == 1)
        .map(|(e, _)| e)
        .collect();
    once.sort_unstable();
    region.boundary = once
        .into_iter()
        .map(|(a, b)| [frame.to_2d(world(a)), frame.to_2d(world(b))])
        .collect();
    Ok(region)
}

/// One piece of a joint as the checks see it.
pub struct Piece<'a> {
    pub name: &'a str,
    pub mesh: &'a Mesh,
    pub transform: [f64; 12],
    pub bvh: &'a Bvh,
    pub cap: &'a CapRegion,
}

/// What the checks measure against.
#[derive(Debug, Clone)]
pub struct CheckSettings {
    /// The thinnest wall to leave (mm).
    pub min_wall: f64,
    pub fit_class: FitClass,
    pub machine: MachineProfile,
    /// Document units per millimetre.
    pub mm: f64,
}

/// What a check is about (each has its own one-click fix, [`fix`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    OffCap,
    Breakout(JointSide),
    EdgeDistance,
    DowelLength,
    Fit,
    /// A dovetail's `-x` (`true`) or `+x` (`false`) end is stopped
    /// (`open_a`/`open_b` false) but its recorded reach falls short of
    /// the cap's own outline on that side, so the tail can't actually
    /// be slid fully in or out from there either (D23).
    DovetailStopped(bool),
}

/// One live warning about a placement, in plain words.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub kind: CheckKind,
    pub message: String,
}

/// What a joint-wide check is about (D22): unlike [`CheckKind`], this
/// isn't about one placement, it's about the whole set — so it has no
/// single placement index to fix, and lives next to the connector list
/// rather than under one of its rows (`joint_checks`, the Connect
/// panel's own joint-level summary area).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JointCheckKind {
    /// Exactly one round dowel or peg, and nothing else on the joint
    /// resists turning about it.
    SingleRoundDowel,
}

/// One live, joint-wide warning, in plain words, with each one-click fix
/// it offers (by name: the Connect panel turns the name back into the
/// actual fix with [`fix_joint`]).
#[derive(Debug, Clone, PartialEq)]
pub struct JointCheck {
    pub kind: JointCheckKind,
    pub message: String,
    pub fixes: Vec<&'static str>,
}

/// Whether connector `id`'s placement, with `params`, resists the joint
/// turning about some other axis on its own: a dovetail, an insert (its
/// screw is a second, independent anchor point as far as rotation
/// goes), or a dowel/peg that isn't round (D22's whole point). A magnet
/// pulls the faces together but never resists an in-plane turn, so it
/// doesn't count.
fn resists_rotation(id: &str, params: &Params) -> bool {
    match id {
        "joint.dovetail" | "joint.insert" => true,
        "joint.dowel" | "joint.peg" => {
            !matches!(
                mf_parts::connectors::joint::Shape::from_code(
                    params.get_float("shape").unwrap_or(0.0),
                    params.get_float("sides").unwrap_or(6.0),
                ),
                mf_parts::connectors::joint::Shape::Round
            )
        }
        _ => false,
    }
}

/// The live, joint-wide checks (see [`JointCheckKind`]): today, only
/// whether the joint's only alignment is a single round dowel or peg,
/// free to turn about it since nothing else on the joint resists
/// rotation (D22). `gap_mm` is the joint's own gap, for
/// `placement_params`.
pub fn joint_checks(placements: &[Placement], gap_mm: f64) -> Vec<JointCheck> {
    let mut out = Vec::new();
    let resisting: Vec<bool> = placements
        .iter()
        .map(|p| {
            placement_params(p, gap_mm)
                .ok()
                .is_some_and(|params| resists_rotation(&p.connector, &params))
        })
        .collect();
    let round_dowels: Vec<usize> = placements
        .iter()
        .enumerate()
        .filter(|(i, p)| {
            (p.connector == "joint.dowel" || p.connector == "joint.peg") && !resisting[*i]
        })
        .map(|(i, _)| i)
        .collect();
    if round_dowels.len() == 1 && resisting.iter().all(|r| !r) {
        out.push(JointCheck {
            kind: JointCheckKind::SingleRoundDowel,
            message: "The parts can rotate around one round dowel. Add a second, or use a \
                      square or keyed shape"
                .to_owned(),
            fixes: vec!["add_second_dowel", "make_square"],
        });
    }
    out
}

/// A joint-wide check's one-click fix, applied to `placements` (see
/// [`joint_checks`]): `"add_second_dowel"` places a second
/// `joint.dowel` (copying the first round dowel/peg's own size and
/// clearance) mirrored across the solid cap's own bounds centre from
/// the first — the simplest offset that's almost always still on the
/// cap and far enough from the first hole not to collide with it, since
/// a real cap is practically never a single point; `"make_square"`
/// changes the existing dowel or peg's own `shape` param to Square in
/// place, leaving everything else about it untouched. `cap` is the
/// solid piece's own cut face, to place the second dowel sensibly;
/// `None` if the fix doesn't apply (an unknown name, or there's no
/// single round dowel/peg to fix).
pub fn fix_joint(placements: &[Placement], gap_mm: f64, cap: &CapRegion, fix: &str) -> Option<Vec<Placement>> {
    let resisting: Vec<bool> = placements
        .iter()
        .map(|p| {
            placement_params(p, gap_mm)
                .ok()
                .is_some_and(|params| resists_rotation(&p.connector, &params))
        })
        .collect();
    let round_dowels: Vec<usize> = placements
        .iter()
        .enumerate()
        .filter(|(i, p)| {
            (p.connector == "joint.dowel" || p.connector == "joint.peg") && !resisting[*i]
        })
        .map(|(i, _)| i)
        .collect();
    let &[i] = round_dowels.as_slice() else {
        return None;
    };
    match fix {
        "make_square" => {
            let mut out = placements.to_vec();
            out[i].params.insert("shape".into(), 1.0);
            Some(out)
        }
        "add_second_dowel" => {
            let (lo, hi) = cap.bounds()?;
            let centre = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
            let first = &placements[i];
            // Mirrored across the cap's own centre: as far from the
            // first hole as this one simple rule can put it, on a cap
            // of any convex-ish shape.
            let at = [
                2.0 * centre[0] - first.at[0],
                2.0 * centre[1] - first.at[1],
            ];
            let at = if cap.contains(at) { at } else { centre };
            let mut second = first.clone();
            second.at = at;
            let mut out = placements.to_vec();
            out.push(second);
            Some(out)
        }
        _ => None,
    }
}

/// The connector's outline on the cap, in the joint's 2D frame.
pub fn footprint_2d(p: &Placement, params: &Params, mm: f64) -> Result<Vec<[f64; 2]>, OpError> {
    let local = mf_parts::connectors::joint::footprint(&p.connector, params)
        .map_err(|mf_parts::ConnectorError::Failed { code, message }| failed(code, message))?;
    let (s, c) = p.angle.to_radians().sin_cos();
    // The local y axis is `outward × x`: on side B (outward −normal) it
    // runs the other way in the joint frame.
    let flip = if p.on == JointSide::A { 1.0 } else { -1.0 };
    Ok(local
        .into_iter()
        .map(|[x, y]| {
            let (x, y) = (x * mm, y * mm * flip);
            [p.at[0] + x * c - y * s, p.at[1] + x * s + y * c]
        })
        .collect())
}

fn generated(p: &Placement, params: &Params) -> Result<Vec<ConnectorPart>, OpError> {
    let connector = find_connector(&p.connector).ok_or_else(|| {
        failed(
            "unknown_connector",
            format!("'{}' isn't registered", p.connector),
        )
    })?;
    connector
        .generate(params, &RunContext::none())
        .map(|g| g.parts)
        .map_err(|mf_parts::ConnectorError::Failed { code, message }| failed(code, message))
}

/// One generated part of a placement, placed: its role, which piece it
/// joins (`None` for a loose part), how, and its mesh in the world (and
/// in the connector's own frame, in mm).
pub struct PlacedPart {
    pub role: &'static str,
    pub side: Option<JointSide>,
    pub join: JoinKind,
    pub world: Mesh,
    pub local: Mesh,
}

/// Every part of placement `p` on the joint, placed in the world: the
/// connector itself on its side and its counterpart on the other, from
/// the one generated connector (`mm` document units per millimetre).
pub fn placed_parts(
    frame: &JointFrame,
    p: &Placement,
    mm: f64,
) -> Result<Vec<PlacedPart>, OpError> {
    let params = placement_params(p, frame.gap / mm)?;
    let to_world = placement_to_world(frame, p, mm);
    Ok(generated(p, &params)?
        .into_iter()
        .map(|part| PlacedPart {
            role: part.role,
            side: part_side(p, part.side),
            join: part.join,
            world: transformed(&part.mesh, &to_world),
            local: part.mesh,
        })
        .collect())
}

/// The side of the joint a generated part belongs to.
fn part_side(p: &Placement, side: Side) -> Option<JointSide> {
    match side {
        Side::A => Some(p.on),
        Side::B => Some(p.on.other()),
        Side::Neither => None,
    }
}

/// How deep a placement's hole goes into `side`, in mm (which parameter).
fn depth_key(p: &Placement, side: JointSide) -> &'static str {
    if side == p.on {
        "depth_on"
    } else {
        "depth_other"
    }
}

/// The fit check of one placement against `settings` (None for a
/// connector with no mating part of its own).
pub fn fit_check(
    p: &Placement,
    params: &Params,
    settings: &CheckSettings,
    ctx: &RunContext,
) -> Result<Option<mf_parts::FitCheck>, OpError> {
    let Some((hole, pin)) = mf_parts::connectors::joint::fit_probe(&p.connector, params)
        .map_err(|mf_parts::ConnectorError::Failed { code, message }| failed(code, message))?
    else {
        return Ok(None);
    };
    let bvh = Bvh::build(&hole, ctx).map_err(|_| OpError::Cancelled)?;
    // `clearance` is diametral; the measured gap is radial, so the fit
    // class and machine numbers it's compared with are halved (as
    // `mesh.split_plane`'s alignment fit check does).
    let fit_class = FitClass {
        name: settings.fit_class.name.clone(),
        clearance: settings.fit_class.clearance / 2.0,
    };
    let machine = MachineProfile {
        hole_undersize: settings.machine.hole_undersize / 2.0,
        ..settings.machine.clone()
    };
    check_fit(
        match p.connector.as_str() {
            "joint.dowel" => "Dowel to hole",
            "joint.peg" => "Peg to hole",
            _ => "Tail to groove",
        },
        MatingSurfaces {
            a: &hole,
            a_bvh: &bvh,
            b: &pin,
            b_to_a: &DMat4::IDENTITY,
        },
        FitExpectation {
            fit_class: &fit_class,
            machine: &machine,
            compensation: Compensation::Hole,
        },
        ctx,
    )
    .map_err(|_| OpError::Cancelled)
}

/// The diametral clearance `settings` asks for: the Fit class's
/// clearance plus the Machine profile's hole compensation.
pub fn wanted_clearance(settings: &CheckSettings) -> f64 {
    mf_parts::resolve(&settings.fit_class, &settings.machine, Compensation::Hole)
}

/// The live checks of one placement (see the module docs). `pieces` is
/// `[a, b]`.
pub fn check_placement(
    frame: &JointFrame,
    pieces: [&Piece<'_>; 2],
    p: &Placement,
    settings: &CheckSettings,
    ctx: &RunContext,
) -> Result<Vec<Check>, OpError> {
    let mm = settings.mm;
    let params = placement_params(p, frame.gap / mm)?;
    let mut out = Vec::new();
    let on = pieces[p.on.index()];
    let other = pieces[p.on.other().index()];
    if !on.cap.contains(p.at) || !other.cap.contains(p.at) {
        let off = if on.cap.contains(p.at) { other } else { on };
        out.push(Check {
            kind: CheckKind::OffCap,
            message: format!(
                "It's off the cut face of '{}'; drag it onto the face.",
                off.name
            ),
        });
        return Ok(out);
    }
    // Edge distance: the outline's closest point to either cap's edge.
    let outline = footprint_2d(p, &params, mm)?;
    let edge = outline
        .iter()
        .map(|q| on.cap.edge_distance(*q).min(other.cap.edge_distance(*q)))
        .fold(f64::INFINITY, f64::min);
    if edge < settings.min_wall * mm {
        out.push(Check {
            kind: CheckKind::EdgeDistance,
            message: if edge <= 0.0 {
                "It runs over the edge of the cut face; move it inwards.".to_owned()
            } else {
                format!(
                    "It's {:.1} mm from the edge of the cut face; at least {:.1} mm keeps the \
                     edge from breaking.",
                    edge / mm,
                    settings.min_wall
                )
            },
        });
    }
    // Breakout, per side with a hole in it.
    for side in [JointSide::A, JointSide::B] {
        if let Some(wall) = hole_wall(frame, pieces[side.index()], side, p, &params, settings, ctx)?
            && wall < settings.min_wall * mm
        {
            out.push(Check {
                kind: CheckKind::Breakout(side),
                message: if wall <= 0.0 {
                    format!(
                        "The hole breaks out of '{}'; make it shallower or move it inwards.",
                        pieces[side.index()].name
                    )
                } else {
                    format!(
                        "The hole comes within {:.1} mm of the outside of '{}'; it may break \
                         through. At least {:.1} mm keeps it inside.",
                        wall / mm,
                        pieces[side.index()].name,
                        settings.min_wall
                    )
                },
            });
        }
    }
    if p.connector == "joint.dowel" {
        let len = mf_parts::connectors::joint::dowel_length(&params)
            .map_err(|mf_parts::ConnectorError::Failed { code, message }| failed(code, message))?;
        let room = mf_parts::connectors::joint::dowel_room(&params)
            .map_err(|mf_parts::ConnectorError::Failed { code, message }| failed(code, message))?;
        if len > room - 1e-9 {
            out.push(Check {
                kind: CheckKind::DowelLength,
                message: format!(
                    "The dowel ({len:.1} mm) is longer than its two holes together ({room:.1} \
                     mm), so the pieces won't close."
                ),
            });
        }
    }
    if p.connector == "joint.dovetail" {
        // The groove is cut into the "other" piece; its own cap outline
        // is the boundary a stopped end must reach to be withdrawable.
        let groove_cap = pieces[p.on.other().index()].cap;
        let (s, c) = p.angle.to_radians().sin_cos();
        let dir = [c, s];
        let reach = mf_parts::connectors::joint::dovetail_reach(&params)
            .map_err(|mf_parts::ConnectorError::Failed { code, message }| failed(code, message))?;
        for (open, len, sign, a_end) in [
            (reach.open_a, reach.a, -1.0, true),
            (reach.open_b, reach.b, 1.0, false),
        ] {
            if open {
                continue;
            }
            let d = [dir[0] * sign, dir[1] * sign];
            let Some(to_edge) = groove_cap.ray_boundary(p.at, d) else {
                continue;
            };
            if len * mm < to_edge - 1e-6 {
                out.push(Check {
                    kind: CheckKind::DovetailStopped(a_end),
                    message: "Can't slide together: extend it to the edge".to_owned(),
                });
            }
        }
    }
    if let Some(c) = fit_check(p, &params, settings, ctx)?
        && (c.may_fuse || c.measured_gap < c.expected_gap * 0.75)
    {
        out.push(Check {
            kind: CheckKind::Fit,
            message: c.message(),
        });
    }
    Ok(out)
}

/// The thinnest wall between `side`'s hole (if the placement cuts one
/// into it) and the piece's outer surface, document units: the BVH
/// minimum distance to every face but the cut face, and 0 if the hole's
/// column (from its floor back to the cap) leaves the piece. `None` if
/// there's no hole on that side, or the nearest surface is further than
/// a few walls away. A dovetail's groove runs out of the piece on
/// purpose, so it isn't checked.
#[allow(clippy::too_many_arguments)]
fn hole_wall(
    frame: &JointFrame,
    piece: &Piece<'_>,
    side: JointSide,
    p: &Placement,
    params: &Params,
    settings: &CheckSettings,
    ctx: &RunContext,
) -> Result<Option<f64>, OpError> {
    if p.connector == "joint.dovetail" {
        return Ok(None);
    }
    let mm = settings.mm;
    let place = mat(&piece.transform);
    let to_object = place.inverse();
    let tol = piece.cap.tol;
    let world_corner = |c: [DVec3; 3]| c.map(|q| place.transform_point3(q));
    let limit = (4.0 * settings.min_wall * mm).max(1e-9);
    let depth = params
        .get_float(depth_key(p, side))
        .map_err(|e| failed("invalid_connector_params", e.to_string()))?
        * mm;
    let mut wall: Option<f64> = None;
    for part in placed_parts(frame, p, mm)? {
        if part.join != JoinKind::Subtract || part.side != Some(side) {
            continue;
        }
        // Only the hole inside the piece: the part beyond the cap opens
        // onto the cut by design.
        let d = mf_geometry::minimum_distance_within(
            piece.mesh,
            piece.bvh,
            &part.world,
            &to_object,
            limit / place.x_axis.truncate().length().max(1e-12),
            |_, c| on_cap(world_corner(c), frame, side, tol),
            ctx,
        )
        .map_err(|_| OpError::Cancelled)?
        .map(|d| d * place.x_axis.truncate().length());
        // The column from the hole's floor back to the cap must stay in
        // the piece: the first surface the ray meets is the cap itself.
        let out = frame.outward(side);
        let floor = frame.at(p.at) - out * (frame.gap / 2.0 + depth);
        let column_ok =
            match piece
                .bvh
                .ray_pick(piece.mesh, floor.to_array(), out.to_array(), &place)
            {
                Some((f, t)) => {
                    t >= depth * (1.0 - 1e-6)
                        || piece.mesh.face(f).is_some_and(|c| {
                            on_cap(
                                c.map(|v| {
                                    place.transform_point3(DVec3::from(
                                        piece.mesh.corner_position(v),
                                    ))
                                }),
                                frame,
                                side,
                                tol,
                            )
                        })
                }
                None => false,
            };
        let w = if column_ok {
            d.unwrap_or(f64::INFINITY)
        } else {
            0.0
        };
        wall = Some(wall.map_or(w, |x: f64| x.min(w)));
    }
    Ok(wall.filter(|w| w.is_finite()))
}

/// The placement with check `kind`'s one-click fix applied, or `None`
/// if there's nothing it can change to pass. Breakout: shallower (or,
/// if no depth is shallow enough, moved inwards); edge distance or off
/// the cap: moved towards the middle of the cut face; dowel length: the
/// longest that seats; fit: the clearance the Fit class and Machine
/// profile ask for.
pub fn fix(
    frame: &JointFrame,
    pieces: [&Piece<'_>; 2],
    p: &Placement,
    kind: CheckKind,
    settings: &CheckSettings,
    ctx: &RunContext,
) -> Result<Option<Placement>, OpError> {
    let still = |q: &Placement, k: CheckKind| -> Result<bool, OpError> {
        Ok(check_placement(frame, pieces, q, settings, ctx)?
            .iter()
            .any(|c| c.kind == k))
    };
    match kind {
        CheckKind::Breakout(side) => {
            let key = depth_key(p, side);
            let params = placement_params(p, frame.gap / settings.mm)?;
            let depth = params
                .get_float(key)
                .map_err(|e| failed("invalid_connector_params", e.to_string()))?;
            let min = 0.5_f64.min(depth);
            let mut q = p.clone();
            q.params.insert(key.into(), min);
            if !still(&q, kind)? {
                // Bisect for the deepest hole that stays inside.
                let (mut lo, mut hi) = (min, depth);
                for _ in 0..20 {
                    let mid = (lo + hi) / 2.0;
                    q.params.insert(key.into(), mid);
                    if still(&q, kind)? {
                        hi = mid;
                    } else {
                        lo = mid;
                    }
                }
                q.params.insert(key.into(), (lo * 100.0).floor() / 100.0);
                return Ok(Some(q));
            }
            move_inwards(frame, pieces, p, kind, settings, ctx)
        }
        CheckKind::EdgeDistance | CheckKind::OffCap => {
            move_inwards(frame, pieces, p, kind, settings, ctx)
        }
        CheckKind::DowelLength => {
            let params = placement_params(p, frame.gap / settings.mm)?;
            let room = mf_parts::connectors::joint::dowel_room(&params).map_err(
                |mf_parts::ConnectorError::Failed { code, message }| failed(code, message),
            )?;
            let allowance = params
                .get_float("length_allowance")
                .map_err(|e| failed("invalid_connector_params", e.to_string()))?;
            let mut q = p.clone();
            q.params
                .insert("dowel_length".into(), room - allowance.max(0.1));
            Ok(Some(q))
        }
        CheckKind::Fit => {
            let mut q = p.clone();
            q.params.insert(
                "clearance".into(),
                (wanted_clearance(settings) * 1000.0).round() / 1000.0,
            );
            Ok(Some(q))
        }
        CheckKind::DovetailStopped(a_end) => {
            let groove_cap = pieces[p.on.other().index()].cap;
            let (s, c) = p.angle.to_radians().sin_cos();
            let sign = if a_end { -1.0 } else { 1.0 };
            let d = [c * sign, s * sign];
            let Some(to_edge) = groove_cap.ray_boundary(p.at, d) else {
                return Ok(None);
            };
            let mut q = p.clone();
            // A touch past the boundary (never short of it, however the
            // ray's own rounding falls), so the check can't immediately
            // re-fire on the fixed placement.
            let key = if a_end { "groove_a" } else { "groove_b" };
            q.params
                .insert(key.into(), to_edge / settings.mm + 0.5);
            Ok(Some(q))
        }
    }
}

/// The point of both caps furthest from their edges (a grid search, then
/// refined around the best cell).
pub fn cap_middle(caps: [&CapRegion; 2]) -> Option<[f64; 2]> {
    let (lo, hi) = caps[0].bounds()?;
    let score = |p: [f64; 2]| caps[0].edge_distance(p).min(caps[1].edge_distance(p));
    let mut best = (
        f64::NEG_INFINITY,
        [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0],
    );
    let (mut c, mut h) = (best.1, [(hi[0] - lo[0]) / 2.0, (hi[1] - lo[1]) / 2.0]);
    for _ in 0..6 {
        for i in 0..=16 {
            for j in 0..=16 {
                let p = [
                    c[0] - h[0] + 2.0 * h[0] * i as f64 / 16.0,
                    c[1] - h[1] + 2.0 * h[1] * j as f64 / 16.0,
                ];
                let s = score(p);
                if s > best.0 {
                    best = (s, p);
                }
            }
        }
        c = best.1;
        h = [h[0] / 4.0, h[1] / 4.0];
    }
    (best.0 > 0.0).then_some(best.1)
}

/// Moves the placement towards the middle of the cut face, as little as
/// passes `kind` (bisecting along the way).
fn move_inwards(
    frame: &JointFrame,
    pieces: [&Piece<'_>; 2],
    p: &Placement,
    kind: CheckKind,
    settings: &CheckSettings,
    ctx: &RunContext,
) -> Result<Option<Placement>, OpError> {
    let Some(mid) = cap_middle([pieces[0].cap, pieces[1].cap]) else {
        return Ok(None);
    };
    let at = |t: f64| {
        [
            p.at[0] + (mid[0] - p.at[0]) * t,
            p.at[1] + (mid[1] - p.at[1]) * t,
        ]
    };
    let fails = |t: f64| -> Result<bool, OpError> {
        let q = Placement {
            at: at(t),
            ..p.clone()
        };
        let checks = check_placement(frame, pieces, &q, settings, ctx)?;
        Ok(checks.iter().any(|c| {
            c.kind == kind || c.kind == CheckKind::OffCap || c.kind == CheckKind::EdgeDistance
        }))
    };
    if fails(1.0)? {
        return Ok(Some(Placement {
            at: mid,
            ..p.clone()
        }));
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..20 {
        let m = (lo + hi) / 2.0;
        if fails(m)? {
            lo = m;
        } else {
            hi = m;
        }
    }
    Ok(Some(Placement {
        at: at(hi),
        ..p.clone()
    }))
}

// ---------------------------------------------------------------------
// split.place_connectors
// ---------------------------------------------------------------------

pub struct PlaceConnectors;

/// A new object's mesh and placement.
type Placed = (Mesh, [f64; 12]);

fn created_provenance(mesh: &Mesh) -> Provenance {
    Provenance {
        created_faces: mesh.faces().map(|(f, _)| (f, Sources::default())).collect(),
        created_vertices: mesh.vertices().map(|(v, _)| v).collect(),
        ..Provenance::default()
    }
}

fn world_footprint(mesh: &Mesh, t: &[f64; 12]) -> Option<Footprint> {
    let m = mat(t);
    let mut f = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for (_, p) in mesh.vertices() {
        let w = m.transform_point3(DVec3::from(p));
        f = [f[0].min(w.x), f[1].min(w.y), f[2].max(w.x), f[3].max(w.y)];
    }
    f[0].is_finite().then_some(f)
}

impl Operation for PlaceConnectors {
    fn id(&self) -> &'static str {
        "split.place_connectors"
    }

    fn version(&self) -> u32 {
        1
    }

    fn execute(
        &self,
        _: &Mesh,
        _: &Target,
        _: &Params,
        _: &RunContext,
    ) -> Result<OpOutput, OpError> {
        Err(needs_document(self.id()))
    }

    fn wants_scene(&self) -> bool {
        true
    }

    fn execute_doc(
        &self,
        inputs: &[OperandInput],
        params: &Params,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        self.execute_scene(inputs, &[], params, ctx)
    }

    fn execute_scene(
        &self,
        inputs: &[OperandInput],
        scene: &[crate::SceneObject],
        params: &Params,
        ctx: &RunContext,
    ) -> Result<DocOutput, OpError> {
        let [a, b] = inputs else {
            return Err(failed(
                "missing_operand",
                "split.place_connectors needs the joint's two pieces (the target and side_b)",
            ));
        };
        for o in [a, b] {
            if o.target != Target::WholeObject {
                return Err(unsupported_target(self.id(), &o.target));
            }
        }
        let point = DVec3::from(params.get_vec3("plane_point")?);
        let normal = DVec3::from(params.get_vec3("plane_normal")?);
        let u = DVec3::from(params.get_vec3("plane_u")?);
        let gap = params.get_float("gap")?;
        let mm = params.get_float("mm")?;
        if mm.is_nan() || mm <= 0.0 {
            return Err(failed("invalid_units", "mm must be positive"));
        }
        let frame = JointFrame::new(point, normal, u, gap)
            .ok_or_else(|| failed("invalid_plane", "The joint's plane has no direction."))?;
        let placements: Vec<Placement> = serde_json::from_str(params.get_text("connectors")?)
            .map_err(|e| failed("invalid_connectors", format!("connectors isn't valid: {e}")))?;
        if placements.is_empty() {
            return Err(failed(
                "no_connectors",
                "There are no connectors to add; click on the cut face to place one.",
            ));
        }
        let fit_clearance = params.get_float("fit_clearance")?;
        let machine_tolerance = params.get_float("machine_tolerance")?;
        let settings = CheckSettings {
            min_wall: params.get_float("min_wall")? / mm,
            fit_class: FitClass {
                name: "Recorded".into(),
                clearance: fit_clearance / mm,
            },
            machine: MachineProfile {
                name: "Recorded".into(),
                calibrated: machine_tolerance != 0.0,
                outer_xy: 0.0,
                hole_undersize: machine_tolerance / mm,
                z: 0.0,
                print_volume_mm: MachineProfile::uncalibrated().print_volume_mm,
            },
            mm,
        };
        // Both cut faces, to refuse a connector that isn't on them.
        let caps = [
            cap_region(a.mesh, None, &a.transform, &frame, JointSide::A, ctx)?,
            cap_region(b.mesh, None, &b.transform, &frame, JointSide::B, ctx)?,
        ];
        for (side, cap) in [(a, &caps[0]), (b, &caps[1])] {
            if cap.is_empty() {
                return Err(failed(
                    "no_cut_face",
                    format!(
                        "'{}' has no cut face on this joint's plane (was it cut here?).",
                        side.name
                    ),
                ));
            }
        }
        // (subtract, union) tools per side, in world space; loose parts.
        let mut tools: [(Vec<Mesh>, Vec<Mesh>); 2] = Default::default();
        let mut loose: Vec<Mesh> = Vec::new();
        let mut fits = Vec::new();
        let mut warnings = Vec::new();
        for (k, p) in placements.iter().enumerate() {
            ctx.checkpoint()?;
            if !caps[0].contains(p.at) || !caps[1].contains(p.at) {
                return Err(failed(
                    "connector_off_cap",
                    format!(
                        "Connector {} isn't on the cut face; drag it onto the face.",
                        k + 1
                    ),
                ));
            }
            let cp = placement_params(p, gap / mm)?;
            for part in placed_parts(&frame, p, mm)? {
                match (part.join, part.side) {
                    (JoinKind::Separate, _) => loose.push(transformed(
                        &part.local,
                        &DMat4::from_scale(DVec3::splat(mm)),
                    )),
                    (JoinKind::Subtract, Some(s)) => tools[s.index()].0.push(part.world),
                    (JoinKind::Union, Some(s)) => tools[s.index()].1.push(part.world),
                    (_, None) => {
                        return Err(failed(
                            "connector_part_unassigned",
                            format!("{}'s '{}' joins neither piece", p.connector, part.role),
                        ));
                    }
                }
            }
            let fit = fit_check(p, &cp, &settings, ctx)?;
            if p.connector == "joint.dowel" {
                let len = mf_parts::connectors::joint::dowel_length(&cp).map_err(
                    |mf_parts::ConnectorError::Failed { code, message }| failed(code, message),
                )?;
                let room = mf_parts::connectors::joint::dowel_room(&cp).map_err(
                    |mf_parts::ConnectorError::Failed { code, message }| failed(code, message),
                )?;
                if len > room - 1e-9 {
                    warnings.push(format!(
                        "Connector {}: the dowel ({len:.1} mm) is longer than its two holes \
                         together ({room:.1} mm)",
                        k + 1
                    ));
                }
            }
            if let Some(c) = &fit
                && c.may_fuse
            {
                warnings.push(format!("Connector {}: {}", k + 1, c.message()));
            }
            fits.push(fit.map_or(serde_json::Value::Null, |c| {
                serde_json::json!({
                    "pair": c.pair,
                    "measured_gap": c.measured_gap,
                    "expected_gap": c.expected_gap,
                    "may_fuse": c.may_fuse,
                    "message": c.message(),
                })
            }));
        }
        // Apply: subtract the holes, then union the pegs and tails, on
        // each piece (provenance relative to its input).
        let mut changes = Vec::new();
        for (k, piece) in [a, b].into_iter().enumerate() {
            let (sub, uni) = &tools[k];
            let mut mesh = piece.mesh.clone();
            let mut prov: Option<Provenance> = None;
            for (list, op) in [
                (sub, mf_boolean::BoolOp::Subtract),
                (uni, mf_boolean::BoolOp::Union),
            ] {
                if list.is_empty() {
                    continue;
                }
                ctx.checkpoint()?;
                let tool = mf_boolean::operand_in_frame(
                    &super::pegs::merged(list),
                    &crate::affine::IDENTITY,
                    &piece.transform,
                )
                .ok_or_else(|| {
                    failed(
                        "singular_transform",
                        "A piece's transform can't be inverted.",
                    )
                })?;
                let (m, p) = super::pegs::apply_tool(&mesh, &tool, op, ctx)?;
                prov = Some(match &prov {
                    Some(prev) => super::pegs::compose(prev, &p),
                    None => p,
                });
                mesh = m;
            }
            if let Some(provenance) = prov {
                changes.push(ObjectChange::Geometry {
                    operand: k,
                    mesh,
                    provenance,
                });
            }
        }
        // Loose dowels: upright on the plate, clear of everything.
        let mut dowels: Vec<Placed> = Vec::new();
        let mut off_plate = Vec::new();
        if !loose.is_empty() {
            let mut prints = Vec::new();
            let mut upright = Vec::new();
            for m in &loose {
                let bb = m
                    .bounding_box()
                    .ok_or_else(|| failed("empty_part", "a dowel has no faces"))?;
                let c = bb.center();
                let shift = DMat4::from_translation(DVec3::new(-c[0], -c[1], -bb.min[2]));
                let m = transformed(m, &shift);
                prints.push(world_footprint(&m, &crate::affine::IDENTITY).unwrap_or([0.0; 4])); // allow-swallow: a dowel always has vertices (checked just above)
                upright.push(m);
            }
            let occupied: Vec<Footprint> = [a, b]
                .iter()
                .filter_map(|o| world_footprint(o.mesh, &o.transform))
                .chain(scene.iter().map(|o| o.footprint()))
                .collect();
            let plate = Plate {
                size: [
                    params.get_float("plate_width")?,
                    params.get_float("plate_depth")?,
                ],
            };
            let layout = layout_on_plate(
                &occupied,
                &prints,
                &plate,
                params.get_float("plate_spacing")?,
            );
            off_plate = layout.off_plate.clone();
            dowels = upright.into_iter().zip(layout.transforms).collect();
        }
        let n_dowels = dowels.len();
        for (k, (mesh, transform)) in dowels.into_iter().enumerate() {
            changes.push(ObjectChange::Create {
                name: format!("{} dowel {}", a.name, k + 1),
                provenance: created_provenance(&mesh),
                mesh,
                transform,
            });
        }
        if !off_plate.is_empty() {
            warnings.push(format!(
                "{} dowel{} didn't fit on the plate and {} placed beside it",
                off_plate.len(),
                if off_plate.len() == 1 { "" } else { "s" },
                if off_plate.len() == 1 { "is" } else { "are" }
            ));
        }
        Ok(DocOutput {
            changes,
            report: serde_json::json!({
                "connectors": placements.len(),
                "dowels": n_dowels,
                "fit_checks": fits,
                "warnings": warnings,
                "off_plate": off_plate,
            }),
        })
    }
}
