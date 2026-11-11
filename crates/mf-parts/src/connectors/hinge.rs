//! Print-in-place pin hinge (Phase 1 Task 26, connector `hinge.pin` v1):
//! two leaves of knuckles, interleaved along one axis, around a captive
//! pin. `leaf_a` carries the odd knuckles fused to the pin; `leaf_b`
//! carries the even knuckles, bored to clear the pin by `gap`. Axial
//! gaps between knuckles and 45° cones on the pin's ends come from the
//! fit profile's `hinge_gap` and make the bores printable without
//! support, per the brief.

use mf_geometry::Mesh;
use mf_runtime::RunContext;
use mf_spec::{OperationSpec, Params};
use std::sync::OnceLock;

use crate::connector::{Connector, ConnectorError, ConnectorPart, ConnectorParts, JoinKind, Side};

const SPEC_YAML: &str = include_str!("../../../../specs/connectors/hinge.pin.yaml");

fn spec() -> &'static OperationSpec {
    static SPEC: OnceLock<OperationSpec> = OnceLock::new();
    SPEC.get_or_init(|| {
        serde_saphyr::from_str(SPEC_YAML).expect("connectors/hinge.pin.yaml is a valid spec")
    })
}

const SEGMENTS: u32 = 32;

/// A solid's positions and triangles, before merging with others into
/// one [`Mesh`].
type SolidParts = (Vec<[f64; 3]>, Vec<[u32; 3]>);

fn refuse(code: &'static str, message: impl Into<String>) -> ConnectorError {
    ConnectorError::Failed {
        code,
        message: message.into(),
    }
}

/// A cylindrical annulus (a tube, bored through), outer radius `r_out`,
/// inner radius `r_in`, from `x0` to `x1`: a knuckle with a bore.
fn bored_cylinder(r_out: f64, r_in: f64, x0: f64, x1: f64) -> SolidParts {
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    super::cylinder_wall(r_out, x0, x1, SEGMENTS, &mut positions, &mut triangles);
    // Inner wall, winding reversed (it faces into the bore).
    let inner_base = positions.len() as u32;
    let mut inner_tris = Vec::new();
    super::cylinder_wall(r_in, x0, x1, SEGMENTS, &mut positions, &mut inner_tris);
    for t in inner_tris {
        triangles.push([
            inner_base + (t[0] - inner_base),
            inner_base + (t[2] - inner_base),
            inner_base + (t[1] - inner_base),
        ]);
    }
    // End rings: an annulus face at each end, joining the outer and
    // inner rings. `base` is where this wall's two rings start (0 for
    // the outer wall, `inner_base` for the inner); each wall spans
    // `2 * SEGMENTS` vertices (the x0 ring then the x1 ring).
    let ring_indices = |base: u32, x: f64| -> Vec<u32> {
        (base..base + 2 * SEGMENTS)
            .filter(|&i| (positions[i as usize][0] - x).abs() < 1e-9)
            .collect()
    };
    for (x, flip) in [(x0, true), (x1, false)] {
        let outer = ring_indices(0, x);
        let inner = ring_indices(inner_base, x);
        for i in 0..SEGMENTS as usize {
            let j = (i + 1) % SEGMENTS as usize;
            let (a, b, c, d) = (outer[i], outer[j], inner[j], inner[i]);
            if flip {
                triangles.push([a, c, b]);
                triangles.push([a, d, c]);
            } else {
                triangles.push([a, b, c]);
                triangles.push([a, c, d]);
            }
        }
    }
    (positions, triangles)
}

/// The pin: a cylinder of radius `r` from `x0` to `x1`, with a 45-degree
/// cone closing each end (so the bore it passes through, and the
/// printer bridging over it, need no support) — one closed solid, built
/// as a single revolve so the cone-cylinder joints share vertices
/// exactly (no separate caps to leave a seam).
fn pin_mesh(r: f64, x0: f64, x1: f64) -> Mesh {
    let cone_len = r; // 45 degrees: rise equals run
    super::revolve_x(
        &[(x0 - cone_len, 0.0), (x0, r), (x1, r), (x1 + cone_len, 0.0)],
        SEGMENTS,
    )
}

fn merge(parts: &[SolidParts]) -> Mesh {
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    for (p, t) in parts {
        let base = positions.len() as u32;
        positions.extend_from_slice(p);
        triangles.extend(t.iter().map(|tri| tri.map(|i| i + base)));
    }
    Mesh::new(positions, triangles).expect("a merged set of closed solids is a valid mesh")
}

