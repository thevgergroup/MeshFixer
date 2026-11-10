#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The object transform gizmo's maths (MF-OBJ-001): handle geometry,
//! hit-testing, drag maths, snapping, and decompose/compose. Pure `f64`,
//! no GPU, no egui.

use glam::DVec3;
use mf_render::OrbitCamera;
use mf_render::{
    Decomposed, Gizmo, GizmoMode, Handle, HandleGeometry, PlateSnap, Snap, compose, decompose,
    model_matrix,
};

const IDENTITY: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

/// A camera looking down +X-ish at the origin, far enough back that the
/// gizmo's screen-space handles are well separated.
fn test_camera() -> OrbitCamera {
    OrbitCamera {
        target: DVec3::ZERO,
        distance: 10.0,
        yaw: -45f64.to_radians(),
        pitch: 25f64.to_radians(),
        fov_y: 40f64.to_radians(),
        radius: 1.0,
    }
}

const VIEW: (f64, f64) = (800.0, 600.0);

fn screen_of(cam: &OrbitCamera, pivot: DVec3, world: DVec3) -> (f64, f64) {
    mf_render::screen_position(cam, pivot, VIEW, world.to_array()).unwrap()
}

#[test]
fn axis_drag_moves_only_along_that_axis() {
    let cam = test_camera();
    let pivot = DVec3::ZERO;
    let gizmo = Gizmo {
        pivot,
        mode: GizmoMode::Move,
        size_pt: 110.0,
    };
    let start_px = screen_of(&cam, pivot, pivot);
    let drag = gizmo.begin(Handle::Axis(0), start_px, &cam, VIEW, IDENTITY);

    // The closest point on the X axis line to the ray through a pixel
    // 100 px to the right, computed independently of `Drag`, to check the
    // drag reproduces exactly the ray-axis closest-point distance.
    let end_px = (start_px.0 + 100.0, start_px.1);
    let (ro, rd) = cam.ray(pivot, VIEW, end_px);
    let ro = DVec3::from(ro);
    let rd = DVec3::from(rd);
    // Closest point between the ray and the line through the origin along
    // X, via Ericson's (*Real-Time Collision Detection*, 5.1.9) two-line
    // closest-point formula, specialised to infinite lines.
    let axis_dir = DVec3::X;
    let r = pivot - ro; // line_origin (pivot = 0) - ro
    let a = axis_dir.dot(axis_dir);
    let e_len2 = rd.dot(rd);
    let b = axis_dir.dot(rd);
    let c = axis_dir.dot(r);
    let f = rd.dot(r);
    let denom = a * e_len2 - b * b;
    assert!(
        denom.abs() > 1e-12,
        "ray not parallel to axis in this setup"
    );
    let s = (b * f - c * e_len2) / denom;
    let expected_t9 = s; // axis origin (pivot.x = 0) + s

    let snap = Snap {
        grid: None,
        angle_deg: None,
        scale_step: None,
        plate: None,
    };
    let result = drag.update(end_px, &snap);

    assert!((result[9] - expected_t9).abs() < 1e-9, "{result:?}");
    assert!((result[10] - 0.0).abs() < 1e-9, "{result:?}");
    assert!((result[11] - 0.0).abs() < 1e-9, "{result:?}");
    // Linear part is untouched by a move.
    for i in 0..9 {
        assert!((result[i] - IDENTITY[i]).abs() < 1e-12, "{result:?}");
    }
}

#[test]
fn plane_handle_moves_in_its_plane() {
    let cam = test_camera();
    let pivot = DVec3::ZERO;
    let gizmo = Gizmo {
        pivot,
        mode: GizmoMode::Move,
        size_pt: 110.0,
    };
    let start_px = screen_of(&cam, pivot, pivot);
    // Plane(2) is the XY plane (normal Z), so dragging in it must not move Z.
    let drag = gizmo.begin(Handle::Plane(2), start_px, &cam, VIEW, IDENTITY);
    let end_px = (start_px.0 + 40.0, start_px.1 - 25.0);
    let snap = Snap {
        grid: None,
        angle_deg: None,
        scale_step: None,
        plate: None,
    };
    let result = drag.update(end_px, &snap);

    assert!(
        (result[11] - 0.0).abs() < 1e-9,
        "z must stay zero: {result:?}"
    );
    assert!(
        result[9].abs() > 1e-6 || result[10].abs() > 1e-6,
        "plane drag should move something: {result:?}"
    );
    for i in 0..9 {
        assert!((result[i] - IDENTITY[i]).abs() < 1e-12, "{result:?}");
    }
}

