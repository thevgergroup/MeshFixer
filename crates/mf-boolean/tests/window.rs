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

/// A closed tetrahedron whose edge B–C is split by a vertex M one ulp
/// from B (off the line, so every face has area, exactly): the faces
/// B-M-D and A-M-B are slivers, and any plane crossing B–D or A–B cuts
/// two of their edges within rounding of each other, wherever it is.
/// No box near the requested one escapes that crowding.
fn sliver_tetrahedron() -> Mesh {
    let (a, b, c, d) = (
        [5.0, 5.0, 5.0],
        [25.0, 5.0, 5.0],
        [5.0, 25.0, 5.0],
        [5.0, 5.0, 25.0],
    );
    let m = [25.0, 5.0f64.next_up(), 5.0];
    Mesh::new(
        vec![a, b, c, d, m],
        vec![
            [0, 1, 3],
            [0, 3, 2],
            [0, 2, 4],
            [0, 4, 1],
            [1, 4, 3],
            [4, 2, 3],
        ],
    )
    .unwrap()
}

#[test]
fn window_never_exceeds_d5_margin() {
    let target = sliver_tetrahedron();
    let (positions, triangles) = target.to_arrays();
    mf_boolean::validate_solid(&positions, &triangles, &RunContext::none()).unwrap();
    let window = mf_geometry::Aabb {
        min: [12.0, 3.0, 3.0],
        max: [18.0, 8.0, 8.0],
    };
    let margin = 0.5;
    let limit = mf_geometry::Aabb {
        min: [0.0, -10.0, -10.0],
        max: [30.0, 20.0, 20.0],
    };
    let r = mf_boolean::clip_to_box(
        &target,
        window,
        mf_boolean::ClipOptions {
            close_openings: true,
            limit,
            margin,
        },
        &RunContext::none(),
    );
    match r {
        Err(mf_boolean::ClipError::WindowDegenerate) => {}
        Ok(c) => {
            for a in 0..3 {
                assert!(
                    c.window.min[a] >= window.min[a] - margin
                        && c.window.max[a] <= window.max[a] + margin,
                    "the window grew past the D5 margin: {:?} from {window:?}",
                    c.window
                );
            }
        }
        Err(e) => panic!("refused, but not as a degenerate window: {e:?}"),
    }
}

#[test]
fn a_window_that_cant_escape_crowding_is_refused_by_name() {
    let target = sliver_tetrahedron();
    let tool = translated(&scaled(&cube(1.0), 4.0), [13.0, 4.0, 4.0]);
    let e = boolean(
        &target,
        &tool,
        BoolOp::Union,
        &ManifoldCpp::default(),
        &RunContext::none(),
    )
    .unwrap_err();
    assert_eq!(e.code(), "window_degenerate", "{e}");
}