/// `merge`, plus one already-built [`Mesh`] (the pin, built by
/// [`super::revolve_x`]).
fn merge_with_mesh(parts: &[SolidParts], extra: &Mesh) -> Mesh {
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    for (p, t) in parts {
        let base = positions.len() as u32;
        positions.extend_from_slice(p);
        triangles.extend(t.iter().map(|tri| tri.map(|i| i + base)));
    }
    let base = positions.len() as u32;
    let (extra_p, extra_t) = extra.to_arrays();
    positions.extend_from_slice(&extra_p);
    triangles.extend(extra_t.iter().map(|tri| tri.map(|i| i + base)));
    Mesh::new(positions, triangles).expect("a merged set of closed solids is a valid mesh")
}

pub struct PinHinge;

impl Connector for PinHinge {
    fn id(&self) -> &'static str {
        "hinge.pin"
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
        _ctx: &RunContext,
    ) -> Result<ConnectorParts, ConnectorError> {
        let length = params
            .get_float("length")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let knuckle_diameter = params
            .get_float("knuckle_diameter")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let knuckles = params
            .get_int("knuckles")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let pin_diameter = params
            .get_float("pin_diameter")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let gap = params
            .get_float("gap")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;

        if knuckles < 3 || knuckles % 2 == 0 {
            return Err(refuse(
                "knuckles_must_be_odd",
                format!(
                    "knuckles is {knuckles}; it must be odd and at least 3, so the two leaves interleave evenly"
                ),
            ));
        }
        if pin_diameter >= knuckle_diameter {
            return Err(refuse(
                "pin_too_wide",
                format!(
                    "pin_diameter {pin_diameter} must be less than knuckle_diameter {knuckle_diameter}"
                ),
            ));
        }
        let knuckles = knuckles as u32;
        let gaps = (knuckles - 1) as f64 * gap;
        let knuckle_length = (length - gaps) / knuckles as f64;
        if knuckle_length <= gap {
            return Err(refuse(
                "hinge_too_short",
                format!(
                    "length {length} mm can't fit {knuckles} knuckles with a {gap} mm gap between them"
                ),
            ));
        }

        let r_knuckle = knuckle_diameter / 2.0;
        let r_pin = pin_diameter / 2.0;
        let r_bore = r_pin + gap;

        // The pin: one continuous cylinder from `cone_len` to
        // `length - cone_len`, with 45-degree cones closing both ends
        // at x = 0 and x = length (so the bore it passes through, and
        // the printer bridging over it, need no support).
        let cone_len = r_pin;
        let pin = pin_mesh(r_pin, cone_len, length - cone_len);
        // leaf_a's knuckle sleeves bore very slightly wider than the pin
        // (a hairline, sub-print-resolution clearance, not a sloppy
        // fit): the pin's own radius grows from 0 to r_pin across each
        // cone, so a sleeve bored to exactly r_pin would meet the cone's
        // narrower surface tangentially rather than with clear volume,
        // which the exact validator (correctly) still calls touching.
        let sleeve_bore = r_pin + 1e-4;

        let mut leaf_a_solids = Vec::new();
        let mut leaf_b_solids = Vec::new();
        for k in 0..knuckles {
            let x0 = k as f64 * (knuckle_length + gap);
            let x1 = x0 + knuckle_length;
            if k % 2 == 0 {
                // Odd-numbered knuckles (1st, 3rd, ... — 0-indexed even):
                // leaf_a, a sleeve fused onto the pin.
                leaf_a_solids.push(bored_cylinder(r_knuckle, sleeve_bore, x0, x1));
            } else {
                leaf_b_solids.push(bored_cylinder(r_knuckle, r_bore, x0, x1));
            }
        }

        let leaf_a = merge_with_mesh(&leaf_a_solids, &pin);
        let leaf_b = merge(&leaf_b_solids);

        Ok(ConnectorParts {
            parts: vec![
                ConnectorPart {
                    role: "leaf_a",
                    mesh: leaf_a,
                    side: Side::A,
                    join: JoinKind::Union,
                },
                ConnectorPart {
                    role: "leaf_b",
                    mesh: leaf_b,
                    side: Side::B,
                    join: JoinKind::Union,
                },
            ],
        })
    }
}
