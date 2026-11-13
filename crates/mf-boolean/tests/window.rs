#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The D5 window pipeline (`boolean`, `clip_to_box`, `triangulate_section`)
//! on small cases. The operation-level tests (refusals, replay, the
//! document) are in mf-operations' `tests/boolean.rs`.

use mf_boolean::{BoolOp, BooleanError, ManifoldCpp, SectionError, boolean, triangulate_section};
use mf_fixtures::{cube, scaled, translated, uv_sphere};
use mf_geometry::{FaceId, Mesh};
use mf_runtime::RunContext;

fn volume(m: &Mesh) -> f64 {
    mf_geometry::signed_volume(m) / 6.0
}

#[test]
fn offset_cubes_have_exact_volumes() {
    let target = cube(2.0);
    let tool = translated(&cube(1.0), [1.5, 0.5, 0.5]);
    for (op, want) in [
        (BoolOp::Union, 8.5),
        (BoolOp::Subtract, 7.5),
        (BoolOp::Intersect, 0.5),
    ] {
        let out = boolean(
            &target,
            &tool,
            op,
            &ManifoldCpp::default(),
            &RunContext::none(),
        )
        .unwrap_or_else(|e| panic!("{op:?}: {e}"));
        let v = volume(&out.mesh);
        assert!((v - want).abs() <= 1e-12 * want, "{op:?}: {v} vs {want}");
    }
}

#[test]
fn a_small_tool_on_a_sphere_keeps_faces_outside_the_window() {
    let target = scaled(&uv_sphere(48, 96), 10.0);
    let tool = translated(&cube(1.0), [9.5, -0.5, -0.5]);
    let out = boolean(
        &target,
        &tool,
        BoolOp::Union,
        &ManifoldCpp::default(),
        &RunContext::none(),
    )
    .unwrap();
    let removed: std::collections::HashSet<FaceId> =
        out.provenance.removed_faces.iter().copied().collect();
    assert!(
        removed.len() < target.face_count() / 20,
        "{}",
        removed.len()
    );
    for (f, t) in target.faces() {
        if !removed.contains(&f) {
            assert_eq!(out.mesh.face(f), Some(t));
        }
    }
}

#[test]
fn tools_that_dont_touch_are_no_overlap() {
    let r = boolean(
        &cube(1.0),
        &translated(&cube(1.0), [3.0, 0.0, 0.0]),
        BoolOp::Union,
        &ManifoldCpp::default(),
        &RunContext::none(),
    );
    assert_eq!(r.unwrap_err(), BooleanError::NoOverlap);
}

#[test]
fn sections_triangulate_with_holes_by_orientation() {
    let outer = vec![[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0]];
    let hole = vec![[1.0, 1.0], [1.0, 3.0], [3.0, 3.0], [3.0, 1.0]];
    let t = triangulate_section(&[outer.clone(), hole]).unwrap();
    let pts: Vec<[f64; 2]> = [
        outer.clone(),
        vec![[1.0, 1.0], [1.0, 3.0], [3.0, 3.0], [3.0, 1.0]],
    ]
    .concat();
    let area: f64 = t
        .iter()
        .map(|t| {
            let [a, b, c] = t.map(|i| pts[i as usize]);
            ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])) / 2.0
        })
        .sum();
    assert_eq!(area, 12.0);
    // A figure eight crosses itself.
    let eight = vec![[0.0, 0.0], [2.0, 2.0], [2.0, 0.0], [0.0, 2.0]];
    assert_eq!(
        triangulate_section(&[eight]),
        Err(SectionError::SelfIntersects)
    );
}

#[test]
fn a_mesh_edge_on_the_windows_edge_still_joins() {
    // The sphere's meridian edges lie in the plane x = y, and so does the
    // symmetric window's vertical edge: cuts there land within rounding of
    // each other. The window moves (asymmetrically) instead of handing
    // the kernel near-coincident seam points it would merge.
    let base = scaled(&uv_sphere(48, 96), 10.0);
    let mut e = base.edit();
    for (f, t) in base.faces() {
        if t.iter().all(|&v| base.corner_position(v)[2] > 9.5) {
            e.remove_face(f).unwrap();
        }
    }
    let target = e.finish().unwrap().0;
    let tool = translated(&scaled(&uv_sphere(24, 48), 4.0), [0.0, 0.0, 9.8]);
    let out = boolean(
        &target,
        &tool,
        BoolOp::Union,
        &ManifoldCpp::default(),
        &RunContext::none(),
    )
    .unwrap();
    assert_eq!(mf_geometry::inspect(&out.mesh).boundary_edges, 0);
}
