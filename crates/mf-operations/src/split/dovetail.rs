//! The dovetail joint for `mesh.split_plane` (`pegs: dovetail`, Phase 1
//! Task 14, D12). One per section region, along its principal axis
//! through its pole of inaccessibility:
//! - a **groove** is cut into the bottom part: a trapezoid `width` wide at
//!   the cut, widening by `tan 15°` per side with depth, `depth` deep,
//!   running right through the part along the axis, so the top part
//!   slides in from the side;
//! - a **tail** of the same section, less `peg_clearance` across its
//!   flanks (both sides together) and `length_allowance` short of the
//!   groove's floor, is joined onto the top part, ending a wall short of
//!   the section's outline.
//!
//! The angle is fixed at 15° in v1 and recorded in the report.

use mf_boolean::SectionRegion;
use mf_geometry::Mesh;

use super::pegs::{prism_at, region_distance};

/// The flank angle, from the plane's normal.
pub const ANGLE_DEGREES: f64 = 15.0;

/// The groove's (or tail's) section in (along the normal, across), with
/// `x` from `x0` (deepest, below the cut) to `x1`: `width` across at
/// `x = 0`, wider below it.
fn trapezoid(width: f64, x0: f64, x1: f64) -> Vec<[f64; 2]> {
    let t = ANGLE_DEGREES.to_radians().tan();
    let w = |x: f64| width - 2.0 * x * t;
    vec![
        [x0, -w(x0) / 2.0],
        [x1, -w(x1) / 2.0],
        [x1, w(x1) / 2.0],
        [x0, w(x0) / 2.0],
    ]
}

/// How far along `axis` from `c` the tail can run (both ways) with
/// `reach` of the section around it.
pub fn tail_half_length(
    region: &SectionRegion,
    c: [f64; 2],
    axis: [f64; 2],
    reach: f64,
    span: f64,
) -> f64 {
    let fits = |t: f64| {
        [-t, t]
            .iter()
            .all(|&s| region_distance(region, [c[0] + s * axis[0], c[1] + s * axis[1]]) >= reach)
    };
    (0..=128)
        .map(|i| span * (1.0 - i as f64 / 128.0))
        .find(|&t| fits(t))
        .unwrap_or(0.0) // allow-swallow: 0 means no tail fits; the caller refuses
}

/// The groove (bottom part) and the tail (top part), in world space.
/// `base` is on the cut, `n` the unit normal towards the top part, `along`
/// the joint's direction (world, in the plane).
#[allow(clippy::too_many_arguments)]
pub fn groove_and_tail(
    base_below: [f64; 3],
    base_above: [f64; 3],
    n: [f64; 3],
    along: [f64; 3],
    width: f64,
    depth: f64,
    clearance: f64,
    allowance: f64,
    groove_half: f64,
    tail_half: f64,
) -> (Mesh, Mesh) {
    // Profiles in (u = n, v = along × n... ): prism_at's u is the key (n),
    // its extrusion axis w the joint's direction.
    let over = width / 2.0;
    let groove = prism_at(
        base_below,
        along,
        n,
        &trapezoid(width, -depth, over),
        -groove_half,
        groove_half,
    );
    let tail = prism_at(
        base_above,
        along,
        n,
        &trapezoid(width - clearance, -(depth - allowance), over.min(depth)),
        -tail_half,
        tail_half,
    );
    (groove, tail)
}