#[test]
fn ring_drag_rotates_about_pivot_and_snaps_to_15_degrees() {
    let cam = test_camera();
    let pivot = DVec3::new(1.0, 2.0, 3.0);
    let gizmo = Gizmo {
        pivot,
        mode: GizmoMode::Rotate,
        size_pt: 110.0,
    };
    // Start the drag at a point on the Z ring, offset from the pivot in X.
    let ring_px = screen_of(&cam, pivot, pivot + DVec3::X * 0.3);
    let drag = gizmo.begin(Handle::Ring(2), ring_px, &cam, VIEW, IDENTITY);

    // Find the pixel that is exactly a 37 degree rotation (about Z) away
    // from the ring start point, as seen on screen, by rotating the world
    // point itself and re-projecting - the drag must recover this angle.
    let rot37 = glam::DMat3::from_euler(glam::EulerRot::XYZEx, 0.0, 0.0, 37f64.to_radians());
    let end_world = pivot + rot37 * (DVec3::X * 0.3);
    let end_px = screen_of(&cam, pivot, end_world);

    let snap_on = Snap {
        grid: None,
        angle_deg: Some(15.0),
        scale_step: None,
        plate: None,
    };
    let snapped = drag.update(end_px, &snap_on);
    let snapped_decomp = decompose(&snapped);
    match snapped_decomp {
        Decomposed::Trs { rotate_deg_xyz, .. } => {
            assert!((rotate_deg_xyz.z - 30.0).abs() < 1e-6, "{rotate_deg_xyz:?}");
        }
        Decomposed::Sheared => panic!("expected Trs"),
    }

    let snap_off = Snap {
        grid: None,
        angle_deg: None,
        scale_step: None,
        plate: None,
    };
    let unsnapped = drag.update(end_px, &snap_off);
    let unsnapped_decomp = decompose(&unsnapped);
    match unsnapped_decomp {
        Decomposed::Trs { rotate_deg_xyz, .. } => {
            assert!((rotate_deg_xyz.z - 37.0).abs() < 0.01, "{rotate_deg_xyz:?}");
        }
        Decomposed::Sheared => panic!("expected Trs"),
    }

    // The pivot stays fixed under rotation about itself.
    let m_snapped = model_matrix(&snapped);
    let p = m_snapped * glam::DVec4::new(pivot.x, pivot.y, pivot.z, 1.0);
    assert!(
        (DVec3::new(p.x, p.y, p.z) - pivot).length() < 1e-12,
        "{p:?}"
    );
}

#[test]
fn uniform_scale_about_pivot() {
    let cam = test_camera();
    let pivot = DVec3::new(2.0, -1.0, 0.5);
    let gizmo = Gizmo {
        pivot,
        mode: GizmoMode::Scale,
        size_pt: 110.0,
    };
    let start_px = screen_of(&cam, pivot, pivot + DVec3::X * 0.5);
    let drag = gizmo.begin(Handle::ScaleUniform, start_px, &cam, VIEW, IDENTITY);
    let end_px = (start_px.0 + 60.0, start_px.1);
    let snap = Snap {
        grid: None,
        angle_deg: None,
        scale_step: None,
        plate: None,
    };
    let result = drag.update(end_px, &snap);
    let decomp = decompose(&result);
    let scale = match decomp {
        Decomposed::Trs { scale, .. } => scale,
        Decomposed::Sheared => panic!("expected Trs"),
    };
    assert!((scale.x - scale.y).abs() < 1e-9, "{scale:?}");
    assert!((scale.y - scale.z).abs() < 1e-9, "{scale:?}");
    assert!(scale.x > 1.0, "dragging outward should scale up: {scale:?}");

    // The pivot stays fixed under scale about itself.
    let m = model_matrix(&result);
    let p = m * glam::DVec4::new(pivot.x, pivot.y, pivot.z, 1.0);
    assert!((DVec3::new(p.x, p.y, p.z) - pivot).length() < 1e-9, "{p:?}");
}

#[test]
fn grid_snap_rounds_translation_to_1mm() {
    let cam = test_camera();
    let pivot = DVec3::ZERO;
    let gizmo = Gizmo {
        pivot,
        mode: GizmoMode::Move,
        size_pt: 110.0,
    };
    let start_px = screen_of(&cam, pivot, pivot);
    let drag = gizmo.begin(Handle::Axis(0), start_px, &cam, VIEW, IDENTITY);
    let end_px = (start_px.0 + 137.0, start_px.1);
    let snap = Snap {
        grid: Some(1.0),
        angle_deg: None,
        scale_step: None,
        plate: None,
    };
    let result = drag.update(end_px, &snap);
    let rounded = (result[9] / 1.0).round() * 1.0;
    assert!((result[9] - rounded).abs() < 1e-9, "{result:?}");
}

