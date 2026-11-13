//! The dovetail as a linked parametric pair (owner decision 2026-11-12:
//! moved out of Connect's point placement into the parametric shape
//! system, `dovetail.slide` v1). Unlike the legacy `joint.dovetail`
//! (`joint.rs`, Connect's drag-to-place line segment, D23, kept
//! unchanged so its old `split.place_connectors` logs keep replaying
//! Exact), this connector is placed, moved and rotated like any other
//! connector group (the Parts library, the Connectors tool, Transform's
//! gizmo, `object.move_group`).
//!
//! - **The tail** is Solid, unioned onto the piece it's placed on (side
//!   A). **The groove** is a Hole, cut from the other piece (side B),
//!   offset from the tail by the fit class's clearance plus the machine
//!   tolerance (same two-part compensation every other connector's
//!   clearance already uses, `Compensation::None` resolved by the
//!   caller into `fit_clearance`/`machine_tolerance`).
//! - **Local frame:** z is the cut's normal, from side A into side B;
//!   side A's cap at `z = -gap/2`, side B's at `z = +gap/2`; the tail
//!   slides along local x, centred at `x = y = 0` (matching the legacy
//!   dovetail's own frame, `joint.rs`'s module docs).
//! - **Extend through edge** ([`extended_length`]): when on (the
//!   default), the recorded `length` auto-extends along the slide axis
//!   until it passes the target piece's own bounding box along that
//!   axis by [`EXTEND_MARGIN_MM`], re-evaluated whenever the group moves
//!   or turns (`shape_panel::after_commit`, the same idle-reconciliation
//!   pattern a hole profile's re-orientation already uses). A stopped
//!   dovetail, or one whose recorded length doesn't reach the edge, is
//!   refused at generate time with a warning the desktop can offer a
//!   one-click fix for (turning `extend_through_edge` on).

use std::sync::OnceLock;

use mf_geometry::Mesh;
use mf_spec::{OperationSpec, Params};

use crate::connector::{ConnectorError, Side};
use crate::generator::{GenContext, Generator, PlacementHint, ShapePart};
use crate::shape::ShapeRole;

const SPEC_YAML: &str = include_str!("../../../../specs/connectors/dovetail.slide.yaml");

fn spec() -> &'static OperationSpec {
    static SPEC: OnceLock<OperationSpec> = OnceLock::new();
    SPEC.get_or_init(|| {
        serde_saphyr::from_str(SPEC_YAML).expect("connectors/dovetail.slide.yaml is a valid spec")
    })
}

/// How far the tail (or groove) must clear the target piece's boundary
/// once extended, mm (the brief's "by about 1 mm").
pub const EXTEND_MARGIN_MM: f64 = 1.0;

fn refuse(code: &'static str, message: impl Into<String>) -> ConnectorError {
    ConnectorError::Failed {
        code,
        message: message.into(),
    }
}

fn float(params: &Params, name: &str) -> Result<f64, ConnectorError> {
    params
        .get_float(name)
        .map_err(|e| refuse("invalid_parameter", e.to_string()))
}

fn boolean(params: &Params, name: &str) -> Result<bool, ConnectorError> {
    params
        .get_bool(name)
        .map_err(|e| refuse("invalid_parameter", e.to_string()))
}

/// The dimensions every part is built from, checked. `through` (stopped
/// vs. all the way through) doesn't change the generated geometry at
/// all — a stopped dovetail's groove and tail are exactly the same
/// shape, just not guaranteed to clear the target piece's edge — so it's
/// read ([`boolean`] validates it's actually a bool) but not kept here;
/// it's the desktop's `dovetail_tool::ends_inside` that cares which the
/// placement is.
struct Sizes {
    width: f64,
    angle_deg: f64,
    depth: f64,
    length: f64,
    clearance: f64,
    gap: f64,
}

fn sizes(params: &Params) -> Result<Sizes, ConnectorError> {
    boolean(params, "through")?;
    let s = Sizes {
        width: float(params, "width")?,
        angle_deg: float(params, "angle")?,
        depth: float(params, "depth")?,
        length: float(params, "length")?,
        clearance: float(params, "fit_clearance")? + float(params, "machine_tolerance")?,
        gap: float(params, "gap")?,
    };
    if s.clearance >= s.width {
        return Err(refuse(
            "clearance_too_wide",
            format!(
                "The groove's clearance ({:.2} mm) is as wide as the tail ({:.2} mm), so there'd \
                 be nothing left; pick a tighter Fit class or widen the dovetail.",
                s.clearance, s.width
            ),
        ));
    }
    if s.depth <= 0.0 {
        return Err(refuse(
            "depth_too_shallow",
            "The dovetail needs a depth greater than 0 to cut a groove or sink a tail.",
        ));
    }
    Ok(s)
}

/// A dovetail's section, in (y, s) where `s` is the depth into side B
/// from its cap: `width` across at `s = 0`, widening by `tan(angle)` per
/// side with depth, from `s0` to `s1` (mirrors `joint::dovetail_section`,
/// but with a param-driven flank angle rather than the legacy fixed 15°).
fn section(width: f64, angle_deg: f64, s0: f64, s1: f64, cap_b: f64) -> Vec<[f64; 2]> {
    let t = angle_deg.to_radians().tan();
    let w = |s: f64| width + 2.0 * s * t;
    vec![
        [-w(s0) / 2.0, cap_b + s0],
        [w(s0) / 2.0, cap_b + s0],
        [w(s1) / 2.0, cap_b + s1],
        [-w(s1) / 2.0, cap_b + s1],
    ]
}

