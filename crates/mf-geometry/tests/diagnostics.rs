use std::sync::Arc;

use mf_fixtures::{
    cube, cube_with_missing_face, flipped_tetrahedron, merge, translated, uv_sphere,
};
use mf_geometry::{Mesh, inspect};

#[test]
fn cube_closed() {
    let d = inspect(&cube(1.0));
    assert_eq!(d.triangle_count, 12);
    assert_eq!(d.vertex_count, 8);
    assert_eq!(d.components, 1);
    assert_eq!(d.boundary_edges, 0);
    assert!(d.boundary_loops.is_empty());
    assert!(d.is_closed && d.is_manifold);
    assert!((d.surface_area - 6.0).abs() < 1e-12);
    assert!((d.signed_volume.unwrap() - 1.0).abs() < 1e-12);
    assert_eq!(d.inconsistent_orientation_edges, 0);
}

#[test]
fn sphere_closed_positive_volume() {
    let d = inspect(&uv_sphere(16, 32));
    assert!(d.is_closed && d.is_manifold);
    assert!(d.signed_volume.unwrap() > 4.0); // ~4.19 for r=1
}

#[test]
fn missing_face() {
    let d = inspect(&cube_with_missing_face());
    assert_eq!(d.boundary_loops.len(), 1);
    assert_eq!(d.boundary_loops[0].edge_count, 4);
    assert!((d.boundary_loops[0].perimeter - 4.0).abs() < 1e-12);
    assert_eq!(d.boundary_loops[0].centroid, [0.5, 0.5, 1.0]);
    assert_eq!(d.boundary_edges, 4);
    assert!(!d.is_closed);
    assert!(d.signed_volume.is_none());
}

#[test]
fn two_cubes() {
    let m = merge(&[cube(1.0), translated(&cube(1.0), [3.0, 0.0, 0.0])]);
    let topo = m.topology();
    assert_eq!(topo.components.len(), 2);
    assert!(topo.components.iter().all(|c| c.len() == 12));
    assert_eq!(topo.components[0][0], 0);
}

#[test]
fn components_largest_first() {
    let m = merge(&[
        mf_fixtures::tetrahedron(),
        translated(&cube(1.0), [3.0, 0.0, 0.0]),
    ]);
    let topo = m.topology();
    assert_eq!(topo.components[0].len(), 12);
    assert_eq!(topo.components[1].len(), 4);
}

#[test]
fn flipped() {
    let d = inspect(&flipped_tetrahedron());
    assert_eq!(d.inconsistent_orientation_edges, 3);
    assert_eq!(d.components, 1);
}

#[test]
fn fin() {
    // Three triangles sharing edge (0,1).
    let m = Mesh::new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.5, 1.0, 0.0],
            [0.5, -1.0, 0.0],
            [0.5, 0.0, 1.0],
        ],
        vec![[0, 1, 2], [1, 0, 3], [0, 1, 4]],
    )
    .unwrap();
    let d = inspect(&m);
    assert_eq!(d.non_manifold_edges, 1);
    assert!(!d.is_manifold);
}

#[test]
fn degenerate_and_duplicates() {
    let m = Mesh::new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
        vec![[0, 1, 2], [0, 1, 3], [0, 1, 4]],
    )
    .unwrap();
    let d = inspect(&m);
    assert_eq!(d.degenerate_triangles, 1);
    assert_eq!(d.duplicate_vertices_exact, 1);
}

#[test]
fn topology_cached() {
    let m = cube(1.0);
    assert!(Arc::ptr_eq(&m.topology(), &m.topology()));
    assert!(Arc::ptr_eq(&m.topology(), &m.clone().topology()));
}

#[test]
fn empty_mesh() {
    let d = inspect(&Mesh::empty());
    assert_eq!(d.triangle_count, 0);
    assert_eq!(d.components, 0);
    assert!(d.bounding_box.is_none());
    assert!(d.boundary_loops.is_empty());
}

#[test]
fn diagnostics_serialize() {
    let v = serde_json::to_value(inspect(&cube_with_missing_face())).unwrap();
    assert_eq!(v["boundary_loops"][0]["edge_count"], 4);
}
