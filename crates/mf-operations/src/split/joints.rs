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
    Compensation, FitClass, FitExpectation, Generator, JoinKind, MachineProfile, MatingSurfaces,
    ShapePart, Side, check_fit,
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

fn find_connector(id: &str) -> Option<&'static dyn Generator> {
    mf_parts::find_generator(id)
}

/// The hole profile choices, by the code a [`Placement`] records them as
/// (`hole_profile`: 0 auto, 1 round, 2 teardrop, 3 flat-top): its spec's
/// own choice order, as every choice param a placement records.
pub const HOLE_PROFILES: [&str; 4] = ["auto", "round", "teardrop", "flat_top"];

/// The placement's generator, if it's one placed on a cut face.
pub fn placement_generator(p: &Placement) -> Result<&'static dyn Generator, OpError> {
    find_connector(&p.connector)
        .filter(|g| g.placement_hint().on_cut())
        .ok_or_else(|| {
            failed(
                "unknown_connector",
                format!("'{}' isn't a connector that goes across a cut", p.connector),
            )
        })
}

/// The placement's full, validated connector parameters (its spec's
/// defaults for anything not set), with the joint's gap (`gap_mm`). A
/// placement's params are numbers: a choice param is recorded as its
/// index in the spec's values and handed to the generator as the word;
/// a placement recorded by an older schema is upgraded first
/// (`Generator::upgrade_params`).
pub fn placement_params(p: &Placement, gap_mm: f64) -> Result<Params, OpError> {
    let connector = placement_generator(p)?;
    let spec = connector.spec();
    let mut raw: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in &p.params {
        if k == "gap" {
            continue;
        }
        let text = match spec.parameter(k).map(|ps| &ps.kind) {
            Some(mf_spec::ParamKind::Choice { values, .. }) => {
                let i = v.round().clamp(0.0, (values.len().max(1) - 1) as f64) as usize;
                values.get(i).cloned().unwrap_or_default() // allow-swallow: i is clamped into values; empty values validate as a refusal below
            }
            Some(mf_spec::ParamKind::Bool { .. }) => (*v != 0.0).to_string(),
            Some(_) => v.to_string(),
            // Not in the spec: an old field, for `upgrade_params`.
            None => v.to_string(),
        };
        raw.insert(k.clone(), text);
    }
    raw.insert("gap".into(), gap_mm.max(0.0).to_string());
    connector.upgrade_params(&mut raw);
    raw.retain(|k, _| spec.parameter(k).is_some());
    spec.validate(&raw).map_err(|e| {
        failed(
            "invalid_connector_params",
            format!("{}'s parameters: {e}", p.connector),
        )
    })
}

