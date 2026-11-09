//! Camera framing and cursor zoom (M4 Task 2).

use glam::DVec3;
use mf_geometry::Aabb;
use mf_render::{OrbitCamera, screen_position};

fn unit_box(at: [f64; 3]) -> Aabb {
    Aabb {
        min: at,
        max: [at[0] + 1.0, at[1] + 1.0, at[2] + 1.0],
    }
}

#[test]
fn frame_recentres_on_the_box_and_keeps_the_view_direction() {
    let mut cam = OrbitCamera::fit(&unit_box([0.0, 0.0, 0.0]));
    cam.orbit(0.7, -0.3);
    let (yaw, pitch) = (cam.yaw, cam.pitch);
    let target = unit_box([10.0, -4.0, 2.0]);
    cam.frame(&target);
    assert_eq!((cam.yaw, cam.pitch), (yaw, pitch), "framing doesn't rotate");
    assert_eq!(cam.target, DVec3::from(target.center()));
    let fitted = OrbitCamera::fit(&target);
    assert!((cam.distance - fitted.distance).abs() < 1e-12);
    assert!((cam.radius - fitted.radius).abs() < 1e-12);
    // The whole box is on screen.
    for corner in [target.min, target.max] {
        let (x, y) = screen_position(&cam, cam.target, (800.0, 600.0), corner).unwrap();
        assert!((0.0..800.0).contains(&x) && (0.0..600.0).contains(&y));
    }
}

#[test]
fn zoom_at_keeps_the_point_under_the_cursor_fixed() {
    let bb = unit_box([0.0, 0.0, 0.0]);
    let mut cam = OrbitCamera::fit(&bb);
    let size = (800.0, 600.0);
    let px = (600.0, 200.0);
    // The world point on the target's view plane under the cursor.
    let (o, d) = cam.ray(cam.target, size, px);
    let (o, d) = (DVec3::from(o), DVec3::from(d));
    let normal = (cam.eye() - cam.target).normalize();
    let t = (cam.target - o).dot(normal) / d.dot(normal);
    let under = o + d * t;

    let before = cam.distance;
    cam.zoom_at(0.5, size, px);
    assert!((cam.distance - before * 0.5).abs() < 1e-12);
    let (x, y) = screen_position(&cam, cam.target, size, under.to_array()).unwrap();
    assert!(
        (x - px.0).abs() < 1e-6 && (y - px.1).abs() < 1e-6,
        "the point under the cursor stays put: ({x}, {y}) vs {px:?}"
    );

    // At the centre, zooming doesn't move the target.
    let target = cam.target;
    cam.zoom_at(2.0, size, (400.0, 300.0));
    assert!((cam.target - target).length() < 1e-9);
}

#[test]
fn look_from_puts_the_eye_on_that_side_of_the_target() {
    let mut cam = OrbitCamera::fit(&unit_box([0.0, 0.0, 0.0]));
    let target = cam.target;
    let distance = cam.distance;
    let from = DVec3::new(-1.0, 2.0, -0.5).normalize();
    cam.look_from(from);
    let dir = (cam.eye() - cam.target).normalize();
    assert!((dir - from).length() < 1e-9, "{dir} vs {from}");
    assert_eq!((cam.target, cam.distance), (target, distance));
    // Straight down is clamped short of the pole (the view matrix needs it).
    cam.look_from(DVec3::new(0.0, 0.0, -1.0));
    assert!((cam.eye() - cam.target).normalize().z > -1.0);
}