#[test]
fn plate_snap_lands_min_z_on_zero_within_8pt() {
    let cam = OrbitCamera {
        target: DVec3::ZERO,
        distance: 10.0,
        yaw: -45f64.to_radians(),
        pitch: 45f64.to_radians(),
        fov_y: 40f64.to_radians(),
        radius: 1.0,
    };
    let pivot = DVec3::new(0.0, 0.0, 2.0);
    let gizmo = Gizmo {
        pivot,
        mode: GizmoMode::Move,
        size_pt: 110.0,
    };
    let start_px = screen_of(&cam, pivot, pivot);
    let drag = gizmo.begin(Handle::Axis(2), start_px, &cam, VIEW, IDENTITY);

    // At `start` (translate = 0), the object's own min Z sits 2.0 below
    // the pivot in world space, i.e. world_min_z(start) = -2.0 (the
    // pivot is above the mesh, as for an object sitting on the plate).
    // Dragging the Z handle to translate.z = t moves that min Z to
    // (-2.0 + t); it lands exactly on the plate (zero) at t = 2.0, i.e.
    // a world end point of pivot.z + 2.0 = 4.0 (the axis anchor is the
    // ray's closest point on the Z line through the pivot).
    let near_landing = pivot + DVec3::Z * 2.0 - DVec3::Z * 0.02;
    let end_px = screen_of(&cam, pivot, near_landing);

    let snap = Snap {
        grid: None,
        angle_deg: None,
        scale_step: None,
        plate: Some(PlateSnap {
            world_min_z: -2.0,
            within_pt: 8.0,
        }),
    };
    let result = drag.update(end_px, &snap);
    assert!((result[11] - 2.0).abs() < 1e-9, "{result:?}");

    // A drag end point whose resulting (pre-snap) object min-Z is exactly
    // 9pt of screen distance from the plate (zero), measured at the
    // object's own depth near the plate, not the pivot's: snapping must
    // not engage. The world Z step for 9pt is found by bisection, the
    // same way the gizmo sizes its own screen-constant handles.
    let plate_px = screen_of(&cam, pivot, DVec3::new(0.0, 0.0, 0.0));
    let step_for_px = |target_px: f64| -> f64 {
        let (mut lo, mut hi) = (0.0_f64, 10.0_f64);
        for _ in 0..60 {
            let mid = 0.5 * (lo + hi);
            let px = screen_of(&cam, pivot, DVec3::new(0.0, 0.0, mid));
            let d = ((px.0 - plate_px.0).powi(2) + (px.1 - plate_px.1).powi(2)).sqrt();
            if d < target_px { lo = mid } else { hi = mid }
        }
        0.5 * (lo + hi)
    };
    let step_9pt = step_for_px(9.0);
    // translate.z = 2.0 + step_9pt gives min-Z = step_9pt, exactly 9pt
    // (by construction) from the plate.
    let far_world = pivot + DVec3::Z * (2.0 + step_9pt);
    let far_px = screen_of(&cam, pivot, far_world);
    let check_px = screen_of(&cam, pivot, DVec3::new(0.0, 0.0, step_9pt));
    let screen_gap = ((check_px.0 - plate_px.0).powi(2) + (check_px.1 - plate_px.1).powi(2)).sqrt();
    assert!(
        (screen_gap - 9.0).abs() < 1e-3,
        "test setup: {screen_gap}pt"
    );
    let result_far = drag.update(far_px, &snap);
    assert!(
        (result_far[11] - 2.0).abs() > 1e-6,
        "should not snap at 9pt away: {result_far:?}"
    );
}

#[test]
fn end_on_axis_is_not_hittable() {
    let cam = test_camera();
    let pivot = DVec3::ZERO;
    let gizmo = Gizmo {
        pivot,
        mode: GizmoMode::Move,
        size_pt: 110.0,
    };

    // With the camera square-on (not end-on), the X axis is hittable along
    // its own screen segment, away from the centre and the other axes.
    let shapes = gizmo.handles(&cam, VIEW);
    let axis_x = shapes
        .iter()
        .find(|s| s.handle == Handle::Axis(0))
        .expect("x axis handle present when not end-on");
    let HandleGeometry::Arrow { tip, .. } = axis_x.geometry else {
        panic!("axis handle must have Arrow geometry: {axis_x:?}");
    };
    assert_eq!(gizmo.hit(&cam, VIEW, tip), Some(Handle::Axis(0)));

    // Looking almost straight down the X axis: yaw/pitch chosen so the
    // camera forward direction is within a couple of degrees of +/-X.
    // Every pixel along the (now near-degenerate) X arm must not report
    // the X axis handle, even though it would for a square-on camera.
    let mut end_on_cam = cam;
    end_on_cam.yaw = 0.0;
    end_on_cam.pitch = 0.0; // forward is along -X-ish per OrbitCamera's convention
    for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
        let world = pivot + DVec3::X * (0.5 * t + 1e-6);
        let px = screen_of(&end_on_cam, pivot, world);
        assert_ne!(
            gizmo.hit(&end_on_cam, VIEW, px),
            Some(Handle::Axis(0)),
            "t={t}"
        );
    }
}

#[test]
fn handles_keep_screen_size_at_any_zoom() {
    let pivot = DVec3::ZERO;
    let gizmo = Gizmo {
        pivot,
        mode: GizmoMode::Move,
        size_pt: 110.0,
    };
    for &zoom in &[0.01_f64, 100.0] {
        let cam = OrbitCamera {
            target: DVec3::ZERO,
            distance: 10.0 * zoom,
            yaw: -45f64.to_radians(),
            pitch: 25f64.to_radians(),
            fov_y: 40f64.to_radians(),
            radius: 1.0 * zoom,
        };
        let centre_px = screen_of(&cam, pivot, pivot);
        let shapes = gizmo.handles(&cam, VIEW);
        let axis_x = shapes
            .iter()
            .find(|s| s.handle == Handle::Axis(0))
            .expect("x axis handle present");
        let HandleGeometry::Arrow { tip, .. } = axis_x.geometry else {
            panic!("axis handle must have Arrow geometry: {axis_x:?}");
        };
        // Measured from the pivot's own screen position, not
        // `shaft_start` (which is inset a few pixels from the pivot, a
        // dead-zone reserved for the centre handle).
        let len = ((tip.0 - centre_px.0).powi(2) + (tip.1 - centre_px.1).powi(2)).sqrt();
        assert!((len - 110.0).abs() < 1.0, "zoom {zoom}: len {len}");
    }
}

