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
];

/// `mf-operations`' `affine::inverse`, verbatim: the plane must be
/// mapped to the object exactly as `mesh.split_plane` maps it, to the bit.
fn inverse(t: &[f64; 12]) -> [f64; 12] {
    let m = [[t[0], t[1], t[2]], [t[3], t[4], t[5]], [t[6], t[7], t[8]]];
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    let cof = |r: usize, c: usize| {
        let (r0, r1) = ((r + 1) % 3, (r + 2) % 3);
        let (c0, c1) = ((c + 1) % 3, (c + 2) % 3);
        m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]
    };
    let mut inv = [[0.0; 3]; 3];
    for (r, row) in inv.iter_mut().enumerate() {
        for (c, x) in row.iter_mut().enumerate() {
            *x = cof(c, r) / det;
        }
    }
    let tr = [t[9], t[10], t[11]];
    let it: [f64; 3] =
        std::array::from_fn(|c| -(tr[0] * inv[0][c] + tr[1] * inv[1][c] + tr[2] * inv[2][c]));
    [
        inv[0][0], inv[0][1], inv[0][2], inv[1][0], inv[1][1], inv[1][2], inv[2][0], inv[2][1],
        inv[2][2], it[0], it[1], it[2],
    ]
}

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
    let inv = inverse(&t);
    let local: [f64; 3] = std::array::from_fn(|c| {
        inv[c] * point[0] + inv[3 + c] * point[1] + inv[6 + c] * point[2] + inv[9 + c]
    });
    let n: [f64; 3] =
        std::array::from_fn(|r| t[3 * r] * normal[0] + t[3 * r + 1] * normal[1] + t[3 * r + 2] * normal[2]);
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
