#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `validate_solid` (D4, minimal Task 7 form): closed, edge-manifold,
//! consistently oriented, no degenerate faces and no self-intersections,
//! decided with exact predicates.

use mf_boolean::{SolidError, validate_solid};
use mf_fixtures::{
    cube, cube_with_missing_face, flipped_tetrahedron, merge, translated, uv_sphere,
};
use mf_geometry::Mesh;
use mf_runtime::RunContext;

fn check(m: &Mesh) -> Result<(), SolidError> {
    let (p, t) = m.to_arrays();
    validate_solid(&p, &t, &RunContext::none())
}

#[test]
fn closed_solids_pass() {
    check(&cube(1.0)).unwrap();
    check(&uv_sphere(16, 32)).unwrap();
    check(&mf_fixtures::tetrahedron()).unwrap();
    // Two disjoint solids are one valid (multi-shell) solid.
    check(&merge(&[
        cube(1.0),
        translated(&cube(1.0), [3.0, 0.0, 0.0]),
    ]))
    .unwrap();
}

#[test]
fn an_empty_mesh_is_not_a_solid() {
    assert_eq!(
        validate_solid(&[], &[], &RunContext::none()),
        Err(SolidError::Empty)
    );
}

#[test]
fn a_hole_is_refused_as_not_closed() {
    match check(&cube_with_missing_face()) {
        Err(SolidError::NotClosed { boundary_edges, .. }) => assert_eq!(boundary_edges, 4),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_flipped_face_is_refused_as_inconsistently_oriented() {
    match check(&flipped_tetrahedron()) {
        Err(SolidError::InconsistentOrientation { edges, .. }) => assert_eq!(edges, 3),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_edge_on_four_faces_is_refused_as_not_manifold() {
    // Two cubes sharing the edge (1,1,z) by vertex index.
    let (mut p, mut t) = cube(1.0).to_arrays();
    let (q, u) = translated(&cube(1.0), [1.0, 1.0, 0.0]).to_arrays();
    let base = p.len() as u32;
    // Vertex 0 and 4 of the second cube are (1,1,0) and (1,1,1): reuse
    // the first cube's vertices 2 and 6.
    let remap = |v: u32| match v {
        0 => 2,
        4 => 6,
        v => v + base,
    };
    p.extend(q);
    t.extend(u.iter().map(|f| f.map(remap)));
    match validate_solid(&p, &t, &RunContext::none()) {
        Err(SolidError::NotManifold { edge, faces, .. }) => {
            assert_eq!(edge, [2, 6]);
            assert_eq!(faces, 4);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn overlapping_shells_are_refused_as_self_intersecting() {
    let m = merge(&[cube(1.0), translated(&cube(1.0), [0.5, 0.25, 0.125])]);
    match check(&m) {
        Err(SolidError::SelfIntersecting { faces, pairs }) => {
            assert!(faces[0] < 12 && faces[1] >= 12, "{faces:?}");
            assert!(pairs >= 4, "{pairs}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn shells_touching_face_to_face_are_refused() {
    // Coplanar contact with separate vertices: the solids share a wall.
    let m = merge(&[cube(1.0), translated(&cube(1.0), [1.0, 0.0, 0.0])]);
    assert!(matches!(
        check(&m),
        Err(SolidError::SelfIntersecting { .. })
    ));
    // Touching at a single corner point, too.
    let m = merge(&[cube(1.0), translated(&cube(1.0), [1.0, 1.0, 1.0])]);
    assert!(matches!(
        check(&m),
        Err(SolidError::SelfIntersecting { .. })
    ));
}

#[test]
fn a_shell_inside_another_is_not_an_intersection() {
    // Nested without contact: surfaces don't intersect (orientation of
    // the cavity isn't this check's business).
    let inner = translated(&mf_fixtures::scaled(&cube(1.0), 0.5), [0.25, 0.25, 0.25]);
    check(&merge(&[cube(1.0), inner])).unwrap();
}

#[test]
fn a_zero_area_face_is_refused_as_degenerate() {
    // Two opposite collinear triangles: closed, oriented and
    // edge-manifold, but bounding no volume.
    let p = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [2.0, 0.0, 0.0]];
    let t = vec![[0, 1, 2], [0, 2, 1]];
    match validate_solid(&p, &t, &RunContext::none()) {
        Err(SolidError::DegenerateFace { face, faces }) => {
            assert_eq!(face, 0);
            assert_eq!(faces, 2);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_index_out_of_range_is_refused() {
    let p = vec![[0.0, 0.0, 0.0]];
    let t = vec![[0, 1, 2]];
    assert_eq!(
        validate_solid(&p, &t, &RunContext::none()),
        Err(SolidError::IndexOutOfRange { face: 0 })
    );
}

#[test]
fn validation_is_cancellable() {
    let ctx = RunContext::none();
    ctx.cancel.cancel();
    let (p, t) = uv_sphere(64, 128).to_arrays();
    assert_eq!(validate_solid(&p, &t, &ctx), Err(SolidError::Cancelled));
}