/// Fix round 1, review point 3: rings used to all draw/hit as the same
/// screen-space circle (`radius: Some(size_pt)`, centred on the pivot),
/// so the X and Y rings were indistinguishable — any point on that circle
/// hit whichever ring happened to be found first. Each ring must now
/// project its own 3D circle (perpendicular to its own axis), so from an
/// oblique view the X and Y rings trace different screen ellipses and a
/// point on one is not on the other.
#[test]
fn x_ring_and_y_ring_are_distinct_and_hittable_from_an_oblique_view() {
    let cam = test_camera(); // oblique: yaw -45, pitch 25 degrees
    let pivot = DVec3::ZERO;
    let gizmo = Gizmo {
        pivot,
        mode: GizmoMode::Rotate,
        size_pt: 110.0,
    };
    let shapes = gizmo.handles(&cam, VIEW);
    let ring_points = |i: u8| {
        let shape = shapes
            .iter()
            .find(|s| s.handle == Handle::Ring(i))
            .unwrap_or_else(|| panic!("ring {i} present"));
        match &shape.geometry {
            HandleGeometry::Ring { points, .. } => points.clone(),
            other => panic!("ring {i} must have Ring geometry: {other:?}"),
        }
    };
    let x_points = ring_points(0);
    let y_points = ring_points(1);
    assert!(x_points.len() >= 8, "{}", x_points.len());
    assert_eq!(x_points.len(), y_points.len());

    // The two rings' projected ellipses are not the same set of points:
    // at least one X-ring point is far (> 1pt) from every Y-ring point.
    let min_dist_to_y = |p: (f64, f64)| {
        y_points
            .iter()
            .map(|q| ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt())
            .fold(f64::INFINITY, f64::min)
    };
    assert!(
        x_points.iter().any(|&p| min_dist_to_y(p) > 1.0),
        "the X and Y rings must trace different screen ellipses, not the same circle"
    );

    // Each ring is independently hittable at one of its own (front-facing)
    // points, and that hit resolves to the ring whose points those are,
    // not the other one. Picks the front-facing point *furthest* from the
    // other ring's own polyline, not just the first one, since the two
    // ellipses can pass close together (or cross) at some angles.
    let x_shape = shapes.iter().find(|s| s.handle == Handle::Ring(0)).unwrap();
    let y_shape = shapes.iter().find(|s| s.handle == Handle::Ring(1)).unwrap();
    let (
        HandleGeometry::Ring {
            points: xp,
            front: xf,
        },
        HandleGeometry::Ring {
            points: yp,
            front: yf,
        },
    ) = (&x_shape.geometry, &y_shape.geometry)
    else {
        panic!("expected Ring geometry");
    };
    let dist_to = |p: (f64, f64), others: &[(f64, f64)]| {
        others
            .iter()
            .map(|q| ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt())
            .fold(f64::INFINITY, f64::min)
    };
    let farthest_front_point = |points: &[(f64, f64)], front: &[bool], others: &[(f64, f64)]| {
        points
            .iter()
            .zip(front)
            .filter(|&(_, &f)| f)
            .map(|(p, _)| *p)
            .max_by(|a, b| dist_to(*a, others).total_cmp(&dist_to(*b, others)))
            .expect("a ring must have at least one front-facing point")
    };
    let x_hit_px = farthest_front_point(xp, xf, yp);
    let y_hit_px = farthest_front_point(yp, yf, xp);
    assert_eq!(gizmo.hit(&cam, VIEW, x_hit_px), Some(Handle::Ring(0)));
    assert_eq!(gizmo.hit(&cam, VIEW, y_hit_px), Some(Handle::Ring(1)));
}

/// Fix round 1, review point 3: a plane handle used to draw/hit as a bare
/// diagonal line between two points; it must now be a small filled square,
/// hittable anywhere in its interior (not just along an edge).
#[test]
fn plane_handle_hit_inside_its_square() {
    let cam = test_camera();
    let pivot = DVec3::ZERO;
    let gizmo = Gizmo {
        pivot,
        mode: GizmoMode::Move,
        size_pt: 110.0,
    };
    let shapes = gizmo.handles(&cam, VIEW);
    // Plane(2) is the XY plane handle.
    let plane = shapes
        .iter()
        .find(|s| s.handle == Handle::Plane(2))
        .expect("XY plane handle present");
    let HandleGeometry::Plane { corners } = &plane.geometry else {
        panic!("plane handle must have Plane geometry: {plane:?}");
    };
    // The square's own centroid: well inside its interior, not on an edge.
    let centroid = (
        corners.iter().map(|c| c.0).sum::<f64>() / 4.0,
        corners.iter().map(|c| c.1).sum::<f64>() / 4.0,
    );
    assert_eq!(gizmo.hit(&cam, VIEW, centroid), Some(Handle::Plane(2)));
}

