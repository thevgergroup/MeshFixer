//! Flap hinge (Phase 1 connector `hinge.flap` v1): two leaves of
//! knuckles around a pin, like [`super::hinge::PinHinge`], plus each
//! leaf's flat wing (the part `hinge.pin`'s spec reserved
//! `leaf_width`/`leaf_thickness` for but never generated) — and a choice
//! of assembly: `captive` fuses the pin into `leaf_a` and prints the
//! whole hinge in place, like the pin hinge; `separate` makes the pin
//! its own loose part (`ShapeRole::Solid`, printed apart and inserted
//! after), with both leaves' knuckles bored to clear it by `gap`.
//!
//! Geometry mirrors `hinge.pin`'s knuckle interleaving and 45-degree pin
//! cones (so a captive bore prints without support). Each knuckle
//! carries its own wing tab fused on as one watertight solid
//! ([`bored_paddle`]/[`paddle_outline`]) rather than a separately-closed
//! wing plate glued to the knuckles at a shared face: this codebase's
//! exact validator refuses any two faces that merely touch without
//! sharing a vertex (D4), so the knuckle and its wing tab have to be one
//! continuous cross-section from the start, not two solids merged after.

use mf_geometry::Mesh;
use mf_spec::{OperationSpec, Params};
use std::sync::OnceLock;

use crate::connector::{ConnectorError, Side};
use crate::generator::{GenContext, Generator, PlacementHint, ShapePart};
use crate::shape::ShapeRole;

const SPEC_YAML: &str = include_str!("../../../../specs/connectors/hinge.flap.yaml");

fn spec() -> &'static OperationSpec {
    static SPEC: OnceLock<OperationSpec> = OnceLock::new();
    SPEC.get_or_init(|| {
        serde_saphyr::from_str(SPEC_YAML).expect("connectors/hinge.flap.yaml is a valid spec")
    })
}

const SEGMENTS: u32 = 32;

type SolidParts = (Vec<[f64; 3]>, Vec<[u32; 3]>);

fn refuse(code: &'static str, message: impl Into<String>) -> ConnectorError {
    ConnectorError::Failed {
        code,
        message: message.into(),
    }
}

/// A solid cylinder, radius `r`, from `x0` to `x1`, both ends closed by
/// a 45-degree cone (so a bore it passes through needs no support) —
/// identical construction to `hinge::pin_mesh`.
fn pin_mesh(r: f64, x0: f64, x1: f64) -> Mesh {
    let cone_len = r;
    super::revolve_x(
        &[(x0 - cone_len, 0.0), (x0, r), (x1, r), (x1 + cone_len, 0.0)],
        SEGMENTS,
    )
}

/// The angle (of [`SEGMENTS`] samples around a full circle) nearest
/// where a wing of `leaf_thickness` would naturally join the circle
/// (`half_t.atan2(sqrt(r^2-half_t^2))`), snapped to the nearest existing
/// sample so [`paddle_outline`]'s circular part reuses a plain circle's
/// own sample grid exactly (at least 1 step from angle 0, so the
/// rectangle always replaces at least one facet).
fn junction_step(r_knuckle: f64, leaf_thickness: f64) -> u32 {
    let half_t = (leaf_thickness / 2.0).min(r_knuckle * 0.999);
    let a0 = half_t.atan2((r_knuckle * r_knuckle - half_t * half_t).max(0.0).sqrt());
    ((a0 / std::f64::consts::TAU * SEGMENTS as f64).round() as u32).clamp(1, SEGMENTS / 2 - 1)
}