/// `p` with `patch` applied to its recorded params (a choice recorded as
/// its index in the spec's values, like every placement param).
pub fn patched(p: &Placement, patch: &mf_parts::ParamPatch) -> Placement {
    let spec = find_connector(&p.connector).map(|g| g.spec());
    let mut q = p.clone();
    for (k, v) in &patch.set {
        let n = match v {
            mf_spec::ParamValue::Float(f) => Some(*f),
            mf_spec::ParamValue::Int(i) => Some(*i as f64),
            mf_spec::ParamValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            mf_spec::ParamValue::Text(t) => {
                spec.and_then(|s| s.parameter(k))
                    .and_then(|ps| match &ps.kind {
                        mf_spec::ParamKind::Choice { values, .. } => {
                            values.iter().position(|x| x == t).map(|i| i as f64)
                        }
                        _ => None,
                    })
            }
            _ => None,
        };
        if let Some(n) = n {
            q.params.insert(k.clone(), n);
        }
    }
    q
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

/// [`placement_params`] with the hole profile resolved for where the
/// placement sits on the joint (fix item 9, D24): a round hole whose
/// axis runs within 45 degrees of horizontal in the world (the pieces'
/// print orientation) becomes a teardrop under `auto`, its point facing
/// world up. A pure function of the recorded placement and frame, so
/// replay builds the same profile.
pub fn oriented_params(frame: &JointFrame, p: &Placement, mm: f64) -> Result<Params, OpError> {
    let params = placement_params(p, frame.gap / mm)?;
    let world = placement_to_world(frame, p, mm);
    Ok(mf_parts::shape::orient_params(
        &params,
        DMat3::from_mat4(world),
    ))
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

/// The generic checks' ids (every placement gets them, whatever its
/// generator); a generator's own checks carry their own ids.
pub mod check_id {
    /// Its centre isn't on both cut faces.
    pub const OFF_CAP: &str = "off_cap";
    /// Too close to (or over) a cut face's edge.
    pub const EDGE_DISTANCE: &str = "edge_distance";
    /// A hole too close to the outside of the piece on side A or B.
    pub const BREAKOUT_A: &str = "breakout.a";
    pub const BREAKOUT_B: &str = "breakout.b";
    /// The mating pair's exact gap against the Fit class.
    pub const FIT: &str = "fit";

    pub fn breakout(side: super::JointSide) -> &'static str {
        match side {
            super::JointSide::A => BREAKOUT_A,
            super::JointSide::B => BREAKOUT_B,
        }
    }
}

/// One live warning about a placement, in plain words, with its one-click
/// fix's words. A generator's own check carries its fix (a param patch);
/// a generic one's is worked out when it's clicked ([`fix`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub id: String,
    pub message: String,
    pub fix_label: Option<String>,
    pub patch: Option<mf_parts::ParamPatch>,
}

impl Check {
    fn generic(id: &str, message: String, fix_label: &str) -> Check {
        Check {
            id: id.to_owned(),
            message,
            fix_label: Some(fix_label.to_owned()),
            patch: None,
        }
    }
}

/// One joint-wide fix: its id (for [`fix_joint`]) and its button's words.
#[derive(Debug, Clone, PartialEq)]
pub struct JointFix {
    pub id: String,
    pub label: String,
}

/// One live, joint-wide warning (D22): about the whole set of
/// placements, not one, so it lives next to the connector list. Today's
/// only one, [`SINGLE_ROUND_PIN`]: exactly one round pin and nothing
/// else on the joint resists turning about it.
#[derive(Debug, Clone, PartialEq)]
pub struct JointCheck {
    pub id: &'static str,
    pub message: String,
    pub fixes: Vec<JointFix>,
}

/// [`JointCheck`]'s id for "the parts can rotate around one round pin".
pub const SINGLE_ROUND_PIN: &str = "single_round_pin";
/// [`fix_joint`]'s fix that adds a second pin like the first.
pub const ADD_SECOND: &str = "add_second";
/// [`fix_joint`]'s fix that makes the pin resist turning (its
/// generator's own [`mf_parts::Anchor::RoundPin`] fix).
pub const ANCHOR: &str = "anchor";

/// Each placement's [`mf_parts::Anchor`] (`None` for one that can't be
/// read; its own checks say why).
fn anchors(placements: &[Placement], gap_mm: f64) -> Vec<Option<mf_parts::Anchor>> {
    placements
        .iter()
        .map(|p| {
            // allow-swallow: a placement that can't be read has no anchor; its own checks say why
            let g = placement_generator(p).ok()?;
            // allow-swallow: as above
            let params = placement_params(p, gap_mm).ok()?;
            Some(g.anchor(&params))
        })
        .collect()
}

/// The only round pin on a joint where nothing resists turning: its index
/// and its anchor fix.
fn single_round_pin(placements: &[Placement], gap_mm: f64) -> Option<(usize, mf_parts::Fix)> {
    use mf_parts::Anchor;
    let anchors = anchors(placements, gap_mm);
    if anchors.iter().any(|a| matches!(a, Some(Anchor::Resists))) {
        return None;
    }
    let pins: Vec<(usize, mf_parts::Fix)> = anchors
        .into_iter()
        .enumerate()
        .filter_map(|(i, a)| match a {
            Some(Anchor::RoundPin { fix }) => Some((i, fix)),
            _ => None,
        })
        .collect();
    match <[_; 1]>::try_from(pins) {
        Ok([one]) => Some(one),
        Err(_) => None,
    }
}

