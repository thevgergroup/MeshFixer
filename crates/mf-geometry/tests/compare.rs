#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Identity vs equivalence (MF-DOC-003).

use mf_fixtures::{translated, uv_sphere};
use mf_geometry::{Mesh, equivalent};

fn reversed_order(m: &Mesh) -> Mesh {
    let (positions, triangles) = m.to_arrays();
    let n = positions.len() as u32;
    let positions: Vec<_> = positions.into_iter().rev().collect();
    let triangles: Vec<_> = triangles
        .into_iter()
        .rev()
        .map(|t| t.map(|v| n - 1 - v))
        .collect();
    Mesh::new(positions, triangles).unwrap()
}

#[test]
fn reordered_mesh_not_identical_but_equivalent() {
    let a = uv_sphere(16, 32);
    let b = reversed_order(&a);
    assert_ne!(a.content_hash(), b.content_hash());
    let e = equivalent(&a, &b, 0.0);
    assert!(e.invariants_match && e.within, "{e:?}");
    assert_eq!(e.max_vertex_distance, Some(0.0));
}

#[test]
fn perturbed_equivalent_at_1e6_not_1e12() {
    let a = uv_sphere(16, 32);
    let b = translated(&a, [1e-9, 0.0, 0.0]);
    let loose = equivalent(&a, &b, 1e-6);
    assert!(loose.within, "{loose:?}");
    assert!((loose.max_vertex_distance.unwrap() - 1e-9).abs() < 1e-12);
    let strict = equivalent(&a, &b, 1e-12);
    assert!(strict.invariants_match && !strict.within, "{strict:?}");
    assert_eq!(strict.max_vertex_distance, None, "beyond tolerance");
}

#[test]
fn different_topology_is_not_equivalent() {
    let a = uv_sphere(16, 32);
    let mut e = a.edit();
    e.remove_face(mf_geometry::FaceId(0)).unwrap();
    let (b, _) = e.finish().unwrap();
    let r = equivalent(&a, &b, 1.0);
    assert!(!r.invariants_match && !r.within);
}

#[test]
fn flipped_winding_is_not_equivalent() {
    let a = mf_fixtures::cube(1.0);
    let mut e = a.edit();
    let t = a.face(mf_geometry::FaceId(0)).unwrap();
    e.set_face(mf_geometry::FaceId(0), [t[0], t[2], t[1]])
        .unwrap();
    let (b, _) = e.finish().unwrap();
    let r = equivalent(&a, &b, 1.0);
    assert!(!r.invariants_match && !r.within, "{r:?}");
}