/// The outer "paddle" outline of a knuckle fused to its wing tab, in the
/// (y,z) plane: a circle (radius `r_knuckle`) sampled on the same
/// [`SEGMENTS`]-point grid a plain bore circle uses, with the facets
/// within `junction_step` of angle 0 (the +y axis) replaced by a
/// rectangle reaching out to `r_knuckle + leaf_width` — built
/// analytically (no boolean kernel in this crate), so the knuckle and
/// its wing tab are one watertight solid instead of two separately-
/// closed solids merely touching (which this codebase's exact validator
/// always refuses, D4: touching that isn't vertex-shared is still
/// "meeting"). Reusing the plain circle's own sample grid (rather than
/// an independent angle chosen from `leaf_thickness` directly) keeps
/// this ring's circular part identical, point for point, to a bore
/// ring's own grid, so [`bored_paddle`]'s end-cap annulus between them
/// connects corresponding points directly without a twisted cap.
fn paddle_outline(r_knuckle: f64, leaf_width: f64, leaf_thickness: f64) -> Vec<[f64; 2]> {
    let y_edge = r_knuckle + leaf_width;
    let n = SEGMENTS;
    let k = junction_step(r_knuckle, leaf_thickness);
    let angle = |i: u32| std::f64::consts::TAU * i as f64 / n as f64;
    let at = |i: u32| [r_knuckle * angle(i).cos(), r_knuckle * angle(i).sin()];
    let mut out = Vec::new();
    // The circle's arc from sample `k` round through y = -r to sample
    // `n - k` (i.e. every sample except the `2k - 1` nearest angle 0,
    // on the +y side, that the rectangle replaces).
    for i in k..=n - k {
        out.push(at(i));
    }
    // Close via the rectangle: from the last arc point (sample `n-k`,
    // near -z) straight out to the far edge, up, and back to the first
    // arc point (sample `k`, near +z) — reusing those two samples' own
    // z, not a separately computed `half_t`.
    let z_lo = at(n - k)[1];
    let z_hi = at(k)[1];
    out.push([y_edge, z_lo]);
    out.push([y_edge, z_hi]);
    out
}

/// A ring's wall, swept along x from `x0` to `x1` (the 2D `ring` is in
/// (y,z), counter-clockwise, like the profiles `extrude_y`-style
/// functions use but with x as the swept axis instead of y). Returns
/// where its `2 * ring.len()` vertices start.
fn ring_wall_x(
    ring: &[[f64; 2]],
    x0: f64,
    x1: f64,
    positions: &mut Vec<[f64; 3]>,
    triangles: &mut Vec<[u32; 3]>,
) -> u32 {
    let n = ring.len() as u32;
    let base = positions.len() as u32;
    for &x in &[x0, x1] {
        positions.extend(ring.iter().map(|p| [x, p[0], p[1]]));
    }
    for i in 0..n {
        let j = (i + 1) % n;
        let (a, b) = (base + i, base + j);
        let (c, d) = (base + n + j, base + n + i);
        // A CCW (y,z) profile swept along x (unlike `extrude_y`'s (x,z)
        // profile swept along y, this is the opposite chirality):
        // [a,c,d] and [a,b,c] face outward.
        triangles.push([a, c, d]);
        triangles.push([a, b, c]);
    }
    base
}

