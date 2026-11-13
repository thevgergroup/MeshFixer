//! Built-in connectors (Phase 1 Tasks 25-28, MF-MODEL-005): generated in
//! code like the parts library's built-ins (D16), registered in
//! the one generator registry ([`crate::generators`]). The plugin host adds to the same registry
//! (MF-PLUG-001).

pub mod clasp;
pub mod dovetail_slide;
pub mod flap;
pub mod hinge;
pub mod hole_profile;
pub mod joint;
pub mod leaf_hinge;
pub mod thread;

use mf_geometry::Mesh;

/// Every built-in connector, in registry order (the library lists the
/// ones offered on a surface in this order; Connect its cut-face ones).
/// Adding one is one entry here, next to its own generator file and spec.
/// A test-only one (the `test-kinds` feature) is registered with its
/// module inline, `#[path]` naming its file, so it too is one entry.
pub(crate) fn builtin_generators() -> Vec<&'static dyn crate::Generator> {
    #[allow(unused_mut, clippy::let_and_return)] // only the test-kinds entries push
    let mut all: Vec<&'static dyn crate::Generator> = vec![
        &hinge::PinHinge,
        &leaf_hinge::LeafHinge,
        &flap::FlapHinge,
        &clasp::SnapClasp,
        &thread::Thread,
        &dovetail_slide::DovetailSlide,
        &joint::Dowel,
        &joint::Peg,
        &joint::Dovetail,
        &joint::Magnet,
        &joint::Insert,
    ];
    all
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
