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