/// A knuckle fused to its wing tab, bored to `r_in`, spanning `x0` to
/// `x1`: one watertight solid. The outer paddle ring and the inner bore
/// ring are sampled with the same point count and the bore ring is
/// placed at the outer ring's own angular positions near its circular
/// part (and at matching rectangle-corner positions elsewhere), so the
/// end-cap annulus between them connects corresponding points directly
/// (the same construction as the plain round `hinge::bored_cylinder`,
/// generalized): this keeps the caps simple and planar-ish across the
/// knuckle/wing proportions this connector's spec allows (`leaf_width`
/// at least 1 mm, `leaf_thickness` at least 0.4 mm).
fn bored_paddle(
    r_knuckle: f64,
    r_in: f64,
    leaf_width: f64,
    leaf_thickness: f64,
    x0: f64,
    x1: f64,
) -> SolidParts {
    let outer = paddle_outline(r_knuckle, leaf_width, leaf_thickness);
    // The bore ring, built the same way as `outer` (same sample grid,
    // same `k` arc range and 2 replacement points at those samples'
    // own angle) but at radius `r_in` and never reaching past its own
    // circle (so it stays a plain, slightly gapped circle, not a
    // paddle): point-for-point correspondence with `outer`'s circular
    // part keeps the annulus end caps below simple.
    let k = junction_step(r_knuckle, leaf_thickness);
    let n_full = SEGMENTS;
    let inner_at = |a: f64| [r_in * a.cos(), r_in * a.sin()];
    let step = std::f64::consts::TAU / n_full as f64;
    let mut inner = Vec::new();
    for i in k..=n_full - k {
        inner.push(inner_at(i as f64 * step));
    }
    // Outer's rectangle replaces the `2k - 1` samples strictly between
    // angle `-k*step` and `k*step` (going through 0) with 2 new points
    // (its far-edge corners). Inner has no rectangle, so it fills that
    // same angular gap with 2 new points of its own, part way across
    // it (not duplicating the arc's own endpoints): a plain circle,
    // just sampled a little more finely there, matching outer's point
    // count exactly.
    let gap = 2.0 * k as f64 * step;
    let gap_start = -(k as f64) * step;
    inner.push(inner_at(gap_start + gap / 3.0));
    inner.push(inner_at(gap_start + 2.0 * gap / 3.0));
    let n = outer.len();
    debug_assert_eq!(n, inner.len(), "outer and inner rings must correspond 1:1");
    let mut positions = Vec::new();
    let mut triangles = Vec::new();
    let outer_base = ring_wall_x(&outer, x0, x1, &mut positions, &mut triangles);
    let inner_base = positions.len() as u32;
    // The bore's wall needs the opposite winding from the outer wall
    // (its outward-facing-into-the-material normal points towards the
    // bore axis, not away from it): swap the two side-wall triangles'
    // winding after building them normally (same trick as the plain
    // cylinder's bore in `hinge::bored_cylinder`).
    let inner_tri_start = triangles.len();
    ring_wall_x(&inner, x0, x1, &mut positions, &mut triangles);
    for t in &mut triangles[inner_tri_start..] {
        t.swap(1, 2);
    }
    let n = n as u32;
    // End caps: an annulus between the outer paddle ring and the inner
    // bore ring at each end (x0, flipped since it faces -x; x1).
    for (ring_offset, flip) in [(0u32, true), (n, false)] {
        let oa = outer_base + ring_offset;
        let ia = inner_base + ring_offset;
        for i in 0..n {
            let j = (i + 1) % n;
            let (a, b, c, d) = (oa + i, oa + j, ia + j, ia + i);
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

pub struct FlapHinge;

impl Generator for FlapHinge {
    fn name(&self) -> &'static str {
        "Flap hinge"
    }

    fn placement_hint(&self) -> PlacementHint {
        PlacementHint::Surface
    }

    fn moving_gap(&self, params: &Params) -> f64 {
        params.get_float("gap").unwrap_or(0.0)
    }

    fn id(&self) -> &'static str {
        "hinge.flap"
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
        let length = params
            .get_float("length")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let knuckle_diameter = params
            .get_float("knuckle_diameter")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let knuckles = params
            .get_int("knuckles")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let leaf_width = params
            .get_float("leaf_width")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let leaf_thickness = params
            .get_float("leaf_thickness")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let pin_diameter = params
            .get_float("pin_diameter")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let gap = params
            .get_float("gap")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let pin_mode = params
            .get_choice("pin_mode")
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
        let separate = pin_mode == "separate";

        let cone_len = r_pin;
        let pin = pin_mesh(r_pin, cone_len, length - cone_len);
        // A hairline, sub-print-resolution clearance for the captive
        // sleeve's bore (`hinge::PinHinge` uses the same idea, a
        // sleeve bored to exactly r_pin meets the pin's cone
        // tangentially, which the exact validator correctly still
        // calls touching). Here the margin is twice the chordal error
        // (sagitta) of a SEGMENTS-sided circle at r_pin, not a fixed
        // 1e-4: the paddle's bore ring and the pin's cone are each
        // faceted independently, so their facets' phases don't align
        // the way two plain cylinders' do, and a fixed hairline isn't
        // always enough clearance against both facetings' sagitta at
        // once.
        let sagitta = r_pin * (1.0 - (std::f64::consts::PI / SEGMENTS as f64).cos());
        let sleeve_bore = r_pin + (2.0 * sagitta).max(1e-4);

        // Each knuckle carries its own wing tab fused on as one
        // watertight solid (`bored_paddle`, see its doc comment): a
        // separately-closed wing plate glued to the knuckles at a
        // shared face isn't valid here (this codebase's exact
        // validator refuses touching that isn't vertex-shared, D4).
        let mut leaf_a_solids = Vec::new();
        let mut leaf_b_solids = Vec::new();
        for k in 0..knuckles {
            let x0 = k as f64 * (knuckle_length + gap);
            let x1 = x0 + knuckle_length;
            let bore_a = if separate { r_bore } else { sleeve_bore };
            if k % 2 == 0 {
                leaf_a_solids.push(bored_paddle(
                    r_knuckle,
                    bore_a,
                    leaf_width,
                    leaf_thickness,
                    x0,
                    x1,
                ));
            } else {
                leaf_b_solids.push(bored_paddle(
                    r_knuckle,
                    r_bore,
                    leaf_width,
                    leaf_thickness,
                    x0,
                    x1,
                ));
            }
        }

        let (leaf_a, mut parts) = if separate {
            (merge(&leaf_a_solids), Vec::new())
        } else {
            (merge_with_mesh(&leaf_a_solids, &pin), Vec::new())
        };
        let leaf_b = merge(&leaf_b_solids);

        parts.push(ShapePart::new("leaf_a", ShapeRole::Solid, Side::A, leaf_a));
        parts.push(ShapePart::new("leaf_b", ShapeRole::Solid, Side::B, leaf_b));
        if separate {
            parts.push(ShapePart::new("pin", ShapeRole::Solid, Side::Neither, pin));
        }

        Ok(parts)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may panic on failure
mod tests {
    use super::*;

    fn assert_closed_solid(mesh: &Mesh, label: &str) {
        let ctx = mf_runtime::RunContext::none();
        let bvh = mf_geometry::Bvh::build(mesh, &ctx).unwrap();
        let all: Vec<mf_geometry::FaceId> = mesh.faces().map(|(f, _)| f).collect();
        mf_geometry::validate_solid(mesh, &all, &bvh, &ctx)
            .unwrap()
            .unwrap_or_else(|d| panic!("{label}: not a valid solid: {d}"));
    }

    /// `paddle_outline` is a simple, counter-clockwise polygon (no
    /// self-crossings the ear-clipper or a boolean kernel would choke
    /// on): checked indirectly, through `bored_paddle` being a valid
    /// closed solid for a range of knuckle/wing proportions, including
    /// a wing thicker than the knuckle's own diameter (clamped inside
    /// `paddle_outline`).
    #[test]
    fn bored_paddle_is_closed_for_typical_and_edge_proportions() {
        // The spec's own defaults; its minimum leaf_width/leaf_thickness
        // (1.0 and 0.4 mm) on a wide knuckle; and a case where
        // leaf_thickness exceeds 2*r_knuckle (clamped inside
        // `paddle_outline`). The bore ring is built on the same sample
        // grid as the outer paddle ring's circular part (`junction_step`),
        // so the end-cap annulus between them corresponds point for
        // point regardless of how thin the wing tab is.
        for (r_knuckle, r_in, leaf_width, leaf_thickness) in [
            (3.0, 1.0, 8.0, 2.4),
            (3.0, 1.0, 1.0, 0.4),
            (1.0, 0.3, 10.0, 5.0), // leaf_thickness > 2*r_knuckle, clamped
        ] {
            let (p, t) = bored_paddle(r_knuckle, r_in, leaf_width, leaf_thickness, 0.0, 10.0);
            let mesh = Mesh::new(p, t).unwrap();
            assert_closed_solid(
                &mesh,
                &format!("r_knuckle={r_knuckle} leaf_thickness={leaf_thickness}"),
            );
        }
    }

    /// The paddle's outer boundary reaches exactly `r_knuckle +
    /// leaf_width` at its far (wing) edge and `r_knuckle` at its round
    /// part, measured on the generated solid's own bounding box.
    #[test]
    fn bored_paddle_reaches_the_wing_edge_and_the_knuckle_radius() {
        let (p, t) = bored_paddle(3.0, 1.0, 8.0, 2.0, 0.0, 10.0);
        let mesh = Mesh::new(p, t).unwrap();
        let bb = mesh.bounding_box().unwrap();
        assert!((bb.max[1] - 11.0).abs() < 1e-9, "{bb:?}"); // y: 3 + 8
        // z: +-r_knuckle, within the 32-segment circle's chordal error.
        assert!((bb.max[2] - 3.0).abs() < 0.05, "{bb:?}");
        assert!((bb.min[2] + 3.0).abs() < 0.05, "{bb:?}");
    }
}
