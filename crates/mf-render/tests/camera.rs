//! Camera framing and cursor zoom (M4 Task 2).

use glam::DVec3;
use mf_geometry::Aabb;
use mf_render::{CameraMove, OrbitCamera, ease_out, screen_position};

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

/// M4 Task 8a: framing a speck puts it at the view centre filling the
/// asked share of the view height, and keeps the whole scene within the
/// clipping planes.
#[test]
fn framing_fills_the_view_and_keeps_the_scene_unclipped() {
    let scene = Aabb {
        min: [-100.0; 3],
        max: [100.0; 3],
    };
    let cam = OrbitCamera::fit(&scene);
    let speck = Aabb {
        min: [80.0, 80.0, 0.0],
        max: [80.2, 80.1, 0.1],
    };
    let from = DVec3::new(0.0, -1.0, 1.0);
    let out = cam.framing(&speck, from, 0.4, &scene);
    assert_eq!(out.target, DVec3::from(speck.center()));
    assert!((out.view_dir() - from.normalize()).length() < 1e-9);
    let r = speck.diagonal() * 0.5;
    let fill = r / (out.distance * (out.fov_y * 0.5).tan());
    assert!((fill - 0.4).abs() < 1e-9, "{fill}");
    // Every scene corner is nearer than the far plane (distance + 2R).
    let far = out.distance + 2.0 * out.radius;
    for corner in [scene.min, scene.max, [-100.0, 100.0, -100.0]] {
        assert!((DVec3::from(corner) - out.eye()).length() < far);
    }
    // Centred on screen.
    let (x, y) = screen_position(&out, out.target, (800.0, 600.0), speck.center()).unwrap();
    assert!((x - 400.0).abs() < 1e-6 && (y - 300.0).abs() < 1e-6);
}

/// The move eases out from one camera to the other in its duration, the
/// short way round, and ends exactly on the target camera.
#[test]
fn a_camera_move_eases_out_and_ends_exactly() {
    let a = OrbitCamera::fit(&unit_box([0.0, 0.0, 0.0]));
    let mut b = a;
    b.target = DVec3::new(10.0, 0.0, 0.0);
    b.distance = a.distance * 0.01;
    b.yaw = a.yaw + 6.0; // the short way is −0.28 rad
    let m = CameraMove {
        from: a,
        to: b,
        start: 1.0,
        duration: 0.2,
    };
    assert_eq!(m.at(1.0), a);
    assert_eq!(m.at(0.5), a, "before the start it hasn't moved");
    assert_eq!(m.at(1.201), b);
    assert!(m.done(1.201) && !m.done(1.19));
    let mid = m.at(1.1);
    // Ease-out: past halfway at half time.
    let x = a.target.x + (10.0 - a.target.x) * ease_out(0.5);
    assert!((mid.target.x - x).abs() < 1e-9);
    assert!(ease_out(0.5) > 0.5);
    // Distance changes geometrically (an even zoom).
    let expected = a.distance * (0.01f64).powf(ease_out(0.5));
    assert!((mid.distance - expected).abs() < 1e-9 * a.distance);
    // Yaw turns the short way.
    let turn = 6.0 - std::f64::consts::TAU;
    assert!((mid.yaw - (a.yaw + turn * ease_out(0.5))).abs() < 1e-9);
    // Clipping never tighter than either end on the way.
    assert!(mid.radius >= a.radius.max(b.radius));
}