/// The live, joint-wide checks: today, whether the joint's only alignment
/// is a single round pin, free to turn about it (D22). `gap_mm` is the
/// joint's own gap, for `placement_params`.
pub fn joint_checks(placements: &[Placement], gap_mm: f64) -> Vec<JointCheck> {
    let Some((i, fix)) = single_round_pin(placements, gap_mm) else {
        return Vec::new();
    };
    let name = find_connector(&placements[i].connector).map_or_else(
        || placements[i].connector.clone(),
        |g| g.name().to_lowercase(),
    );
    vec![JointCheck {
        id: SINGLE_ROUND_PIN,
        message: format!(
            "The parts can rotate around one round {name}. Add a second, or use a square or \
             keyed shape"
        ),
        fixes: vec![
            JointFix {
                id: ADD_SECOND.into(),
                label: format!("Add second {name}"),
            },
            JointFix {
                id: ANCHOR.into(),
                label: fix.label,
            },
        ],
    }]
}

/// A joint-wide check's one-click fix, applied to `placements` (see
/// [`joint_checks`]): [`ADD_SECOND`] places a second pin like the first,
/// mirrored across the solid cap's own bounds centre (the simplest offset
/// that's almost always still on the cap and far from the first);
/// [`ANCHOR`] applies the pin's own anti-rotation patch. `cap` is the
/// solid piece's cut face; `None` if the fix doesn't apply.
pub fn fix_joint(
    placements: &[Placement],
    gap_mm: f64,
    cap: &CapRegion,
    fix: &str,
) -> Option<Vec<Placement>> {
    let (i, anchor_fix) = single_round_pin(placements, gap_mm)?;
    match fix {
        ANCHOR => {
            let mut out = placements.to_vec();
            out[i] = patched(&placements[i], &anchor_fix.patch);
            Some(out)
        }
        ADD_SECOND => {
            let (lo, hi) = cap.bounds()?;
            let centre = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
            let first = &placements[i];
            let at = [2.0 * centre[0] - first.at[0], 2.0 * centre[1] - first.at[1]];
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

/// The connector's outline on the cap, in the joint's 2D frame: the
/// section of its own generated parts on the side it was placed on, at
/// that side's cap (`mf_parts::generator::section_outline`), so the
/// footprint is always exactly what it builds.
pub fn footprint_2d(p: &Placement, params: &Params, mm: f64) -> Result<Vec<[f64; 2]>, OpError> {
    let gap_mm = params.get_float("gap").unwrap_or(0.0); // allow-swallow: a generator without a gap param has its cap at z = 0
    let local =
        mf_parts::generator::section_outline(&generated(p, params)?, Side::A, -gap_mm / 2.0);
    Ok(local_to_2d(p, &local, mm))
}

/// Points in placement `p`'s own frame (x along its turn, y = outward ×
/// x, mm) as 2D points in the joint frame (document units).
pub fn local_to_2d(p: &Placement, local: &[[f64; 2]], mm: f64) -> Vec<[f64; 2]> {
    let (s, c) = p.angle.to_radians().sin_cos();
    // The local y axis is `outward × x`: on side B (outward −normal) it
    // runs the other way in the joint frame.
    let flip = if p.on == JointSide::A { 1.0 } else { -1.0 };
    local
        .iter()
        .map(|&[x, y]| {
            let (x, y) = (x * mm, y * mm * flip);
            [p.at[0] + x * c - y * s, p.at[1] + x * s + y * c]
        })
        .collect()
}

fn generated(p: &Placement, params: &Params) -> Result<Vec<ShapePart>, OpError> {
    let connector = find_connector(&p.connector).ok_or_else(|| {
        failed(
            "unknown_connector",
            format!("'{}' isn't registered", p.connector),
        )
    })?;
    connector
        .generate(params, &mf_parts::GenContext::new(&RunContext::none()))
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
    let params = oriented_params(frame, p, mm)?;
    let to_world = placement_to_world(frame, p, mm);
    Ok(generated(p, &params)?
        .into_iter()
        .map(|part| PlacedPart {
            role: part.name,
            side: part_side(p, part.attach_to),
            join: part.join(),
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

/// The param that sets how deep a placement's hole goes into `side` (the
/// cut-face connectors' shared convention), if its spec has it.
fn depth_key(p: &Placement, side: JointSide) -> Option<&'static str> {
    let key = if side == p.on {
        "depth_on"
    } else {
        "depth_other"
    };
    find_connector(&p.connector)
        .is_some_and(|g| g.spec().parameter(key).is_some())
        .then_some(key)
}

/// The fit check of one placement against `settings` (None for a
/// connector with no mating part of its own).
pub fn fit_check(
    p: &Placement,
    params: &Params,
    settings: &CheckSettings,
    ctx: &RunContext,
) -> Result<Option<mf_parts::FitCheck>, OpError> {
    let Some(probe) = placement_generator(p)?
        .fit_probe(params)
        .map_err(|mf_parts::ConnectorError::Failed { code, message }| failed(code, message))?
    else {
        return Ok(None);
    };
    let bvh = Bvh::build(&probe.hole, ctx).map_err(|_| OpError::Cancelled)?;
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
        &probe.label,
        MatingSurfaces {
            a: &probe.hole,
            a_bvh: &bvh,
            b: &probe.pin,
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

/// The cut faces as a generator's [`mf_parts::Targets`]: in placement
/// `p`'s own local frame (x along its turn, y = outward × x, in mm), slot
/// A is the piece it was placed on, B the other.
pub struct CapTargets<'a> {
    pub caps: [&'a CapRegion; 2],
    pub placement: &'a Placement,
    pub mm: f64,
}

impl mf_parts::Targets for CapTargets<'_> {
    fn boundary_along(&self, slot: Side, dir: [f64; 2]) -> Option<f64> {
        let p = self.placement;
        let side = match slot {
            Side::A => p.on,
            Side::B => p.on.other(),
            Side::Neither => return None,
        };
        let (s, c) = p.angle.to_radians().sin_cos();
        // Local y is `outward × x`: on side B it runs the other way.
        let flip = if p.on == JointSide::A { 1.0 } else { -1.0 };
        let d = [
            dir[0] * c - dir[1] * flip * s,
            dir[0] * s + dir[1] * flip * c,
        ];
        self.caps[side.index()]
            .ray_boundary(p.at, d)
            .map(|t| t / self.mm)
    }
}

/// The placement's generator's own checks, with the cut faces as its
/// targets.
pub fn generator_checks(
    caps: [&CapRegion; 2],
    p: &Placement,
    params: &Params,
    mm: f64,
    ctx: &RunContext,
) -> Result<Vec<Check>, OpError> {
    let g = placement_generator(p)?;
    let targets = CapTargets {
        caps,
        placement: p,
        mm,
    };
    Ok(
        g.checks(params, &mf_parts::GenContext::with_targets(ctx, &targets))
            .into_iter()
            .map(|c| Check {
                id: c.id,
                message: c.message,
                fix_label: c.fix.as_ref().map(|f| f.label.clone()),
                patch: c.fix.map(|f| f.patch),
            })
            .collect(),
    )
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
        out.push(Check::generic(
            check_id::OFF_CAP,
            format!(
                "It's off the cut face of '{}'; drag it onto the face.",
                off.name
            ),
            "Move it inwards",
        ));
        return Ok(out);
    }
    // Edge distance: the outline's closest point to either cap's edge.
    let outline = footprint_2d(p, &params, mm)?;
    let edge = outline
        .iter()
        .map(|q| on.cap.edge_distance(*q).min(other.cap.edge_distance(*q)))
        .fold(f64::INFINITY, f64::min);
    if edge < settings.min_wall * mm {
        out.push(Check::generic(
            check_id::EDGE_DISTANCE,
            if edge <= 0.0 {
                "It runs over the edge of the cut face; move it inwards.".to_owned()
            } else {
                format!(
                    "It's {:.1} mm from the edge of the cut face; at least {:.1} mm keeps the \
                     edge from breaking.",
                    edge / mm,
                    settings.min_wall
                )
            },
            "Move it inwards",
        ));
    }
    // Breakout, per side with a hole in it.
    for side in [JointSide::A, JointSide::B] {
        if let Some(wall) = hole_wall(frame, pieces[side.index()], side, p, settings, ctx)?
            && wall < settings.min_wall * mm
        {
            out.push(Check::generic(
                check_id::breakout(side),
                if wall <= 0.0 {
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
                "Make it shallower",
            ));
        }
    }
    out.extend(generator_checks(
        [pieces[0].cap, pieces[1].cap],
        p,
        &params,
        mm,
        ctx,
    )?);
    if let Some(c) = fit_check(p, &params, settings, ctx)?
        && (c.may_fuse || c.measured_gap < c.expected_gap * 0.75)
    {
        out.push(Check::generic(
            check_id::FIT,
            c.message(),
            "Use the fit's clearance",
        ));
    }
    Ok(out)
}

/// The thinnest wall between `side`'s hole (if the placement cuts one
/// into it) and the piece's outer surface, document units: the BVH
/// minimum distance to every face but the cut face, and 0 if the hole's
/// column (from its floor back to the cap) leaves the piece. `None` if
/// there's no hole on that side, the nearest surface is further than a
/// few walls away, or the generator's holes run out of the piece on
/// purpose (`Generator::checks_breakout`).
fn hole_wall(
    frame: &JointFrame,
    piece: &Piece<'_>,
    side: JointSide,
    p: &Placement,
    settings: &CheckSettings,
    ctx: &RunContext,
) -> Result<Option<f64>, OpError> {
    if !placement_generator(p)?.checks_breakout() {
        return Ok(None);
    }
    let mm = settings.mm;
    let place = mat(&piece.transform);
    let to_object = place.inverse();
    let tol = piece.cap.tol;
    let world_corner = |c: [DVec3; 3]| c.map(|q| place.transform_point3(q));
    let limit = (4.0 * settings.min_wall * mm).max(1e-9);
    let gap_mm = frame.gap / mm;
    let mut wall: Option<f64> = None;
    for part in placed_parts(frame, p, mm)? {
        if part.join != JoinKind::Subtract || part.side != Some(side) {
            continue;
        }
        // How deep the hole reaches behind its own cap, from its own
        // mesh (local z: side A's cap at -gap/2 facing +z, B's at +gap/2).
        let Some(bb) = part.local.bounding_box() else {
            continue;
        };
        let depth = if side == p.on {
            -gap_mm / 2.0 - bb.min[2]
        } else {
            bb.max[2] - gap_mm / 2.0
        } * mm;
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

/// The placement with check `id`'s one-click fix applied, or `None` if
/// there's nothing it can change to pass. A generator's own check
/// carries its fix (a param patch); the generic ones: breakout makes the
/// hole shallower (or, if no depth is shallow enough, moves it inwards);
/// edge distance or off the cap moves it towards the middle of the cut
/// face; fit sets every fit-tied param to what the Fit class and Machine
/// profile ask for.
pub fn fix(
    frame: &JointFrame,
    pieces: [&Piece<'_>; 2],
    p: &Placement,
    id: &str,
    settings: &CheckSettings,
    ctx: &RunContext,
) -> Result<Option<Placement>, OpError> {
    let still = |q: &Placement| -> Result<bool, OpError> {
        Ok(check_placement(frame, pieces, q, settings, ctx)?
            .iter()
            .any(|c| c.id == id))
    };
    let breakout = [JointSide::A, JointSide::B]
        .into_iter()
        .find(|s| check_id::breakout(*s) == id);
    if let Some(side) = breakout {
        let Some(key) = depth_key(p, side) else {
            return move_inwards(frame, pieces, p, id, settings, ctx);
        };
        let params = placement_params(p, frame.gap / settings.mm)?;
        let depth = params
            .get_float(key)
            .map_err(|e| failed("invalid_connector_params", e.to_string()))?;
        let min = 0.5_f64.min(depth);
        let mut q = p.clone();
        q.params.insert(key.into(), min);
        if !still(&q)? {
            // Bisect for the deepest hole that stays inside.
            let (mut lo, mut hi) = (min, depth);
            for _ in 0..20 {
                let mid = (lo + hi) / 2.0;
                q.params.insert(key.into(), mid);
                if still(&q)? {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            q.params.insert(key.into(), (lo * 100.0).floor() / 100.0);
            return Ok(Some(q));
        }
        return move_inwards(frame, pieces, p, id, settings, ctx);
    }
    match id {
        check_id::EDGE_DISTANCE | check_id::OFF_CAP => {
            move_inwards(frame, pieces, p, id, settings, ctx)
        }
        check_id::FIT => {
            // Every param the generator's schema ties to the fit
            // (`fit_role`), resolved afresh from the Fit class and
            // Machine profile the check measured against.
            let spec = placement_generator(p)?.spec();
            let mut raw = BTreeMap::new();
            mf_parts::fill_fit_role_params(spec, &mut raw, &settings.fit_class, &settings.machine);
            let mut q = p.clone();
            for (k, v) in raw {
                if let Ok(v) = v.parse::<f64>() {
                    q.params.insert(k, (v * 1000.0).round() / 1000.0);
                }
            }
            Ok(Some(q))
        }
        _ => Ok(check_placement(frame, pieces, p, settings, ctx)?
            .into_iter()
            .find(|c| c.id == id)
            .and_then(|c| c.patch)
            .map(|patch| patched(p, &patch))),
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
    id: &str,
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
        Ok(checks
            .iter()
            .any(|c| c.id == id || c.id == check_id::OFF_CAP || c.id == check_id::EDGE_DISTANCE))
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
        // Every part, each to its piece (world space, so its placement is
        // the identity); loose parts laid out below.
        let mut parts: Vec<crate::PartIn> = Vec::new();
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
            let cp = oriented_params(&frame, p, mm)?;
            for part in placed_parts(&frame, p, mm)? {
                let Some(side) = part.side else {
                    loose.push(transformed(
                        &part.local,
                        &DMat4::from_scale(DVec3::splat(mm)),
                    ));
                    continue;
                };
                parts.push(crate::PartIn {
                    name: format!("Connector {} ({})", k + 1, part.role),
                    mesh: std::borrow::Cow::Owned(part.world),
                    transform: crate::affine::IDENTITY,
                    role: if part.join == JoinKind::Subtract {
                        mf_parts::ShapeRole::Hole
                    } else {
                        mf_parts::ShapeRole::Solid
                    },
                    host: side.index(),
                    operand: None,
                });
            }
            let fit = fit_check(p, &cp, &settings, ctx)?;
            for c in generator_checks([&caps[0], &caps[1]], p, &cp, mm, ctx)? {
                warnings.push(format!("Connector {}: {}", k + 1, c.message));
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
        // Apply: on each piece, the holes cut, then the pegs and tails
        // joined (the one apply path, `crate::apply_parts`).
        let mut changes: Vec<ObjectChange> = crate::apply_parts(
            [Some((0, a)), Some((1, b))],
            &parts,
            crate::ApplyOrder::MergedHolesFirst,
            ctx,
        )?
        .into_iter()
        .map(|r| ObjectChange::Geometry {
            operand: r.host,
            mesh: r.mesh,
            provenance: r.provenance,
        })
        .collect();
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
