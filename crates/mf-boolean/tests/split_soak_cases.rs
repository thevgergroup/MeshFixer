#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Every cut the random soak (`mf-command/tests/split_robust.rs`, 2,200
//! scenes, ~33k cuts) found the custom plane cut refusing on valid closed
//! manifold input (split-harden-brief, 2026-11-13), saved as fixtures:
//! `fixtures/split-soak/<seed>.3mf` is the scene `mesh.combine` built
//! (one object, its transform), and the plane is the world plane the soak
//! cut it at. Each is cut on both sides, capped and uncapped: never
//! refused; capped parts closed, manifold and consistently oriented,
//! uncapped ones manifold and open only at the cut.

use std::path::Path;

use mf_boolean::{Plane, Side, cut_half_with};
use mf_geometry::Mesh;
use mf_runtime::RunContext;

/// `(seed, world plane point, world plane normal)`, as the soak logged
/// them (shortest round-trip decimals, so exact).
const CASES: &[(&str, [f64; 3], [f64; 3])] = &[
    (
        "1c851578372c7087",
        [-2.8046385802568423, 2.9703294367496396, -5.31491579260158],
        [-1.1184589771767293, 0.7187899391950887, -6.413176184483504],
    ),
    (
        "1e4c30ca52dc82f1",
        [-1.5, 1.0000000000000004, 6.401923788646684],
        [0.0, 0.0, 1.0],
    ),
    (
        "38efc80e06b1142d",
        [-0.869915128471654, 1.1384817010492787, 4.788924172750679],
        [-2.0, 1.0, 5.0],
    ),
    ("5ec724b6639c8f0b", [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
    ("c10d74d1bf1d0213", [-0.5, -2.0, 1.5], [0.0, 0.0, 1.0]),
    (
        "263425bb27b700c9",
        [5.995190528383289, 9.0, 3.750000000000001],
        [1.0, 2.0, 3.0],
    ),
    // Found by the soak after the section was rebuilt on topology
    // (split-harden): the first fan pairing and cap repair refused these.
    (
        "1926ead54f47f5bd",
        [3.0, 3.1038298234083785, 0.5],
        [1.0, 0.0, 0.0],
    ),
    (
        "bc83aa83d226ff05",
        [-2.962523224388176, 2.216919686113031, 7.629165124598852],
        [
            0.0000000000000002498001805406602,
            0.03518281605681696,
            0.07468182241865415,
        ],
    ),
];

/// The scene and the cut plane in its object space (`affine::apply` of
/// the inverse for the point, `normal_to_object` for the normal).
pub fn case(seed: &str, point: [f64; 3], normal: [f64; 3]) -> (Mesh, Plane) {
    let path = format!(
        "{}/../../fixtures/split-soak/{seed}.3mf",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes = std::fs::read(Path::new(&path)).unwrap();
    let model = mf_io::threemf::read(&bytes).unwrap().0;
    assert_eq!(model.objects.len(), 1);
    let o = &model.objects[0];
    let t = o.transform;
    // The one affine module (C22), exactly as `mesh.split_plane`'s plan
    // maps the plane, to the bit.
    let inv = mf_geometry::affine::inverse(&t).unwrap();
    let local = mf_geometry::affine::apply(&inv, point);
    let n = mf_geometry::affine::normal_to_object(&t, normal);
    (
        o.mesh.clone(),
        Plane {
            point: local,
            normal: n,
        },
    )
}

#[test]
fn every_soak_refusal_now_cuts_cleanly() {
    let ctx = RunContext::none();
    let mut failures = Vec::new();
    for &(seed, point, normal) in CASES {
        let (mesh, plane) = case(seed, point, normal);
        for keep in [Side::Above, Side::Below] {
            for cap in [true, false] {
                let what = format!("{seed} {keep:?} cap={cap}");
                let half = match cut_half_with(&mesh, &plane, keep, cap, &ctx) {
                    Ok(h) => h,
                    Err(mf_boolean::CutError::MissesObject { grazed: true }) => continue,
                    Err(e) => {
                        failures.push(format!("{what}: refused: {e}"));
                        continue;
                    }
                };
                let topo = half.mesh.topology_with(&ctx).unwrap();
                if (cap && topo.boundary_edges > 0)
                    || !topo.non_manifold_edges.is_empty()
                    || topo.inconsistent_edges > 0
                {
                    failures.push(format!(
                        "{what}: {} open, {} non-manifold, {} misoriented edges",
                        topo.boundary_edges,
                        topo.non_manifold_edges.len(),
                        topo.inconsistent_edges
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
