#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The degenerate post-pass (Phase 1 Task 7b): zero-area triangles the
//! kernel created are removed by exact, geometry-preserving edits (an edge
//! flip for a collinear "cap", an edge collapse for coincident vertices),
//! keeping provenance. What it can't remove stays, and D4 refuses it.

use mf_boolean::{
    KernelMesh, KernelResult, SolidError, remove_created_degenerates, validate_boolean,
    validate_solid,
};
use mf_fixtures::{cube, translated};
use mf_runtime::RunContext;

fn operands() -> (KernelMesh, KernelMesh) {
    let (p, t) = cube(1.0).to_arrays();
    let a = KernelMesh::labelled(p, t, 0);
    let (p, t) = translated(&cube(1.0), [5.0, 0.0, 0.0]).to_arrays();
    (a, KernelMesh::labelled(p, t, 1))
}

/// Sum of triangle areas per origin label, for coverage checks.
fn area_by_origin(r: &KernelResult) -> std::collections::BTreeMap<(u8, u32), f64> {
    let mut out = std::collections::BTreeMap::new();
    for (t, o) in r.triangles.iter().zip(&r.origin) {
        let [a, b, c] = t.map(|v| r.positions[v as usize]);
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        *out.entry(*o).or_insert(0.0) += 0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    }
    out
}

/// The unit cube with a collinear sliver along edge 0–1: vertex 8 at the
/// edge's midpoint splits the front face (0, 1, 5), and the sliver
/// (0, 1, 8) closes the surface against the bottom face (0, 2, 1).
fn cube_with_a_cap() -> KernelResult {
    let (a, _) = operands();
    let mut r = KernelResult {
        positions: a.positions.clone(),
        triangles: a.triangles.clone(),
        origin: a.face_ids.clone(),
    };
    r.positions.push([0.5, 0.0, 0.0]);
    // Face 4 is (0, 1, 5).
    assert_eq!(r.triangles[4], [0, 1, 5]);
    r.triangles[4] = [0, 8, 5];
    r.triangles.push([8, 1, 5]);
    r.origin.push((0, 4));
    // The sliver lies on the front face.
    r.triangles.push([0, 1, 8]);
    r.origin.push((0, 4));
    r
}

#[test]
fn a_collinear_sliver_is_flipped_away() {
    let (a, b) = operands();
    let mut r = cube_with_a_cap();
    assert!(matches!(
        validate_solid(&r.positions, &r.triangles, &RunContext::none()),
        Err(SolidError::DegenerateFace { faces: 1, .. })
    ));
    let before = area_by_origin(&r);
    let c = remove_created_degenerates(&mut r, [&a, &b], &RunContext::none()).unwrap();
    assert_eq!((c.flipped, c.collapsed, c.remaining), (1, 0, 0), "{c:?}");
    validate_boolean(&r, [&a, &b], &RunContext::none()).unwrap();
    assert_eq!(r.triangles.len(), 14);
    // The bottom face (0, 2, 1) is now two triangles through vertex 8,
    // still labelled as the bottom face; every face keeps its area.
    assert_eq!(r.origin.iter().filter(|&&o| o == (0, 0)).count(), 2);
    let after = area_by_origin(&r);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    for (k, v) in &before {
        assert!((v - after[k]).abs() <= 1e-15, "{k:?}: {v} vs {}", after[k]);
    }
}

#[test]
fn coincident_vertices_are_collapsed() {
    let (a, b) = operands();
    let mut r = KernelResult {
        positions: a.positions.clone(),
        triangles: a.triangles.clone(),
        origin: a.face_ids.clone(),
    };
    // Vertex 8 duplicates vertex 0; face 4 (0, 1, 5) becomes (8, 1, 5)
    // and two needles (0, 1, 8), (0, 8, 5) close the surface.
    r.positions.push([0.0, 0.0, 0.0]);
    r.triangles[4] = [8, 1, 5];
    r.triangles.push([0, 1, 8]);
    r.origin.push((0, 4));
    r.triangles.push([0, 8, 5]);
    r.origin.push((0, 4));
    assert!(matches!(
        validate_solid(&r.positions, &r.triangles, &RunContext::none()),
        Err(SolidError::DegenerateFace { faces: 2, .. })
    ));
    let c = remove_created_degenerates(&mut r, [&a, &b], &RunContext::none()).unwrap();
    assert_eq!((c.flipped, c.collapsed, c.remaining), (0, 1, 0), "{c:?}");
    validate_boolean(&r, [&a, &b], &RunContext::none()).unwrap();
    // Back to the cube, vertex for vertex.
    assert_eq!(r.positions, a.positions);
    let mut tris = r.triangles.clone();
    tris.sort();
    let mut want = a.triangles.clone();
    want.sort();
    assert_eq!(tris, want);
}

#[test]
fn a_degenerate_face_on_a_degenerate_input_face_is_left_for_the_validator() {
    // The input's own zero-area face is the input's defect: not ours to
    // edit away (outside a window it would be kept bitwise, D5).
    let (_, b) = operands();
    let mut r = cube_with_a_cap();
    let a = KernelMesh {
        positions: r.positions.clone(),
        triangles: r.triangles.clone(),
        face_ids: (0..r.triangles.len() as u32).map(|f| (0, f)).collect(),
    };
    r.origin = a.face_ids.clone();
    let c = remove_created_degenerates(&mut r, [&a, &b], &RunContext::none()).unwrap();
    assert_eq!((c.flipped, c.collapsed, c.remaining), (0, 0, 1), "{c:?}");
    assert!(matches!(
        validate_boolean(&r, [&a, &b], &RunContext::none()),
        Err(SolidError::DegenerateFace { .. })
    ));
}

#[test]
fn a_clean_result_is_unchanged() {
    let (a, b) = operands();
    let mut r = KernelResult {
        positions: a.positions.clone(),
        triangles: a.triangles.clone(),
        origin: a.face_ids.clone(),
    };
    let before = r.clone();
    let c = remove_created_degenerates(&mut r, [&a, &b], &RunContext::none()).unwrap();
    assert_eq!((c.flipped, c.collapsed, c.remaining), (0, 0, 0));
    assert_eq!(r, before);
}

#[test]
fn the_pass_is_cancellable() {
    let (a, b) = operands();
    let mut r = cube_with_a_cap();
    let ctx = RunContext::none();
    ctx.cancel.cancel();
    assert_eq!(
        remove_created_degenerates(&mut r, [&a, &b], &ctx),
        Err(SolidError::Cancelled)
    );
}
