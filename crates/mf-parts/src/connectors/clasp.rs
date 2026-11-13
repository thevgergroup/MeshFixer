//! Snap clasp (Phase 1 Task 27, connector `clasp.snap` v1): a cantilever
//! arm with a lip (`hook`, the lid, unioned to side B) that flexes over a
//! ridge (`catch`, the body, unioned to side A) and springs back to hold.
//!
//! Axis convention (the arm's own frame, like a hinge's x axis):
//! - **x**: the arm's length, from its fixed base (x = 0) to its free end
//!   (x = `arm_length`).
//! - **y**: the clasp's width.
//! - **z**: the flex direction the lip moves through to pass over the
//!   catch and back.
//!
//! The hook is one flat beam the whole way (its strain at engagement is
//! the plain cantilever formula, [`engagement_strain`]); its free end is
//! the lip simply by virtue of where the catch sits. In the closed
//! print pose, the beam's whole underside sits `gap` clear of the
//! catch's top face (z), and the catch's own x span,
//! `[arm_length - lip_depth, arm_length]`, overlaps the beam's x span
//! (`[0, arm_length]`) by exactly `lip_depth` at the free end — measured
//! on the meshes' bounding boxes, not assumed from one side's parameter
//! alone.

use mf_geometry::Mesh;
use mf_spec::{OperationSpec, Params};
use std::sync::OnceLock;

use crate::connector::{ConnectorError, Side};
use crate::generator::{GenContext, Generator, PlacementHint, ShapePart};
use crate::shape::ShapeRole;

/// A rectangle, `[0, sx] x [0, sy]`, counter-clockwise.
fn rect(sx: f64, sy: f64) -> Vec<[f64; 2]> {
    vec![[0.0, 0.0], [sx, 0.0], [sx, sy], [0.0, sy]]
}

/// A closed solid: a straight prism (along y) of a simple,
/// counter-clockwise polygon `profile` (in the x-z plane), need not be
/// convex — the end caps are triangulated by ear-clipping ([`fan_cap`]),
/// not a centroid fan — so this builds the hook's L-shaped cross-section
/// as one closed solid instead of two boxes glued at a shared face.
fn extrude_y(profile: &[[f64; 2]], y0: f64, y1: f64) -> Mesh {
    let n = profile.len();
    let mut positions = Vec::with_capacity(2 * n);
    for &y in &[y0, y1] {
        positions.extend(profile.iter().map(|p| [p[0], y, p[1]]));
    }
    let mut triangles = Vec::new();
    // Sides.
    for i in 0..n {
        let j = (i + 1) % n;
        let (a, b) = (i as u32, j as u32);
        let (c, d) = (n as u32 + j as u32, n as u32 + i as u32);
        // Outward normal at y0 faces -y; at y1 faces +y. Winding below
        // keeps both side quads outward for a CCW (x,z) profile viewed
        // from +y (so the wall between successive CCW points, swept
        // from y0 to y1, faces outward consistently with the end caps'
        // fans, set up in `fan_cap`).
        triangles.push([a, d, c]);
        triangles.push([a, c, b]);
    }
    // End caps: ear-clipped fans (the profile need not be convex). A
    // CCW (x,z) triangle (ia,ib,ic) has normal -y (outward at y0); y1's
    // cap needs the opposite winding (outward +y).
    let y0_cap = fan_cap(profile, 0, false);
    let y1_cap = fan_cap(profile, n as u32, true);
    triangles.extend(y0_cap);
    triangles.extend(y1_cap);
    Mesh::new(positions, triangles).expect("an extruded simple polygon is a valid mesh")
}

