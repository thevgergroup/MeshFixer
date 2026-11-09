#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Picking (MF-RENDER-004/005 for M3): boundary loops in screen space, faces
//! by ray cast. Object-space picking is the `DMat4::IDENTITY` case of the
//! transform-aware API (MF-ISSUE-042; see `transforms.rs` for translated,
//! rotated and mirrored objects).

use glam::{DMat4, DVec3};
use mf_fixtures::{cube, cube_with_missing_face};
use mf_geometry::FaceId;
use mf_render::{OrbitCamera, pick_loop, ray_pick, screen_position};

#[test]
fn loop_pick_finds_nearest_edge_within_tolerance() {
    let m = cube_with_missing_face();
    let bb = m.bounding_box().unwrap();
    let cam = OrbitCamera::fit(&bb);
    let origin = DVec3::from(bb.center());
    let size = (800.0, 600.0);
    let topo = m.topology();
    let hole = &topo.boundary_loops[0];
    // Midpoint of the first loop edge, on screen.
    let a = DVec3::from(m.corner_position(hole.vertices[0]));
    let b = DVec3::from(m.corner_position(hole.vertices[1]));
    let mid = screen_position(&cam, origin, size, ((a + b) * 0.5).to_array()).unwrap();
    assert_eq!(
        pick_loop(
            &m,
            &DMat4::IDENTITY,
            &cam,
            origin,
            size,
            (mid.0 + 3.0, mid.1 - 2.0),
            8.0
        ),
        Some(hole.id)
    );
    assert_eq!(
        pick_loop(&m, &DMat4::IDENTITY, &cam, origin, size, (5.0, 5.0), 8.0),
        None,
        "far from every loop"
    );
    assert_eq!(
        pick_loop(
            &cube(1.0),
            &DMat4::IDENTITY,
            &cam,
            origin,
            size,
            (mid.0, mid.1),
            8.0
        ),
        None,
        "closed mesh has no loops"
    );
}

#[test]
fn ray_pick_finds_nearest_face() {
    let m = cube(1.0);
    // Straight down onto the top face (faces 2 and 3 are z = 1).
    let hit = ray_pick(&m, &DMat4::IDENTITY, [0.3, 0.6, 5.0], [0.0, 0.0, -1.0]).unwrap();
    assert!(hit.0 == FaceId(2) || hit.0 == FaceId(3), "{hit:?}");
    assert!((hit.1 - 4.0).abs() < 1e-12);
    assert_eq!(
        ray_pick(&m, &DMat4::IDENTITY, [3.0, 3.0, 5.0], [0.0, 0.0, -1.0]),
        None,
        "misses"
    );
    assert_eq!(
        ray_pick(&m, &DMat4::IDENTITY, [0.5, 0.5, 5.0], [0.0, 0.0, 1.0]),
        None,
        "pointing away"
    );
}

#[test]
fn camera_ray_through_a_projected_point_hits_it() {
    let m = cube(1.0);
    let bb = m.bounding_box().unwrap();
    let cam = OrbitCamera::fit(&bb);
    let origin = DVec3::from(bb.center());
    let size = (640.0, 480.0);
    let target = [0.5, 0.5, 1.0];
    let px = screen_position(&cam, origin, size, target).unwrap();
    let (o, d) = cam.ray(origin, size, px);
    let (_, t) = ray_pick(&m, &DMat4::IDENTITY, o, d).unwrap();
    let p = [o[0] + d[0] * t, o[1] + d[1] * t, o[2] + d[2] * t];
    assert!((0..3).all(|k| (p[k] - target[k]).abs() < 1e-6), "{p:?}");
}

#[test]
fn loop_pick_ignores_edges_behind_the_surface() {
    // The cube's top face is missing. Seen from below, the hole's edges
    // still project inside the view, but the bottom face hides them.
    let m = cube_with_missing_face();
    let bb = m.bounding_box().unwrap();
    let mut cam = OrbitCamera::fit(&bb);
    let origin = DVec3::from(bb.center());
    let size = (800.0, 600.0);
    let topo = m.topology();
    let hole = &topo.boundary_loops[0];
    let a = DVec3::from(m.corner_position(hole.vertices[0]));
    let b = DVec3::from(m.corner_position(hole.vertices[1]));
    let mid_world = ((a + b) * 0.5).to_array();

    let mid = screen_position(&cam, origin, size, mid_world).unwrap();
    assert_eq!(
        pick_loop(&m, &DMat4::IDENTITY, &cam, origin, size, mid, 8.0),
        Some(hole.id),
        "visible from above"
    );
    cam.pitch = -60f64.to_radians();
    let mid = screen_position(&cam, origin, size, mid_world).unwrap();
    assert_eq!(
        pick_loop(&m, &DMat4::IDENTITY, &cam, origin, size, mid, 8.0),
        None,
        "hidden from below"
    );
}
