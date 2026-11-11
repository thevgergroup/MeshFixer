//! Built-in connectors (Phase 1 Tasks 25-28, MF-MODEL-005): generated in
//! code like the parts library's built-ins (D16), registered in
//! [`crate::connectors()`]. The plugin host adds to the same registry
//! (MF-PLUG-001).

pub mod clasp;
pub mod flap;
pub mod hinge;
pub mod joint;
pub mod thread;

use mf_geometry::Mesh;

use crate::connector::Connector;

/// Every built-in connector. The plugin host appends to a copy of this
/// (or its own registry chained after it); nothing here assumes it's the
/// only source.
pub fn connectors() -> Vec<&'static dyn Connector> {
    vec![
        &hinge::PinHinge,
        &clasp::SnapClasp,
        &flap::FlapHinge,
        &thread::Thread,
        &joint::Dowel,
        &joint::Peg,
        &joint::Dovetail,
        &joint::Magnet,
        &joint::Insert,
    ]
}

/// A closed surface of revolution around the x axis: `profile` is
/// `(x, r)` from one end to the other. A point with `r == 0` is a pole
/// (one vertex, closing that end); the result is closed only when both
/// ends are poles or the caller accepts open rims at the ends that
/// aren't. Mirrors `builtin::revolve` (around z, for parts attached to a
/// surface) but around x, the axis a hinge or clasp's own mechanism
/// turns about.
pub(crate) fn revolve_x(profile: &[(f64, f64)], segments: u32) -> Mesh {
    let mut positions = Vec::new();
    let mut rows: Vec<(u32, bool)> = Vec::new();
    for &(x, r) in profile {
        let start = positions.len() as u32;
        if r == 0.0 {
            positions.push([x, 0.0, 0.0]);
            rows.push((start, true));
        } else {
            for s in 0..segments {
                let a = std::f64::consts::TAU * s as f64 / segments as f64;
                positions.push([x, r * a.cos(), r * a.sin()]);
            }
            rows.push((start, false));
        }
    }
    let mut triangles = Vec::new();
    for k in 0..rows.len() - 1 {
        let ((a, pa), (b, pb)) = (rows[k], rows[k + 1]);
        for i in 0..segments {
            let j = (i + 1) % segments;
            match (pa, pb) {
                (false, false) => {
                    triangles.push([a + i, a + j, b + j]);
                    triangles.push([a + i, b + j, b + i]);
                }
                (true, false) => triangles.push([a, b + j, b + i]),
                (false, true) => triangles.push([a + i, a + j, b]),
                (true, true) => {}
            }
        }
    }
    Mesh::new(positions, triangles).expect("a revolved profile is a valid mesh")
}

/// A cylinder's wall, from `z0` to `z1`, radius `r`, around the x axis
/// (so the hinge's own axis is x; a part is placed on a surface by its
/// `Attach`, like any other part). `segments` points around the ring.
/// Shared by every connector that builds round features.
pub(crate) fn cylinder_wall(
    r: f64,
    x0: f64,
    x1: f64,
    segments: u32,
    positions: &mut Vec<[f64; 3]>,
    triangles: &mut Vec<[u32; 3]>,
) {
    let mut ring = |x: f64| -> u32 {
        let start = positions.len() as u32;
        for s in 0..segments {
            let a = std::f64::consts::TAU * s as f64 / segments as f64;
            positions.push([x, r * a.cos(), r * a.sin()]);
        }
        start
    };
    let a = ring(x0);
    let b = ring(x1);
    for i in 0..segments {
        let j = (i + 1) % segments;
        triangles.push([a + i, a + j, b + j]);
        triangles.push([a + i, b + j, b + i]);
    }
}
