//! Print-in-place pin hinge as three separate pieces (connector
//! `hinge.leaf` v1, parametric-shapes brief, "Hinges"; owner bug "no
//! separation between the laps… can't attach laps 1,3,5 to A and 2,4,6
//! to B"): **leaf A** carries the odd knuckles (1st, 3rd, ...), **leaf
//! B** the even ones, and **the pin** is its own object, captive (printed
//! in place, a head at each end trapping it) or separate (printed beside
//! the hinge and pushed in after). Each leaf is a plate lying on its own
//! object's face, joined to its knuckles by a tab under each one, so it
//! attaches to its object (Solid) when the group is attached or combined.
//!
//! **Frame** (the hinge's own, placed on the edge the two objects share):
//! x along the axis, from 0 to `length`; the objects meet across y = 0,
//! A at -y and B at +y; z = 0 is the face the leaves lie on, the objects
//! behind it (-z), the axis `knuckle_diameter / 2 + gap` above it. Side B
//! opens by turning about +x through the axis.
//!
//! **Clearances** (D4: moving parts never print fused): neighbouring
//! knuckles are `gap` apart along the axis; each leaf stops `gap` short
//! of the other side (y = 0) and its plate stays `gap` clear of the other
//! leaf's knuckles; the knuckles sit `gap` above the face; every bore
//! clears the pin by `pin_clearance` radially. Both come from the Fit
//! class and Machine profile, and a value of 0 or less is refused.
//!
//! Each leaf is one closed solid: its plate, tabs and knuckles unioned by
//! the exact boolean kernel (they overlap on purpose; a mesh of
//! overlapping pieces would be refused by Combine).

use std::sync::OnceLock;

use mf_boolean::{BoolOp, ManifoldCpp};
use mf_geometry::Mesh;
use mf_runtime::RunContext;
use mf_spec::{OperationSpec, Params};
use mf_units::Unit;

use crate::connector::{ConnectorError, Side};
use crate::generator::{GenContext, Generator, PlacementHint, ShapePart};
use crate::shape::ShapeRole;

const SPEC_YAML: &str = include_str!("../../../../specs/connectors/hinge.leaf.yaml");

fn spec() -> &'static OperationSpec {
    static SPEC: OnceLock<OperationSpec> = OnceLock::new();
    SPEC.get_or_init(|| {
        serde_saphyr::from_str(SPEC_YAML).expect("connectors/hinge.leaf.yaml is a valid spec")
    })
}

const SEGMENTS: u32 = 32;

/// A length already in millimetres (every generator is mm-internal,
/// D16), through the one shared formatter (ADR-0010, consolidation plan
/// C14): never a hard-coded "mm" or a bare `{:.2}`/`{:.1}`, so a value
/// close to zero still reads as a genuine number rather than rounding
/// away to "0.00".
fn mm(v: f64) -> String {
    mf_units::format_length(v, Unit::Millimeter)
}

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

/// An axis-aligned box from `lo` to `hi`, outward winding.
fn cuboid(lo: [f64; 3], hi: [f64; 3]) -> Mesh {
    let p = |i: usize| {
        [
            if i & 1 == 0 { lo[0] } else { hi[0] },
            if i & 2 == 0 { lo[1] } else { hi[1] },
            if i & 4 == 0 { lo[2] } else { hi[2] },
        ]
    };
    let positions = (0..8).map(p).collect();
    let quads = [
        [0, 2, 3, 1], // -z
        [4, 5, 7, 6], // +z
        [0, 1, 5, 4], // -y
        [2, 6, 7, 3], // +y
        [0, 4, 6, 2], // -x
        [1, 3, 7, 5], // +x
    ];
    let triangles = quads
        .iter()
        .flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]])
        .collect();
    Mesh::new(positions, triangles).expect("a box is a valid mesh")
}

fn lifted(parts: super::hinge::SolidParts, dz: f64) -> Mesh {
    let (p, t) = parts;
    let p = p.into_iter().map(|[x, y, z]| [x, y, z + dz]).collect();
    Mesh::new(p, t).expect("a bored knuckle is a valid mesh")
}

fn union(a: Mesh, b: &Mesh, ctx: &RunContext) -> Result<Mesh, ConnectorError> {
    static KERNEL: ManifoldCpp = ManifoldCpp::checked();
    mf_boolean::boolean(&a, b, BoolOp::Union, &KERNEL, ctx)
        .map(|o| o.mesh)
        .map_err(|e| {
            refuse(
                "leaf_failed",
                format!(
                    "Couldn't build the hinge leaf ({e}); try a thicker leaf or wider knuckles."
                ),
            )
        })
}