/// Fix round 1, review point 3: the centre (and uniform-scale) point
/// handle is a small disc, hittable near the pivot's own screen position.
/// Every axis shaft now starts a few pixels out from the pivot (not
/// exactly at it), leaving a dead-zone reserved for the centre disc —
/// without that inset, a shaft beginning exactly at the pivot is always
/// within the disc's own hit radius of it, and Task 1's priority order
/// (axis beats centre) means the axis would always win there, making the
/// centre handle practically unreachable.
#[test]
fn centre_disc_is_hittable() {
    let cam = test_camera();
    let pivot = DVec3::new(0.5, -0.5, 0.2);
    let gizmo = Gizmo {
        pivot,
        mode: GizmoMode::Move,
        size_pt: 110.0,
    };
    let centre_px = screen_of(&cam, pivot, pivot);
    assert_eq!(gizmo.hit(&cam, VIEW, centre_px), Some(Handle::Centre));
    // A couple of pixels off still lands inside the dead-zone.
    let off_axis_px = (centre_px.0 + 3.0, centre_px.1 + 2.0);
    assert_eq!(gizmo.hit(&cam, VIEW, off_axis_px), Some(Handle::Centre));

    let shapes = gizmo.handles(&cam, VIEW);

    let centre = shapes
        .iter()
        .find(|s| s.handle == Handle::Centre)
        .expect("centre handle present");
    assert!(matches!(centre.geometry, HandleGeometry::Point { .. }));
}

#[test]
fn decompose_compose_round_trip() {
    // `decompose` reports `translate` as the image of the local origin
    // (t[9..12]); `compose` reproduces exactly that when `pivot` is zero,
    // since a non-zero pivot deliberately keeps the pivot point fixed
    // under rotate/scale instead (see `uniform_scale_about_pivot` and
    // `ring_drag_rotates_about_pivot_and_snaps_to_15_degrees`).
    let pivot = DVec3::ZERO;
    for &deg in &[0.0_f64, 90.0, 179.9, 89.9, 90.1] {
        let translate = DVec3::new(1.5, -2.25, 3.0);
        let rotate = DVec3::new(deg * 0.3, deg, deg * 0.7);
        let scale = DVec3::new(1.2, 0.8, 2.0);
        let t = compose(translate, rotate, scale, pivot);
        let decomposed = decompose(&t);
        match decomposed {
            Decomposed::Trs {
                translate: t2,
                rotate_deg_xyz: r2,
                scale: s2,
            } => {
                let t_back = compose(t2, r2, s2, pivot);
                for i in 0..12 {
                    assert!(
                        (t[i] - t_back[i]).abs() < 1e-6,
                        "deg {deg}: {t:?} vs {t_back:?}"
                    );
                }
            }
            Decomposed::Sheared => panic!("deg {deg}: expected Trs, got Sheared"),
        }
    }
}

#[test]
fn mirrored_transform_decomposes_with_negative_x_scale() {
    let translate = DVec3::new(0.0, 0.0, 0.0);
    let rotate = DVec3::ZERO;
    let scale = DVec3::new(-1.0, 1.0, 1.0);
    let pivot = DVec3::ZERO;
    let t = compose(translate, rotate, scale, pivot);
    let decomposed = decompose(&t);
    match decomposed {
        Decomposed::Trs { scale: s, .. } => {
            assert!(s.x < 0.0, "{s:?}");
        }
        Decomposed::Sheared => panic!("expected Trs for a pure mirror"),
    }
}

