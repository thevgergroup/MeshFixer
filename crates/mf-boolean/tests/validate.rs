#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `validate_solid` (D4, minimal Task 7 form): closed, edge-manifold,
//! consistently oriented, no degenerate faces and no self-intersections,
//! decided with exact predicates.

use mf_boolean::{
    KernelMesh, KernelResult, SolidError, self_intersecting_faces, validate_boolean, validate_solid,
};
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

// ---------------------------------------------------------------------------
// Task 7b: a boolean result is checked for *new* self-intersections (D4).
// ---------------------------------------------------------------------------

fn operand(m: &Mesh, op: u8) -> KernelMesh {
    let (p, t) = m.to_arrays();
    KernelMesh::labelled(p, t, op)
}

/// A "result" made of whole operands, each face labelled by its source.
fn result_of(parts: &[&KernelMesh]) -> KernelResult {
    let mut r = KernelResult::default();
    for m in parts {
        let base = r.positions.len() as u32;
        r.positions.extend(&m.positions);
        r.triangles
            .extend(m.triangles.iter().map(|t| t.map(|v| v + base)));
        r.origin.extend(&m.face_ids);
    }
    r
}

#[test]
fn an_operands_own_self_intersection_is_not_new() {
    // Operand a intersects itself: two overlapping cubes in one mesh.
    let a = operand(
        &merge(&[cube(1.0), translated(&cube(1.0), [0.5, 0.25, 0.125])]),
        0,
    );
    let b = operand(&translated(&cube(1.0), [5.0, 0.0, 0.0]), 1);
    let r = result_of(&[&a, &b]);
    assert!(matches!(
        validate_solid(&r.positions, &r.triangles, &RunContext::none()),
        Err(SolidError::SelfIntersecting { .. })
    ));
    validate_boolean(&r, [&a, &b], &RunContext::none()).unwrap();
}

#[test]
fn a_crossing_away_from_the_inputs_own_crossing_is_new() {
    // Task 7b review C1: operand a already intersects itself (two
    // overlapping cubes). The "result" carries the same labels, but the
    // second cube has moved: its faces no longer lie on the input faces
    // they name, so where they cross the first cube is not the input's own
    // crossing, even though the labelled input faces did cross somewhere.
    let a = operand(
        &merge(&[cube(1.0), translated(&cube(1.0), [0.5, 0.25, 0.125])]),
        0,
    );
    let b = operand(&translated(&cube(1.0), [5.0, 0.0, 0.0]), 1);
    let mut r = result_of(&[&a, &b]);
    for p in &mut r.positions[8..16] {
        p[0] += 0.0625;
    }
    assert!(matches!(
        validate_boolean(&r, [&a, &b], &RunContext::none()),
        Err(SolidError::SelfIntersecting { .. })
    ));
}

#[test]
fn a_piece_of_an_input_face_crossing_where_the_input_crossed_is_excused() {
    // The excused pair must lie on its input faces: split one face of the
    // second cube into two pieces (both on the input face), and the
    // pre-existing crossing stays excused.
    let a = operand(
        &merge(&[cube(1.0), translated(&cube(1.0), [0.5, 0.25, 0.125])]),
        0,
    );
    let b = operand(&translated(&cube(1.0), [5.0, 0.0, 0.0]), 1);
    let mut r = result_of(&[&a, &b]);
    // Subdivide every face of a's second cube into 4 at its edge
    // midpoints (exact: the cube's coordinates are dyadic), each piece
    // keeping its face's label; neighbours share the midpoints.
    let mut mids = std::collections::HashMap::new();
    let mut triangles = Vec::new();
    let mut origin = Vec::new();
    for (t, o) in r.triangles.iter().zip(&r.origin) {
        if o.0 != 0 || o.1 < 12 {
            triangles.push(*t);
            origin.push(*o);
            continue;
        }
        let mut m = |u: u32, v: u32| {
            *mids.entry((u.min(v), u.max(v))).or_insert_with(|| {
                let (p, q) = (r.positions[u as usize], r.positions[v as usize]);
                r.positions.push([0, 1, 2].map(|k| (p[k] + q[k]) / 2.0));
                (r.positions.len() - 1) as u32
            })
        };
        let [x, y, z] = *t;
        let (mxy, myz, mzx) = (m(x, y), m(y, z), m(z, x));
        for piece in [[x, mxy, mzx], [mxy, y, myz], [mzx, myz, z], [mxy, myz, mzx]] {
            triangles.push(piece);
            origin.push(*o);
        }
    }
    r.triangles = triangles;
    r.origin = origin;
    validate_boolean(&r, [&a, &b], &RunContext::none()).unwrap();
}

#[test]
fn faces_of_the_two_operands_intersecting_are_new() {
    let a = operand(&cube(1.0), 0);
    let b = operand(&translated(&cube(1.0), [0.5, 0.25, 0.125]), 1);
    match validate_boolean(&result_of(&[&a, &b]), [&a, &b], &RunContext::none()) {
        Err(SolidError::SelfIntersecting { faces, pairs }) => {
            assert!(faces[0] < 12 && faces[1] >= 12, "{faces:?}");
            assert!(pairs >= 4, "{pairs}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn faces_of_one_operand_that_did_not_intersect_in_the_input_are_new() {
    // a is two disjoint cubes; the "result" moves the second into the first.
    let a = operand(
        &merge(&[cube(1.0), translated(&cube(1.0), [3.0, 0.0, 0.0])]),
        0,
    );
    let b = operand(&translated(&cube(1.0), [9.0, 0.0, 0.0]), 1);
    let mut r = result_of(&[&a]);
    for p in &mut r.positions[8..16] {
        p[0] -= 2.5;
    }
    assert!(matches!(
        validate_boolean(&r, [&a, &b], &RunContext::none()),
        Err(SolidError::SelfIntersecting { .. })
    ));
}

#[test]
fn a_boolean_result_is_still_checked_for_everything_else() {
    let a = operand(&cube_with_missing_face(), 0);
    let b = operand(&translated(&cube(1.0), [5.0, 0.0, 0.0]), 1);
    assert!(matches!(
        validate_boolean(&result_of(&[&a, &b]), [&a, &b], &RunContext::none()),
        Err(SolidError::NotClosed { .. })
    ));
}

#[test]
fn self_intersecting_faces_are_flagged() {
    let (p, t) = merge(&[cube(1.0), translated(&cube(1.0), [0.5, 0.25, 0.125])]).to_arrays();
    let flags = self_intersecting_faces(&p, &t, &RunContext::none()).unwrap();
    assert_eq!(flags.len(), 24);
    assert!(flags.iter().any(|&f| f));
    // The faces away from the overlap (the first cube's -x, -y, -z sides
    // and the second's +x, +y, +z sides) don't intersect anything.
    for f in [0, 1, 4, 5, 10, 11, 14, 15, 20, 21, 18, 19] {
        assert!(!flags[f], "face {f}");
    }
    let (p, t) = cube(1.0).to_arrays();
    assert!(
        self_intersecting_faces(&p, &t, &RunContext::none())
            .unwrap()
            .iter()
            .all(|&f| !f)
    );
}