/// The dimensions every piece is built from, checked.
struct Sizes {
    knuckles: u32,
    length: f64,
    r_knuckle: f64,
    r_pin: f64,
    r_bore: f64,
    leaf_width: f64,
    leaf_thickness: f64,
    gap: f64,
    knuckle_length: f64,
    /// The axis height above the face.
    zc: f64,
}

fn sizes(params: &Params) -> Result<Sizes, ConnectorError> {
    let knuckles = params
        .get_int("knuckles")
        .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
    let length = float(params, "length")?;
    let r_knuckle = float(params, "knuckle_diameter")? / 2.0;
    let r_pin = float(params, "pin_diameter")? / 2.0;
    let leaf_width = float(params, "leaf_width")?;
    let leaf_thickness = float(params, "leaf_thickness")?;
    let gap = float(params, "gap")?;
    let pin_clearance = float(params, "pin_clearance")?;
    if gap <= 0.0 || pin_clearance <= 0.0 {
        return Err(refuse(
            "moving_parts_fused",
            format!(
                "A print-in-place hinge needs clearance, but the fit gives {} between knuckles \
                 and {} around the pin, so it would print fused. Pick a looser Fit class \
                 (Sliding or Free running).",
                mm(gap),
                mm(pin_clearance)
            ),
        ));
    }
    if knuckles < 2 {
        return Err(refuse(
            "too_few_knuckles",
            "A hinge needs at least 2 knuckles, one for each leaf.",
        ));
    }
    let r_bore = r_pin + pin_clearance;
    // The knuckle needs some wall left outside the bore, or it prints as
    // a shell with no strength (or breaks through into open air). The
    // margin used to be a bare 0.4 mm literal; it now reuses
    // `pin_clearance` itself (consolidation plan C18: "comes from the
    // fit profile"), the Fit class/Machine profile's own resolved bore
    // clearance for this generator — a looser fit (which already needs
    // more room around the pin) also demands a thicker wall, instead of
    // a constant with no tie to either profile.
    let min_knuckle_wall = pin_clearance;
    if r_bore + min_knuckle_wall >= r_knuckle {
        return Err(refuse(
            "pin_too_wide",
            format!(
                "The pin and its clearance ({} across) leave no knuckle wall in a {} knuckle; \
                 make the knuckles wider or the pin thinner.",
                mm(2.0 * r_bore),
                mm(2.0 * r_knuckle)
            ),
        ));
    }
    if leaf_thickness >= r_knuckle + gap {
        return Err(refuse(
            "leaf_too_thick",
            format!(
                "A {} leaf would reach the other leaf's knuckles; make it thinner than {} or the \
                 knuckles wider.",
                mm(leaf_thickness),
                mm(r_knuckle + gap)
            ),
        ));
    }
    let knuckles = knuckles as u32;
    let knuckle_length = (length - (knuckles - 1) as f64 * gap) / knuckles as f64;
    if knuckle_length <= 2.0 * gap {
        return Err(refuse(
            "hinge_too_short",
            format!(
                "{} can't fit {knuckles} knuckles with a {} gap between them; make the hinge \
                 longer or use fewer knuckles.",
                mm(length),
                mm(gap)
            ),
        ));
    }
    Ok(Sizes {
        knuckles,
        length,
        r_knuckle,
        r_pin,
        r_bore,
        leaf_width,
        leaf_thickness,
        gap,
        knuckle_length,
        zc: r_knuckle + gap,
    })
}

/// Knuckle `k`'s span along the axis.
fn span(s: &Sizes, k: u32) -> (f64, f64) {
    let x0 = k as f64 * (s.knuckle_length + s.gap);
    (x0, x0 + s.knuckle_length)
}

/// The axis of the hinge in its own frame: a point on it and its
/// direction (for the swing preview).
pub fn axis(params: &Params) -> Result<([f64; 3], [f64; 3]), ConnectorError> {
    let s = sizes(params)?;
    Ok(([0.0, 0.0, s.zc], [1.0, 0.0, 0.0]))
}