#[test]
fn shear_is_reported_as_sheared() {
    // A shear in the XY plane: not expressible as rotate+scale.
    let t: [f64; 12] = [1.0, 0.0, 0.0, 0.5, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
    let decomposed = decompose(&t);
    assert!(matches!(decomposed, Decomposed::Sheared), "{decomposed:?}");
}

// --- Phase 1 design stream (owner review): no view ring at rest, and a
// curved-arrow grip on each rotation ring. ---

fn rotate_gizmo(pivot: DVec3) -> Gizmo {
    Gizmo {
        pivot,
        mode: GizmoMode::Rotate,
        size_pt: 110.0,
    }
}

/// A camera at `yaw`/`pitch` degrees around the origin.
fn camera_at(yaw: f64, pitch: f64) -> OrbitCamera {
    OrbitCamera {
        target: DVec3::ZERO,
        distance: 10.0,
        yaw: yaw.to_radians(),
        pitch: pitch.to_radians(),
        fov_y: 40f64.to_radians(),
        radius: 1.0,
    }
}

/// A grip, for these tests: its ring, its band's middle point, and the
/// centroid of each arrowhead.
type Grip = (u8, (f64, f64), [(f64, f64); 2]);

/// Every grip of `shapes`.
fn grips(shapes: &[mf_render::HandleShape]) -> Vec<Grip> {
    shapes
        .iter()
        .filter_map(|s| match (&s.geometry, s.handle) {
            (HandleGeometry::RingGrip { arc, heads }, Handle::Ring(i)) => {
                let mid = arc[arc.len() / 2];
                let c = |t: &[(f64, f64); 3]| {
                    (
                        (t[0].0 + t[1].0 + t[2].0) / 3.0,
                        (t[0].1 + t[1].1 + t[2].1) / 3.0,
                    )
                };
                Some((i, mid, [c(&heads[0]), c(&heads[1])]))
            }
            (HandleGeometry::RingGrip { .. }, other) => panic!("a grip on {other:?}"),
            _ => None,
        })
        .collect()
}

/// The gizmo at rest draws no view ring: only the three axis rings and a
/// grip on each (the owner read the grey outer ring as a mistake).
#[test]
fn rotate_mode_draws_three_rings_each_with_a_grip_and_no_view_ring() {
    let cam = test_camera();
    let gizmo = rotate_gizmo(DVec3::ZERO);
    let shapes = gizmo.handles(&cam, VIEW);
    assert!(
        shapes.iter().all(|s| s.handle != Handle::ViewRing),
        "no view ring at rest"
    );
    assert!(
        shapes
            .iter()
            .all(|s| !matches!(s.geometry, HandleGeometry::Circle { .. })),
        "no plain circle at rest"
    );
    let rings: Vec<u8> = shapes
        .iter()
        .filter_map(|s| match (&s.geometry, s.handle) {
            (HandleGeometry::Ring { .. }, Handle::Ring(i)) => Some(i),
            _ => None,
        })
        .collect();
    assert_eq!(rings, [0, 1, 2]);
    let with_grips: Vec<u8> = grips(&shapes).iter().map(|g| g.0).collect();
    assert_eq!(with_grips, [0, 1, 2], "each ring has one grip");
    // Grips come after every ring, so no ring's line crosses a grip.
    let first_grip = shapes
        .iter()
        .position(|s| matches!(s.geometry, HandleGeometry::RingGrip { .. }))
        .unwrap();
    assert!(
        shapes[first_grip..]
            .iter()
            .all(|s| matches!(s.geometry, HandleGeometry::RingGrip { .. }))
    );
    // Move and Scale have no rings or grips.
    for mode in [GizmoMode::Move, GizmoMode::Scale] {
        let g = Gizmo { mode, ..gizmo };
        assert!(g.handles(&cam, VIEW).iter().all(|s| !matches!(
            s.geometry,
            HandleGeometry::RingGrip { .. }
                | HandleGeometry::Ring { .. }
                | HandleGeometry::Circle { .. }
        )));
    }
}

/// Each grip is its ring's handle: pressing its band or either arrowhead
/// hits that ring, and dragging from it along the arrow turns the object
/// about that ring's axis.
#[test]
fn a_ring_grip_is_hittable_and_turns_about_its_axis() {
    let pivot = DVec3::new(1.0, 2.0, 3.0);
    let gizmo = rotate_gizmo(pivot);
    for cam in [
        test_camera(),
        camera_at(30.0, 35.0),
        camera_at(-120.0, 20.0),
    ] {
        let shapes = gizmo.handles(&cam, VIEW);
        for (i, mid, heads) in grips(&shapes) {
            assert_eq!(
                gizmo.hit(&cam, VIEW, mid),
                Some(Handle::Ring(i)),
                "band {i}"
            );
            for c in heads {
                assert_eq!(gizmo.hit(&cam, VIEW, c), Some(Handle::Ring(i)), "head {i}");
            }
            // Drag from the band's middle to one arrowhead's centroid:
            // a turn about axis i only.
            let drag = gizmo.begin(Handle::Ring(i), mid, &cam, VIEW, IDENTITY);
            let off = Snap {
                grid: None,
                angle_deg: None,
                scale_step: None,
                plate: None,
            };
            let t = drag.update(heads[0], &off);
            let Decomposed::Trs { rotate_deg_xyz, .. } = decompose(&t) else {
                panic!("a turn is not a shear");
            };
            let r = rotate_deg_xyz.to_array();
            assert!(r[i as usize].abs() > 1.0, "ring {i} turned {r:?}");
            for (k, v) in r.iter().enumerate() {
                if k != i as usize {
                    assert!(v.abs() < 1e-6, "ring {i} turned about {k}: {r:?}");
                }
            }
        }
    }
}

/// The grip sits where the ring is nearest the viewer (a ring seen at an
/// angle, neither face-on nor edge-on): on the ring's front half, at the
/// ring point in the eye's direction.
#[test]
fn a_grip_sits_at_the_ring_point_nearest_the_viewer() {
    let cam = test_camera();
    let gizmo = rotate_gizmo(DVec3::ZERO);
    let shapes = gizmo.handles(&cam, VIEW);
    for (i, mid, _) in grips(&shapes) {
        let axis = [DVec3::X, DVec3::Y, DVec3::Z][i as usize];
        let ring = shapes.iter().find(|s| s.handle == Handle::Ring(i)).unwrap();
        let HandleGeometry::Ring { points, .. } = &ring.geometry else {
            panic!("a ring");
        };
        // The ring's world radius, from its first point.
        let centre = screen_of(&cam, DVec3::ZERO, DVec3::ZERO);
        let toward_eye = cam.eye().reject_from(axis).normalize();
        // Bisect the world radius whose projection along `toward_eye`
        // lands on one of the ring's own points (they share the radius).
        let near = |r: f64| screen_of(&cam, DVec3::ZERO, toward_eye * r);
        let on_ring = |p: (f64, f64)| {
            points
                .iter()
                .map(|q| (p.0 - q.0).hypot(p.1 - q.1))
                .fold(f64::INFINITY, f64::min)
        };
        let r = (1..=400)
            .map(|k| k as f64 * 0.005)
            .min_by(|a, b| on_ring(near(*a)).total_cmp(&on_ring(near(*b))))
            .unwrap();
        let want = near(r);
        let d = (want.0 - mid.0).hypot(want.1 - mid.1);
        assert!(
            d < 6.0,
            "ring {i}: grip at {mid:?}, nearest point {want:?} ({d:.1} px apart)"
        );
        assert!(
            (mid.0 - centre.0).hypot(mid.1 - centre.1) > 30.0,
            "ring {i}'s grip is clear of the pivot"
        );
    }
}

/// Edge-on (the Z ring from a level camera) the nearest point would sit on
/// the pivot, over the other handles: the grip slides out along the ring
/// instead. Face-on (the Z ring from straight above) it rests on the
/// ring's drawn half.
#[test]
fn a_grip_stays_clear_of_the_pivot_edge_on_and_on_the_drawn_half_face_on() {
    let gizmo = rotate_gizmo(DVec3::ZERO);
    for cam in [
        camera_at(-30.0, 0.0),
        camera_at(-30.0, 89.0),
        camera_at(0.0, 0.0),
    ] {
        let centre = screen_of(&cam, DVec3::ZERO, DVec3::ZERO);
        let shapes = gizmo.handles(&cam, VIEW);
        let gs = grips(&shapes);
        assert_eq!(gs.len(), 3, "every ring keeps its grip");
        for (i, mid, _) in gs {
            let ring = shapes.iter().find(|s| s.handle == Handle::Ring(i)).unwrap();
            let HandleGeometry::Ring { points, front } = &ring.geometry else {
                panic!("a ring");
            };
            let r_max = points
                .iter()
                .map(|p| (p.0 - centre.0).hypot(p.1 - centre.1))
                .fold(0.0, f64::max);
            let out = (mid.0 - centre.0).hypot(mid.1 - centre.1);
            assert!(
                out >= 0.33 * r_max,
                "ring {i} grip {out:.1} px from the pivot (ring {r_max:.1})"
            );
            // On (near) a drawn, front-facing point of its ring.
            let near_front = points
                .iter()
                .zip(front)
                .filter(|(_, f)| **f)
                .map(|(q, _)| (mid.0 - q.0).hypot(mid.1 - q.1))
                .fold(f64::INFINITY, f64::min);
            assert!(
                near_front < 8.0,
                "ring {i}'s grip is {near_front:.1} px off its drawn half"
            );
        }
    }
}

/// The view ring isn't drawn at rest, but pointing just outside the axis
/// rings finds it: that hover draws it (`shapes` with it highlighted), and
/// a drag there turns about the view direction.
#[test]
fn the_view_ring_appears_only_when_pointed_at() {
    let cam = test_camera();
    let gizmo = rotate_gizmo(DVec3::ZERO);
    let centre = screen_of(&cam, DVec3::ZERO, DVec3::ZERO);
    let r = gizmo.size_pt * mf_render::VIEW_RING_SCALE;
    // Up and to the left, away from every grip.
    let a = 200f64.to_radians();
    let px = (centre.0 + r * a.cos(), centre.1 + r * a.sin());
    assert_eq!(gizmo.hit(&cam, VIEW, px), Some(Handle::ViewRing));
    assert!(
        gizmo
            .shapes(&cam, VIEW, None)
            .iter()
            .all(|s| s.handle != Handle::ViewRing)
    );
    let shown = gizmo.shapes(&cam, VIEW, Some(Handle::ViewRing));
    let ring = shown
        .iter()
        .find(|s| s.handle == Handle::ViewRing)
        .expect("the hovered view ring is drawn");
    assert!(matches!(ring.geometry, HandleGeometry::Circle { .. }));
    // Not in Move or Scale.
    for mode in [GizmoMode::Move, GizmoMode::Scale] {
        let g = Gizmo { mode, ..gizmo };
        assert_ne!(g.hit(&cam, VIEW, px), Some(Handle::ViewRing));
        assert!(
            g.shapes(&cam, VIEW, Some(Handle::ViewRing))
                .iter()
                .all(|s| s.handle != Handle::ViewRing)
        );
    }
}

/// Distance from `p` to what `shape` draws (pixels), for the parity sweep.
fn drawn_distance(p: (f64, f64), shape: &mf_render::HandleShape) -> f64 {
    let seg = |a: (f64, f64), b: (f64, f64)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l2 = dx * dx + dy * dy;
        let t = if l2 > 0.0 {
            (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy)
    };
    let inside = |poly: &[(f64, f64)]| {
        let n = poly.len();
        let s: Vec<f64> = (0..n)
            .map(|i| {
                let (a, b) = (poly[i], poly[(i + 1) % n]);
                (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0)
            })
            .collect();
        s.iter().all(|&x| x >= 0.0) || s.iter().all(|&x| x <= 0.0)
    };
    let edges = |poly: &[(f64, f64)]| {
        if inside(poly) {
            return 0.0;
        }
        (0..poly.len())
            .map(|i| seg(poly[i], poly[(i + 1) % poly.len()]))
            .fold(f64::INFINITY, f64::min)
    };
    match &shape.geometry {
        HandleGeometry::Arrow {
            shaft_start,
            shaft_end,
            head,
            ..
        } => seg(*shaft_start, *shaft_end).min(edges(head)),
        HandleGeometry::Ring { points, front } => (0..points.len())
            .filter(|&i| front[i] || front[(i + 1) % points.len()])
            .map(|i| seg(points[i], points[(i + 1) % points.len()]))
            .fold(f64::INFINITY, f64::min),
        HandleGeometry::RingGrip { arc, heads } => arc
            .windows(2)
            .map(|w| seg(w[0], w[1]))
            .chain(heads.iter().map(|h| edges(h)))
            .fold(f64::INFINITY, f64::min),
        HandleGeometry::Circle { center, radius_px } => {
            ((p.0 - center.0).hypot(p.1 - center.1) - radius_px).abs()
        }
        HandleGeometry::Plane { corners } => edges(corners),
        HandleGeometry::Point { center, radius_px } => {
            ((p.0 - center.0).hypot(p.1 - center.1) - radius_px).max(0.0)
        }
    }
}

/// Draw/hit parity (Task 4's contract, kept with the grips and the
/// hover-only view ring): sweeping every 3 px around the gizmo in each
/// mode from several views, wherever `hit` finds a handle, `shapes` with
/// that handle highlighted (what the view draws once the pointer rests
/// there) draws that handle within the 8 pt hit tolerance of the pointer.
#[test]
fn every_hit_lands_on_a_drawn_shape_of_that_handle() {
    let pivot = DVec3::new(0.5, -1.0, 2.0);
    let cams = [
        test_camera(),
        camera_at(30.0, 35.0),
        camera_at(-120.0, 20.0),
        camera_at(-30.0, 0.0),
        camera_at(10.0, 89.0),
    ];
    for mode in [GizmoMode::Move, GizmoMode::Rotate, GizmoMode::Scale] {
        let gizmo = Gizmo {
            pivot,
            mode,
            size_pt: 110.0,
        };
        for cam in cams {
            let mut cam = cam;
            cam.target = pivot;
            let centre = screen_of(&cam, pivot, pivot);
            let reach = 1.5 * gizmo.size_pt;
            let mut hits = 0;
            let mut y = centre.1 - reach;
            while y <= centre.1 + reach {
                let mut x = centre.0 - reach;
                while x <= centre.0 + reach {
                    if let Some(h) = gizmo.hit(&cam, VIEW, (x, y)) {
                        hits += 1;
                        let d = gizmo
                            .shapes(&cam, VIEW, Some(h))
                            .iter()
                            .filter(|s| s.handle == h)
                            .map(|s| drawn_distance((x, y), s))
                            .fold(f64::INFINITY, f64::min);
                        assert!(
                            d <= 8.0 + 1e-9,
                            "{mode:?}: {h:?} hit at ({x:.0}, {y:.0}) but drawn {d:.1} px away"
                        );
                    }
                    x += 3.0;
                }
                y += 3.0;
            }
            assert!(hits > 100, "{mode:?}: the sweep found handles ({hits})");
        }
    }
}

/// From any view, the three grips stay apart (no two on top of each
/// other) and each sits on its ring's drawn half.
#[test]
fn grips_never_overlap_from_any_view() {
    let gizmo = rotate_gizmo(DVec3::ZERO);
    for yaw in (0..360).step_by(15) {
        for pitch in (-75..=89).step_by(16) {
            let cam = camera_at(f64::from(yaw), f64::from(pitch));
            let shapes = gizmo.handles(&cam, VIEW);
            let gs = grips(&shapes);
            assert_eq!(gs.len(), 3, "yaw {yaw} pitch {pitch}");
            for (a, &(i, pa, _)) in gs.iter().enumerate() {
                for &(j, pb, _) in &gs[a + 1..] {
                    let d = (pa.0 - pb.0).hypot(pa.1 - pb.1);
                    assert!(
                        d >= mf_render::GRIP_SEPARATION_PX - 1e-6,
                        "yaw {yaw} pitch {pitch}: grips {i} and {j} {d:.1} px apart"
                    );
                }
                let ring = shapes.iter().find(|s| s.handle == Handle::Ring(i)).unwrap();
                let HandleGeometry::Ring { points, front } = &ring.geometry else {
                    panic!("a ring");
                };
                let off = points
                    .iter()
                    .zip(front)
                    .filter(|(_, f)| **f)
                    .map(|(q, _)| (pa.0 - q.0).hypot(pa.1 - q.1))
                    .fold(f64::INFINITY, f64::min);
                assert!(
                    off < 8.0,
                    "yaw {yaw} pitch {pitch}: grip {i} {off:.1} px off its ring"
                );
            }
        }
    }
}
