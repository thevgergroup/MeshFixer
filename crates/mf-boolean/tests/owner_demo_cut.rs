#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The plane cut on the owner's own demo scan (owner 2026-11-13, the
//! third Split refusal): `uv_sphere(96, 192)` scaled to radius 20, whose
//! equator ring sits at z = 20·cos(π/2) = 1.2e-15 — a whole ring of
//! vertices an ulp above the cut, each with two crossing edges whose cut
//! points round to within an ulp of it. Cut there with the plane's point
//! anywhere along the plane (the owner's default was x = 2.5, the
//! bounding-box centre of 'scan' and 'part' together): never refused,
//! and every capped part a closed solid with no two faces crossing.

use mf_boolean::{Plane, Side, cut_half_with};
use mf_runtime::RunContext;

/// The owner's file (`artifacts/phase1/demo/boolean-demo.3mf`, copied
/// byte for byte): a freshly generated scene differs from it in the last
/// bit of some coordinates, the level this bug lived at.
fn owner_scan() -> mf_geometry::Mesh {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/owner/boolean-demo.3mf"
    );
    let bytes = std::fs::read(path).unwrap();
    let model = mf_io::threemf::read(&bytes).unwrap().0;
    let scan = &model.objects[0];
    assert_eq!(scan.name, "scan");
    // Translation only (to z = 20): the world plane z = 20 is z = 0 here.
    assert_eq!(
        &scan.transform[..9],
        &[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
    );
    assert_eq!(scan.transform[11], 20.0);
    scan.mesh.clone()
}

#[test]
fn owner_scan_cut_at_its_equator_from_any_plane_point_is_never_refused() {
    let scan = owner_scan();
    for point in [
        [2.5, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        [-7.045, 18.718, 0.0],
        [123.456, -0.3, 0.0],
    ] {
        let plane = Plane {
            point,
            normal: [0.0, 0.0, 1.0],
        };
        for keep in [Side::Above, Side::Below] {
            for cap in [true, false] {
                let what = format!("{point:?} {keep:?} cap={cap}");
                let half = cut_half_with(&scan, &plane, keep, cap, &RunContext::none())
                    .unwrap_or_else(|e| panic!("{what}: {e}"));
                let (p, t) = half.mesh.to_arrays();
                if cap {
                    mf_boolean::validate_solid(&p, &t, &RunContext::none())
                        .unwrap_or_else(|e| panic!("{what}: not a solid: {e}"));
                } else {
                    // Open at the cut, along exactly one loop: the rim.
                    let topo = half.mesh.topology_with(&RunContext::none()).unwrap();
                    assert_eq!(topo.boundary_loops.len(), 1, "{what}");
                    assert!(topo.non_manifold_edges.is_empty(), "{what}");
                }
            }
        }
    }
}