/// One leaf: knuckles `first, first + 2, ...`, its plate on the face on
/// side `sign` (-1: A at -y, +1: B at +y).
fn leaf(s: &Sizes, first: u32, sign: f64, ctx: &RunContext) -> Result<Mesh, ConnectorError> {
    let embed = s.leaf_thickness / 2.0;
    let reach = s.r_knuckle + s.gap + s.leaf_width;
    let y = |a: f64, b: f64| {
        let (p, q) = (sign * a, sign * b);
        (p.min(q), p.max(q))
    };
    let (py0, py1) = y(s.r_knuckle + s.gap, reach);
    let mut mesh = cuboid([0.0, py0, -embed], [s.length, py1, s.leaf_thickness]);
    let wall = (s.r_knuckle - s.r_bore) / 2.0;
    let (ty0, ty1) = y(s.gap, reach);
    let mut k = first;
    while k < s.knuckles {
        ctx.checkpoint()
            .map_err(|_| refuse("cancelled", "cancelled by the user"))?;
        let (x0, x1) = span(s, k);
        // Inset a little from the knuckle's ends so the union never
        // meets two coplanar faces.
        let inset = (s.knuckle_length * 0.01).min(0.05);
        let tab = cuboid(
            [x0 + inset, ty0, -embed / 2.0],
            [x1 - inset, ty1, s.zc - s.r_bore - wall],
        );
        mesh = union(mesh, &tab, ctx)?;
        let knuckle = lifted(
            super::hinge::bored_cylinder(s.r_knuckle, s.r_bore, x0, x1),
            s.zc,
        );
        mesh = union(mesh, &knuckle, ctx)?;
        k += 2;
    }
    Ok(mesh)
}

/// The captive pin: through every knuckle, a head at each end (wider
/// than the bores, `gap` clear of the end knuckles) so it can't slide
/// out. One surface of revolution, so it's a single closed solid.
fn captive_pin(s: &Sizes) -> Mesh {
    let head_r = s.r_bore + 0.6 * (s.r_knuckle - s.r_bore);
    let head_t = s.r_knuckle.max(1.0);
    let (x0, x1) = (-s.gap, s.length + s.gap);
    let profile = [
        (x0 - head_t, 0.0),
        (x0 - head_t, head_r),
        (x0, head_r),
        (x0, s.r_pin),
        (x1, s.r_pin),
        (x1, head_r),
        (x1 + head_t, head_r),
        (x1 + head_t, 0.0),
    ];
    let pin = super::revolve_x(&profile, SEGMENTS);
    let (p, t) = pin.to_arrays();
    let p = p.into_iter().map(|[x, y, z]| [x, y, z + s.zc]).collect();
    Mesh::new(p, t).expect("a revolved pin is a valid mesh")
}

/// The separate pin: as long as the hinge, 45-degree ends (it prints
/// lying down without support), printed beside leaf A.
fn separate_pin(s: &Sizes) -> Mesh {
    let cone = s.r_pin;
    let profile = [
        (0.0, 0.0),
        (cone, s.r_pin),
        (s.length - cone, s.r_pin),
        (s.length, 0.0),
    ];
    let pin = super::revolve_x(&profile, SEGMENTS);
    let beside = -(s.r_knuckle + s.gap + s.leaf_width + 3.0 * s.r_knuckle);
    let (p, t) = pin.to_arrays();
    let p = p
        .into_iter()
        .map(|[x, y, z]| [x, y + beside, z + s.r_pin])
        .collect();
    Mesh::new(p, t).expect("a revolved pin is a valid mesh")
}

pub struct LeafHinge;

impl Generator for LeafHinge {
    fn name(&self) -> &'static str {
        "Pin hinge"
    }

    fn placement_hint(&self) -> PlacementHint {
        PlacementHint::Surface
    }

    fn moving_gap(&self, params: &Params) -> f64 {
        params.get_float("gap").unwrap_or(0.0)
    }

    fn id(&self) -> &'static str {
        "hinge.leaf"
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
        ctx: &GenContext,
    ) -> Result<Vec<ShapePart>, ConnectorError> {
        let ctx = ctx.run;
        let s = sizes(params)?;
        let separate = params
            .get_choice("pin_mode")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?
            == "separate";
        let leaf_a = leaf(&s, 0, -1.0, ctx)?;
        let leaf_b = leaf(&s, 1, 1.0, ctx)?;
        let pin = if separate {
            separate_pin(&s)
        } else {
            captive_pin(&s)
        };
        Ok(vec![
            ShapePart::new("leaf_a", ShapeRole::Solid, Side::A, leaf_a),
            ShapePart::new("leaf_b", ShapeRole::Solid, Side::B, leaf_b),
            ShapePart::new("pin", ShapeRole::Solid, Side::Neither, pin),
        ])
    }
}