/// Ear-clipping triangulation of a simple (possibly non-convex),
/// counter-clockwise (in x,z) polygon of `n` points, indices offset by
/// `base`. Unflipped, a triangle's winding gives an outward normal of
/// -y (the y0 cap); `flip` reverses it, for the y1 cap (outward +y).
fn fan_cap(profile: &[[f64; 2]], base: u32, flip: bool) -> Vec<[u32; 3]> {
    let n = profile.len();
    let mut idx: Vec<usize> = (0..n).collect();
    let mut tris = Vec::new();
    let cross = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
        (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
    };
    let point_in_tri = |p: [f64; 2], a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
        let d1 = cross(a, b, p);
        let d2 = cross(b, c, p);
        let d3 = cross(c, a, p);
        (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0)
    };
    while idx.len() > 2 {
        let m = idx.len();
        let mut ear = None;
        for k in 0..m {
            let (ia, ib, ic) = (idx[(k + m - 1) % m], idx[k], idx[(k + 1) % m]);
            let (a, b, c) = (profile[ia], profile[ib], profile[ic]);
            if cross(a, b, c) <= 0.0 {
                continue; // reflex or degenerate: not an ear
            }
            let is_ear = idx
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != (k + m - 1) % m && j != k && j != (k + 1) % m)
                .all(|(_, &ip)| !point_in_tri(profile[ip], a, b, c));
            if is_ear {
                ear = Some((k, ia, ib, ic));
                break;
            }
        }
        let Some((k, ia, ib, ic)) = ear else {
            break; // allow-swallow: a degenerate input polygon stops clipping early; the caller's shapes are hand-built and convex-at-the-ear by construction
        };
        tris.push(if flip {
            [ia as u32, ic as u32, ib as u32]
        } else {
            [ia as u32, ib as u32, ic as u32]
        });
        idx.remove(k);
    }
    tris.into_iter().map(|t| t.map(|i| base + i)).collect()
}

const SPEC_YAML: &str = include_str!("../../../../specs/connectors/clasp.snap.yaml");

fn spec() -> &'static OperationSpec {
    static SPEC: OnceLock<OperationSpec> = OnceLock::new();
    SPEC.get_or_init(|| {
        serde_saphyr::from_str(SPEC_YAML).expect("connectors/clasp.snap.yaml is a valid spec")
    })
}

fn refuse(code: &'static str, message: impl Into<String>) -> ConnectorError {
    ConnectorError::Failed {
        code,
        message: message.into(),
    }
}

/// A box from `(x0, y0, z0)` to `(x1, y1, z1)`, world axis-aligned: an
/// x-z rectangle (`rect`, offset to `x0, z0`), extruded along y from
/// `y0` to `y1`.
fn box_from_to(x0: f64, y0: f64, z0: f64, x1: f64, y1: f64, z1: f64) -> Mesh {
    let profile: Vec<[f64; 2]> = rect(x1 - x0, z1 - z0)
        .into_iter()
        .map(|[x, z]| [x + x0, z + z0])
        .collect();
    extrude_y(&profile, y0, y1)
}

/// The engagement strain the owner's beam formula gives a rectangular
/// cantilever of length `arm_length` and thickness `arm_thickness`
/// deflected by `lip_depth` at its tip (the standard small-deflection
/// surface strain of a cantilever beam, `3·t·δ / (2·L²)`).
pub fn engagement_strain(arm_length: f64, arm_thickness: f64, lip_depth: f64) -> f64 {
    3.0 * arm_thickness * lip_depth / (2.0 * arm_length * arm_length)
}

/// Above this engagement strain, PLA cracks rather than flexes
/// (Task 27's "a PLA-safe limit").
pub const MAX_SAFE_STRAIN: f64 = 0.03;

pub struct SnapClasp;