/// A closed prism along z: the convex, counter-clockwise `section` in
/// (x, y), from `z0` to `z1` (mirrors `joint::prism_z`; proven correct
/// winding, exercised throughout `joint.rs`'s own dowel/peg/dovetail
/// generation).
fn prism_z(section: &[[f64; 2]], z0: f64, z1: f64) -> Mesh {
    let n = section.len() as u32;
    let mut positions: Vec<[f64; 3]> = section.iter().map(|p| [p[0], p[1], z0]).collect();
    positions.extend(section.iter().map(|p| [p[0], p[1], z1]));
    let mut triangles = Vec::new();
    for i in 1..n - 1 {
        triangles.push([0, i + 1, i]);
        triangles.push([n, n + i, n + i + 1]);
    }
    for i in 0..n {
        let j = (i + 1) % n;
        triangles.push([i, j, n + j]);
        triangles.push([i, n + j, n + i]);
    }
    Mesh::new(positions, triangles).expect("a convex prism is a valid mesh")
}

/// A closed prism along x: the convex, counter-clockwise `section` in
/// (y, z), from `x0` to `x1`. Built as [`prism_z`] (proven winding) and
/// turned by the rotation `(x, y, z) -> (z, x, y)` (determinant +1,
/// never a mirror, so the outward winding carries over exactly as
/// `joint::prism_x` turns its own `prism_z`).
fn prism_x(section: &[[f64; 2]], x0: f64, x1: f64) -> Mesh {
    let along_z = prism_z(section, x0, x1);
    let (p, t) = along_z.to_arrays();
    let turned: Vec<[f64; 3]> = p.into_iter().map(|[a, b, c]| [c, a, b]).collect();
    Mesh::new(turned, t).expect("a turned prism is a valid mesh")
}

fn part(name: &'static str, mesh: Mesh, side: Side, role: ShapeRole) -> ShapePart {
    ShapePart::new(name, role, side, mesh)
}

fn generate(params: &Params) -> Result<Vec<ShapePart>, ConnectorError> {
    let s = sizes(params)?;
    let half = s.length / 2.0;
    let (a, b) = (-s.gap / 2.0, s.gap / 2.0);
    // The groove starts a little outside side B and runs its full depth;
    // the tail's root sinks into side A by the same depth, narrowing
    // towards the root but never below half its width at the cut (as the
    // legacy dovetail's own root clamp, `joint::generate`'s "joint.dovetail"
    // arm).
    let t = s.angle_deg.to_radians().tan();
    let groove = section(s.width, s.angle_deg, -s.width / 4.0, s.depth, b);
    let root = (b - a + s.depth).min(0.25 * (s.width - s.clearance) / t.max(1e-6));
    let tail = section(s.width - s.clearance, s.angle_deg, -root, s.depth, b);
    Ok(vec![
        part(
            "tail",
            prism_x(&tail, -half, half),
            Side::A,
            ShapeRole::Solid,
        ),
        part(
            "groove",
            prism_x(&groove, -half, half),
            Side::B,
            ShapeRole::Hole,
        ),
    ])
}

/// The length (along the slide axis) the tail needs to clear the target
/// piece's own bounding box on both ends, in the tail's local frame:
/// `target_lo`/`target_hi` (`target_lo <= target_hi`) are the target's
/// bounding box projected onto the tail's local x axis (so the caller
/// does the one transform; this stays pure geometry, unit-testable
/// without a mesh or a document). The tail is centred at local x = 0, so
/// its +x half must reach at least [`EXTEND_MARGIN_MM`] past
/// `target_hi`, and its -x half at least that far past `target_lo`;
/// returns twice the larger of those two needed half-lengths, never
/// shorter than `current_length` (extending only ever grows the
/// dovetail, it doesn't shrink one the person set deliberately longer
/// than the piece).
pub fn extended_length(current_length: f64, target_lo: f64, target_hi: f64) -> f64 {
    let half_plus = target_hi + EXTEND_MARGIN_MM;
    let half_minus = -target_lo + EXTEND_MARGIN_MM;
    let half_needed = half_plus.max(half_minus).max(0.0);
    (2.0 * half_needed).max(current_length)
}

/// True if a tail of `length`, centred on the slide axis, exits the
/// target's bounding box on at least one end (`target_lo`/`target_hi` as
/// [`extended_length`]): the "ends inside the piece" check. A `through`
/// dovetail must exit; a stopped one is exempt (it's allowed to end
/// inside, that's what "stopped" means) but [`exits_piece`] is still the
/// right question to ask before trusting a stopped length someone typed
/// in by hand, which is why the desktop asks it regardless and only
/// gates the warning on `through` itself.
pub fn exits_piece(length: f64, target_lo: f64, target_hi: f64) -> bool {
    let half = length / 2.0;
    half >= target_hi + EXTEND_MARGIN_MM * 0.999 || -half <= target_lo - EXTEND_MARGIN_MM * 0.999
}

pub struct DovetailSlide;

impl Generator for DovetailSlide {
    fn name(&self) -> &'static str {
        "Dovetail"
    }

    fn placement_hint(&self) -> PlacementHint {
        PlacementHint::Surface
    }

    fn id(&self) -> &'static str {
        "dovetail.slide"
    }

    fn version(&self) -> u32 {
        1
    }

    fn spec(&self) -> &'static OperationSpec {
        spec()
    }

    fn generate(
        &self,
        params: &Params,
        _ctx: &GenContext,
    ) -> Result<Vec<ShapePart>, ConnectorError> {
        generate(params)
    }
}
