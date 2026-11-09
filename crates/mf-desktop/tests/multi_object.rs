#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Transform-aware rendering and picking (MF-ISSUE-042): a document with
//! several objects draws and picks every one at its own 3MF transform.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use mf_desktop::MeshFixerApp;
use mf_fixtures::{cube, tetrahedron};
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};

fn wait_until(h: &mut Harness<'_, MeshFixerApp>, what: &str, cond: impl Fn(&MeshFixerApp) -> bool) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for: {what}"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
}

/// A 3MF with two unit cubes translated to (-3, 0, 0) and (3, 0, 0), far
/// enough apart that their screen projections don't overlap.
fn two_translated_cubes(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("two_cubes.3mf");
    let translate = |x: f64| {
        let mut t = IDENTITY;
        t[9] = x;
        t
    };
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![
            ModelObject {
                name: "left".into(),
                mesh: cube(1.0),
                transform: translate(-3.0),
            },
            ModelObject {
                name: "right".into(),
                mesh: cube(1.0),
                transform: translate(3.0),
            },
        ],
    };
    mf_io::write_model_path(&model, &path).unwrap();
    path
}

#[test]
fn two_objects_both_visible_and_clicking_one_activates_it() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable(
            "two_objects_both_visible_and_clicking_one_activates_it",
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = two_translated_cubes(dir.path());

    let mut h = Harness::builder()
        .with_size((1440.0, 900.0))
        .wgpu()
        .build_eframe(|cc| MeshFixerApp::new(cc, Some(path.clone())).unwrap());
    wait_until(&mut h, "import + analysis", |a| {
        a.session.document().objects().len() == 2 && a.current_analysis().is_some()
    });
    h.run();

    let (first_id, second_id) = {
        let objects = h.state().session.document().objects().to_vec();
        (objects[0].id, objects[1].id)
    };
    assert_eq!(
        h.state().active,
        Some(first_id),
        "the first object imported is active"
    );

    // Both objects must actually be drawn, not just reachable through
    // camera maths and CPU picking: read back the rendered window at each
    // cube's projected centre and at the empty gap between them.
    let world_center_of_first = [-2.5, 0.5, 0.5]; // cube centre + translation (-3,0,0)
    let world_center_of_second = [3.5, 0.5, 0.5]; // cube centre + translation (3,0,0)
    let world_gap = [0.5, 0.5, 0.5];
    let pos_first = h.state().screen_point(world_center_of_first).unwrap();
    let pos_second = h.state().screen_point(world_center_of_second).unwrap();
    let pos_gap = h.state().screen_point(world_gap).unwrap();
    let frame = h.render().unwrap();
    let pixel_at = |p: egui::Pos2| frame.get_pixel(p.x as u32, p.y as u32).0;
    assert_ne!(
        pixel_at(pos_first),
        mf_desktop::theme::VIEWPORT_BACKGROUND.to_array(),
        "the active (first) object must be drawn"
    );
    assert_ne!(
        pixel_at(pos_second),
        mf_desktop::theme::VIEWPORT_BACKGROUND.to_array(),
        "the inactive (second) object must also be drawn, not skipped"
    );
    assert_eq!(
        pixel_at(pos_gap),
        mf_desktop::theme::VIEWPORT_BACKGROUND.to_array(),
        "the gap between the cubes must stay background"
    );

    // Select something on the active (first) object, so we can observe it
    // getting cleared when another object becomes active.
    h.state_mut().selection = Some(mf_selection::Selection {
        object: first_id,
        revision: h.state().session.current(),
        kind: mf_selection::SelectionKind::Faces,
        ids: vec![0],
    });
    h.run_steps(2);

    // Click where the second (right) object's transformed centre projects,
    // since it's an inactive object in the untransformed-viewport bug this
    // test guards against.
    let pos = h
        .state()
        .screen_point(world_center_of_second)
        .expect("the inactive object's world point must still project to a screen position");
    h.event(egui::Event::PointerMoved(pos));
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });

    wait_until(&mut h, "click activates the second object", |a| {
        a.active == Some(second_id)
    });
    // The click landed on a face of the second object, so it now has its
    // own (new) selection; the first object's old selection is gone either
    // way, since a selection belongs to exactly one object.
    assert_ne!(
        h.state().selection.as_ref().map(|s| s.object),
        Some(first_id),
        "activating another object clears the first object's selection: {:?}",
        h.state().selection
    );
}

/// Two objects with different geometry (so stale vertices are obviously
/// wrong, not just a different transform of the same shape): a cube (12
/// triangles) and a tetrahedron (4 triangles).
fn cube_and_tetrahedron(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("cube_and_tetrahedron.3mf");
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![
            ModelObject {
                name: "cube".into(),
                mesh: cube(1.0),
                transform: IDENTITY,
            },
            ModelObject {
                name: "tetrahedron".into(),
                mesh: tetrahedron(),
                transform: IDENTITY,
            },
        ],
    };
    mf_io::write_model_path(&model, &path).unwrap();
    path
}

