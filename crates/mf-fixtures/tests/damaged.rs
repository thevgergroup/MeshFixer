#![allow(clippy::unwrap_used)] // tests may panic on failure

use mf_fixtures::{DamageSpec, damaged_sphere};
use mf_geometry::inspect;

#[test]
fn damaged_sphere_has_requested_defects() {
    let spec = DamageSpec {
        rings: 64,
        segments: 128,
        holes: 5,
        islands: 20,
        unwelded: 10,
        seed: 1,
    };
    let m = damaged_sphere(&spec);
    let d = inspect(&m);
    // Sphere + 20 islands; an unwelded corner on a hole rim can also split
    // off a single face, which is realistic damage.
    assert!(d.components >= 21, "components: {}", d.components);
    let islands = m
        .topology()
        .components
        .iter()
        .filter(|c| c.len() == 4)
        .count();
    assert_eq!(islands, 20);
    assert!(
        d.boundary_loops.len() >= 5,
        "loops: {}",
        d.boundary_loops.len()
    );
    assert!(d.duplicate_vertices_exact >= 10);
    assert!(!d.is_closed);
}

#[test]
fn damaged_sphere_is_deterministic() {
    let spec = DamageSpec {
        rings: 32,
        segments: 64,
        holes: 3,
        islands: 5,
        unwelded: 4,
        seed: 7,
    };
    assert_eq!(
        damaged_sphere(&spec).content_hash(),
        damaged_sphere(&spec).content_hash()
    );
    let other = DamageSpec { seed: 8, ..spec };
    assert_ne!(
        damaged_sphere(&spec).content_hash(),
        damaged_sphere(&other).content_hash()
    );
}

#[test]
fn damage_spec_for_triangle_target() {
    let spec = DamageSpec::for_triangles(1_000_000, 1);
    let tris = 2 * spec.segments as usize * (spec.rings as usize - 1);
    assert!((900_000..=1_100_000).contains(&tris), "{tris}");
}
