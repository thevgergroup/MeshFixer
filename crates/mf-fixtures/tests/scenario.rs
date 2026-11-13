#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `mf_fixtures::scenario` (Phase 1 Task 24a, D24): the octopus, head and
//! hollow box used by the UC2 "Rocktopus" acceptance scenario are seeded,
//! closed and manifold, and the octopus has a neck narrower than the
//! body above it and the tentacle spread below it.

use mf_fixtures::scenario::{head, hollow_box, octopus};
use mf_geometry::{Bvh, FaceId, Mesh};
use mf_runtime::RunContext;

fn assert_closed_solid(mesh: &Mesh, label: &str) {
    let ctx = RunContext::none();
    let bvh = Bvh::build(mesh, &ctx).unwrap();
    let all: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
    mf_geometry::validate_solid(mesh, &all, &bvh, &ctx)
        .unwrap()
        .unwrap_or_else(|d| panic!("{label}: not a valid solid: {d}"));
}

#[test]
fn octopus_is_seeded_closed_and_manifold() {
    let a = octopus(1);
    let b = octopus(1);
    assert_eq!(a.to_arrays(), b.to_arrays(), "same seed, same mesh");
    assert_closed_solid(&a, "octopus");

    let c = octopus(2);
    assert_ne!(a.to_arrays().0, c.to_arrays().0, "different seeds differ");
}

#[test]
fn octopus_neck_is_narrower_than_the_body_and_the_tentacle_spread() {
    let mesh = octopus(1);
    // Radius of every vertex (distance from the z axis) in horizontal
    // bands, from the body's top to the tentacles' bottom.
    let band_radius = |z_lo: f64, z_hi: f64| -> f64 {
        let (positions, _) = mesh.to_arrays();
        let mut max_r = 0.0f64;
        for p in &positions {
            if p[2] >= z_lo && p[2] < z_hi {
                let r = (p[0] * p[0] + p[1] * p[1]).sqrt();
                max_r = max_r.max(r);
            }
        }
        max_r
    };
    let body = band_radius(8.0, 20.0);
    let neck = band_radius(-2.0, 2.0);
    let tentacles = band_radius(-20.0, -8.0);
    assert!(
        neck < body && neck < tentacles,
        "neck radius {neck} should be narrower than body {body} and tentacle spread {tentacles}"
    );
}

#[test]
fn head_is_seeded_closed_and_manifold() {
    let a = head(1);
    let b = head(1);
    assert_eq!(a.to_arrays(), b.to_arrays(), "same seed, same mesh");
    assert_closed_solid(&a, "head");
}

#[test]
fn hollow_box_is_closed_manifold_and_actually_hollow() {
    let mesh = hollow_box(2.0);
    assert_closed_solid(&mesh, "hollow_box");
    // A hollow box has far less volume than its bounding box (outer shell
    // minus the open cavity), but still positive (it's a valid, if
    // concave, solid: the shell itself encloses volume).
    let volume = mf_geometry::signed_volume(&mesh) / 6.0;
    let outer_bbox = 40.0 * 30.0 * 20.0;
    assert!(volume > 0.0, "the shell encloses positive volume");
    assert!(
        volume < outer_bbox * 0.6,
        "hollowed out: {volume} should be well under the outer bbox {outer_bbox}"
    );
}