/// Switching the active object before its own analysis has returned must
/// never draw the previous active object's geometry at the new one's
/// transform (MF-ISSUE-042 review I4): the "active" slot in `object_views`
/// either belongs to the newly active object, or doesn't exist yet.
#[test]
fn switching_active_object_never_shows_the_previous_objects_geometry() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable(
            "switching_active_object_never_shows_the_previous_objects_geometry",
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = cube_and_tetrahedron(dir.path());

    let mut h = Harness::builder()
        .with_size((1440.0, 900.0))
        .wgpu()
        .build_eframe(|cc| MeshFixerApp::new(cc, Some(path.clone())).unwrap());
    wait_until(&mut h, "import + analysis", |a| {
        a.session.document().objects().len() == 2 && a.current_analysis().is_some()
    });
    h.run();

    let (cube_id, tet_id) = {
        let objects = h.state().session.document().objects().to_vec();
        (objects[0].id, objects[1].id)
    };
    assert_eq!(h.state().active, Some(cube_id), "the cube imports first");
    let cube_triangles = h
        .state()
        .session
        .document()
        .object(cube_id)
        .unwrap()
        .mesh()
        .unwrap()
        .triangle_count();
    let tet_triangles = h
        .state()
        .session
        .document()
        .object(tet_id)
        .unwrap()
        .mesh()
        .unwrap()
        .triangle_count();
    assert_ne!(
        cube_triangles, tet_triangles,
        "the two meshes must be distinguishable by triangle count"
    );

    // Switch to the tetrahedron, which has never been analysed, then look
    // at `object_views` before giving the background worker any chance to
    // run: this is exactly the window the bug lived in.
    h.state_mut().set_active(tet_id);
    let views = h.state().object_views();
    if let Some(active_view) = views.iter().find(|v| v.active) {
        assert_eq!(
            active_view.id, tet_id,
            "the active slot must belong to the newly active object"
        );
        assert_eq!(
            active_view.render.gpu.triangle_count, tet_triangles,
            "the active slot must show the tetrahedron's own geometry, not the cube's \
             left over from before the switch"
        );
    }
    // Either way, the cube itself (now inactive) must never claim to be
    // the active one.
    assert!(
        views.iter().all(|v| v.id != cube_id || !v.active),
        "the previous active object must not still report as active"
    );
}

/// A single cube translated by `d`, written as a 3MF with one object.
fn single_translated_cube(dir: &std::path::Path, name: &str, d: [f64; 3]) -> std::path::PathBuf {
    let path = dir.join(name);
    let mut t = IDENTITY;
    t[9] = d[0];
    t[10] = d[1];
    t[11] = d[2];
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![ModelObject {
            name: "cube".into(),
            mesh: cube(1.0),
            transform: t,
        }],
    };
    mf_io::write_model_path(&model, &path).unwrap();
    path
}

/// Importing into a document that already has an object must refit the
/// camera to the union of both, not keep framing only the first one
/// (MF-ISSUE-042 review I5): otherwise the newly imported object can land
/// off screen.
#[test]
fn importing_into_a_non_empty_document_reframes_the_camera() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable(
            "importing_into_a_non_empty_document_reframes_the_camera",
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let first = single_translated_cube(dir.path(), "first.3mf", [0.0, 0.0, 0.0]);
    let far = [1000.0, 1000.0, 1000.0];
    let second = single_translated_cube(dir.path(), "second.3mf", far);

    let mut h = Harness::builder()
        .with_size((1440.0, 900.0))
        .wgpu()
        .build_eframe(|cc| MeshFixerApp::new(cc, Some(first.clone())).unwrap());
    wait_until(&mut h, "first import", |a| a.current_analysis().is_some());
    h.run();
    let radius_before = h.state().camera.unwrap().radius;
    // Fit to a single unit cube: a small radius (about sqrt(3)/2 before the
    // 1.1x margin `OrbitCamera::fit` applies).
    assert!(radius_before < 2.0, "{radius_before}");

    h.state_mut().open(second);
    wait_until(&mut h, "second import", |a| {
        a.session.document().objects().len() == 2
    });
    h.run();

    let cam = h.state().camera.expect("a camera exists after import");
    // The union of a cube at the origin and one translated by `far` has a
    // diagonal of roughly |far|, so the radius must have grown to match,
    // not stayed at the first cube's own small size.
    let expected_radius = (far[0] * far[0] + far[1] * far[1] + far[2] * far[2]).sqrt() * 0.5;
    assert!(
        cam.radius > expected_radius * 0.9,
        "camera did not refit to the union of both objects: radius {} vs expected ~{}",
        cam.radius,
        expected_radius
    );

    // The newly imported (now active) object's own centre must project
    // inside the viewport, not off screen.
    let far_center = [far[0] + 0.5, far[1] + 0.5, far[2] + 0.5];
    let rect = h.state().viewport.last_rect.unwrap();
    let pos = h
        .state()
        .screen_point(far_center)
        .expect("the far cube's centre must project somewhere");
    assert!(
        rect.contains(pos),
        "the newly imported object must be framed on screen: {pos:?} not in {rect:?}"
    );
}