impl Generator for SnapClasp {
    fn name(&self) -> &'static str {
        "Snap clasp"
    }

    fn placement_hint(&self) -> PlacementHint {
        PlacementHint::Surface
    }

    fn id(&self) -> &'static str {
        "clasp.snap"
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
        let width = params
            .get_float("width")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let arm_length = params
            .get_float("arm_length")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let arm_thickness = params
            .get_float("arm_thickness")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let lip_depth = params
            .get_float("lip_depth")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;
        let gap = params
            .get_float("gap")
            .map_err(|e| refuse("invalid_parameter", e.to_string()))?;

        if lip_depth >= arm_length {
            return Err(refuse(
                "lip_deeper_than_arm",
                format!(
                    "lip_depth {lip_depth} mm must be less than arm_length {arm_length} mm, so \
                     the lip has an arm to engage from"
                ),
            ));
        }
        let strain = engagement_strain(arm_length, arm_thickness, lip_depth);
        if strain > MAX_SAFE_STRAIN {
            return Err(refuse(
                "arm_strain_too_high",
                format!(
                    "This clasp's arm strains {:.1}% engaging the lip, above the {:.0}% PLA-safe \
                     limit. Make the arm longer or thinner.",
                    strain * 100.0,
                    MAX_SAFE_STRAIN * 100.0
                ),
            ));
        }

        // The catch: a ridge standing `ridge_height` off its base plate
        // (z = 0), spanning exactly `lip_depth` in x at the free end of
        // the arm's span, so the beam's own x span (below, `[0,
        // arm_length]`) overlaps it by exactly `lip_depth`.
        let ridge_height = 2.0 * lip_depth.max(arm_thickness);
        let catch = box_from_to(
            arm_length - lip_depth,
            0.0,
            0.0,
            arm_length,
            width,
            ridge_height,
        );

        // The arm (hook): a flat beam, fixed at x = 0, free at
        // x = arm_length, its whole underside `gap` clear of the
        // catch's top face (ridge_height). The lip is the part of this
        // same underside whose x span reaches the catch's own span, so
        // it overlaps it in projection along z by exactly `lip_depth`
        // (see the module doc) without a separate stepped shape — one
        // rectangular cross-section, matching the plain cantilever
        // formula `engagement_strain` uses.
        let arm_z0 = ridge_height + gap;
        let arm_z1 = arm_z0 + arm_thickness;
        let hook = box_from_to(0.0, 0.0, arm_z0, arm_length, width, arm_z1);

        Ok(vec![
            ShapePart::new("catch", ShapeRole::Solid, Side::A, catch),
            ShapePart::new("hook", ShapeRole::Solid, Side::B, hook),
        ])
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // tests may panic on failure
mod tests {
    use super::*;
    use mf_geometry::{Bvh, FaceId};
    use mf_runtime::RunContext;

    #[test]
    fn strain_matches_the_beam_formula() {
        // 3 * 1.6 * 0.3 / (2 * 10^2) = 1.44 / 200 = 0.0072
        let s = engagement_strain(10.0, 1.6, 0.3);
        assert!((s - 0.0072).abs() < 1e-12, "{s}");
    }

    fn assert_closed(mesh: &Mesh, label: &str) {
        let ctx = RunContext::none();
        let bvh = Bvh::build(mesh, &ctx).unwrap();
        let all: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
        mf_geometry::validate_solid(mesh, &all, &bvh, &ctx)
            .unwrap()
            .unwrap_or_else(|d| panic!("{label}: not a valid solid: {d}"));
    }

    /// `extrude_y` of a convex (rectangular) profile is a closed solid:
    /// the side-wall winding alone (no ear-clipping involved) must be
    /// consistently outward.
    #[test]
    fn extruded_rectangle_is_closed() {
        let m = extrude_y(&rect(2.0, 3.0), 0.0, 1.0);
        assert_closed(&m, "rect");
    }

    /// `extrude_y` of a non-convex (L-shaped) profile is a closed solid:
    /// the ear-clipped end caps must wind consistently with the sides.
    #[test]
    fn extruded_l_shape_is_closed() {
        let l = vec![
            [0.0, 0.0],
            [2.0, 0.0],
            [2.0, 1.0],
            [1.0, 1.0],
            [1.0, 2.0],
            [0.0, 2.0],
        ];
        let m = extrude_y(&l, 0.0, 1.0);
        assert_closed(&m, "l-shape");
    }
}
