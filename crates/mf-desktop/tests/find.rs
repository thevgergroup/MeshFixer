#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Finding any item from the side panel (M4 Task 8a, Checkpoint B
//! follow-up): every row of "Objects & Defects" behaves the same way.
//! Single click selects (the camera stays); double-click (or Enter) selects
//! and frames it with a 200 ms eased camera move, turning to face it from
//! outside the model, then pulses the selection cue; hovering a row
//! highlights the item in the view. A selected item hidden behind other
//! geometry is drawn through it (x-ray), and rows give each item's size.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use glam::DVec3;
use mf_command::ObjectId;
use mf_desktop::MeshFixerApp;
use mf_desktop::defects::{Defect, defect_copy};
use mf_geometry::{ContentHash, Mesh};
use mf_io::threemf::{IDENTITY, ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_render::{FaceStates, OrbitCamera, model_matrix};
use mf_selection::SelectionKind;

const WINDOW: (f32, f32) = (1440.0, 900.0);

macro_rules! require_gpu {
    ($name:literal) => {
        if !mf_render::offscreen::gpu_available() {
            mf_fixtures::skip::gpu_unavailable($name);
            return;
        }
    };
}

fn wait_until(h: &mut Harness<'_, MeshFixerApp>, what: &str, cond: impl Fn(&MeshFixerApp) -> bool) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "timed out waiting for: {what}"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn model(objects: Vec<(&str, Mesh, [f64; 12])>) -> ImportedModel {
    ImportedModel {
        objects: objects
            .into_iter()
            .map(|(name, mesh, transform)| ModelObject {
                metadata: Default::default(),
                name: name.into(),
                mesh,
                transform,
            })
            .collect(),
        unit: Unit::Millimeter,
        source: SourceInfo {
            path: "find.3mf".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
        },
        warnings: vec![],
    }
}

/// The app with `model` open: analysed, shown, ready to pick, every
/// object drawn, and settled (so the accessibility check runs).
fn opened(model: ImportedModel) -> Harness<'static, MeshFixerApp> {
    let mut h = Harness::builder()
        .with_size(WINDOW)
        // Frames at 60 Hz of input time (the harness's default is 4 Hz):
        // double-clicks and the 200 ms camera move are timed in it.
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(|cc| MeshFixerApp::new(cc, None).unwrap());
    let objects = model.objects.len();
    h.state_mut().import_model(model);
    wait_until(&mut h, "analysis, BVH and every object drawn", |a| {
        a.current_analysis().is_some()
            && a.shows_current_revision()
            && a.picking_ready()
            && a.object_views().len() == objects
            && a.camera.is_some()
    });
    h.run();
    // Nothing under the pointer.
    h.event(egui::Event::PointerGone);
    h.run();
    h
}

fn press(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2, pressed: bool) {
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

fn click_at(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    press(h, pos, true);
    press(h, pos, false);
}

/// Two press/release pairs at `pos`, well inside egui's double-click
/// window (each step is 1/60 s of input time).
fn double_click_at(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2) {
    click_at(h, pos);
    press(h, pos, true);
    press(h, pos, false);
}

fn row_pos(h: &Harness<'_, MeshFixerApp>, label: &str) -> egui::Pos2 {
    h.get_all_by_label(label)
        .next()
        .unwrap_or_else(|| panic!("tree row {label:?}"))
        .rect()
        .center()
}

fn double_click_row(h: &mut Harness<'_, MeshFixerApp>, label: &str) {
    let pos = row_pos(h, label);
    double_click_at(h, pos);
}

/// The input time of the frame just run (seconds).
fn now(h: &Harness<'_, MeshFixerApp>) -> f64 {
    h.ctx.input(|i| i.time)
}

/// Steps until the camera move finishes; returns every camera seen and
/// the largest selection emphasis (the pulse) on the way, through a short
/// settle after it (the pulse follows the move).
///
/// A framing first waits for what it needs (picking, built on the
/// background pool): that wait is in wall time, like the work and the
/// app's own give-up ([`mf_desktop::app::FIND_GIVE_UP_SECONDS`]). The move
/// itself is in input time (200 ms, 12 steps at 60 Hz), so its steps are
/// counted.
fn finish_move(h: &mut Harness<'_, MeshFixerApp>) -> (Vec<OrbitCamera>, f32) {
    let mut cameras = vec![h.state().camera.unwrap()];
    let mut pulse = h.state().selection_emphasis();
    let waiting = Instant::now();
    while h.state().framing_pending() {
        assert!(
            waiting.elapsed() < Duration::from_secs(30),
            "the framing never started"
        );
        h.step();
        cameras.push(h.state().camera.unwrap());
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut steps = 0;
    while h.state().camera_animating() || steps < 4 {
        steps += 1;
        assert!(steps < 600, "the camera move never ended");
        h.step();
        cameras.push(h.state().camera.unwrap());
        pulse = pulse.max(h.state().selection_emphasis());
    }
    for _ in 0..60 {
        h.step();
        pulse = pulse.max(h.state().selection_emphasis());
    }
    (cameras, pulse)
}

fn row_of(app: &MeshFixerApp, d: &Defect) -> String {
    defect_copy(d, app.session.document().units()).row
}

/// Every point of `defect` on the active object, in world space.
fn defect_points(app: &MeshFixerApp, defect: &Defect) -> Vec<DVec3> {
    let object = app.session.document().object(app.active.unwrap()).unwrap();
    let mesh = object.mesh().unwrap();
    let model = model_matrix(&object.transform);
    let topo = mesh.topology();
    let vertices: Vec<u32> = match defect {
        Defect::Hole { loop_id, .. }
        | Defect::OverCap { loop_id, .. }
        | Defect::Crack { loop_id, .. } => topo
            .boundary_loops
            .iter()
            .find(|l| l.id == *loop_id)
            .unwrap()
            .vertices
            .clone(),
        Defect::LoosePiece { component, .. } => topo.components[*component as usize]
            .iter()
            .flat_map(|&f| mesh.face(mf_geometry::FaceId(f)).unwrap())
            .collect(),
        Defect::Tangled { edge, .. } => edge.to_vec(),
    };
    vertices
        .into_iter()
        .map(|v| model.transform_point3(DVec3::from(mesh.corner_position(v))))
        .collect()
}

/// Every vertex of object `id`, in world space.
fn object_points(app: &MeshFixerApp, id: ObjectId) -> Vec<DVec3> {
    let object = app.session.document().object(id).unwrap();
    let model = model_matrix(&object.transform);
    object
        .mesh()
        .unwrap()
        .vertices()
        .map(|(_, p)| model.transform_point3(DVec3::from(p)))
        .collect()
}

fn centre(points: &[DVec3]) -> DVec3 {
    let lo = points.iter().fold(DVec3::splat(f64::MAX), |a, p| a.min(*p));
    let hi = points.iter().fold(DVec3::splat(f64::MIN), |a, p| a.max(*p));
    (lo + hi) * 0.5
}

/// Where the corners of `points`' bounding box land on screen: its
/// centre's distance from the view centre and its larger screen extent,
/// both as fractions of the view height.
fn on_screen(app: &MeshFixerApp, points: &[DVec3]) -> (f32, f32) {
    let view = app.viewport.last_rect.unwrap();
    let lo = points.iter().fold(DVec3::splat(f64::MAX), |a, p| a.min(*p));
    let hi = points.iter().fold(DVec3::splat(f64::MIN), |a, p| a.max(*p));
    let corners: Vec<egui::Pos2> = (0..8)
        .map(|i| {
            let pick = |k: usize| if i & (1 << k) == 0 { lo[k] } else { hi[k] };
            app.screen_point([pick(0), pick(1), pick(2)]).unwrap()
        })
        .collect();
    let rect = egui::Rect::from_points(&corners);
    let c = app.screen_point(centre(points).to_array()).unwrap();
    (
        c.distance(view.center()) / view.height(),
        rect.width().max(rect.height()) / view.height(),
    )
}

/// A one-triangle loose piece, 0.2 mm across, at `at`.
fn speck(at: [f64; 3]) -> Mesh {
    Mesh::new(
        vec![
            at,
            [at[0] + 0.2, at[1], at[2]],
            [at[0], at[1] + 0.1, at[2] + 0.15],
        ],
        vec![[0, 1, 2]],
    )
    .unwrap()
}

/// A 40 mm closed box with its +y wall missing: the hole is on the far
/// side from the default view (which looks from −y, above).
fn box_open_at_the_back() -> Mesh {
    let (positions, triangles) = mf_fixtures::cube(40.0).to_arrays();
    let kept = triangles
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 6 && *i != 7)
        .map(|(_, t)| *t)
        .collect();
    Mesh::new(positions, kept).unwrap()
}

/// Selection of a single item of `kind`.
fn selected(app: &MeshFixerApp) -> Option<(SelectionKind, Vec<u32>)> {
    app.selection.as_ref().map(|s| (s.kind, s.ids.clone()))
}

#[test]
fn double_click_frames_and_selects_a_loose_piece() {
    require_gpu!("double_click_frames_and_selects_a_loose_piece");
    // A 40 mm plate and, off its corner, a one-triangle speck: invisible at
    // the default view.
    let plate = mf_fixtures::scaled(&mf_fixtures::grid(10), 4.0);
    let mut h = opened(model(vec![(
        "plate and speck",
        mf_fixtures::merge(&[plate, speck([46.0, 30.0, 2.0])]),
        IDENTITY,
    )]));
    let piece = h.state().current_analysis().unwrap().defects.loose[0].clone();
    let points = defect_points(h.state(), &piece);
    let (_, before) = on_screen(h.state(), &points);
    assert!(before < 0.02, "a speck at first: {before}");

    let label = row_of(h.state(), &piece);
    double_click_row(&mut h, &label);
    assert_eq!(
        selected(h.state()),
        Some((SelectionKind::Component, vec![1])),
        "double-click selects, as a click does"
    );
    let (cameras, pulse) = finish_move(&mut h);
    assert!(cameras.windows(2).any(|w| w[0] != w[1]), "the camera moved");
    let (off_centre, size) = on_screen(h.state(), &points);
    assert!(
        off_centre < 0.05,
        "the piece is at the view centre: {off_centre}"
    );
    assert!(
        size >= 0.25,
        "the piece fills a good part of the view: {size}"
    );
    assert!(size <= 0.9, "not so close it fills the view: {size}");
    assert!(
        pulse > 0.5,
        "the selection cue pulses after framing: {pulse}"
    );
    // And it's drawn in the selection colour.
    let faces = h.state().active_mesh().unwrap().topology().components[1].clone();
    wait_until(&mut h, "selection tint", |a| {
        faces
            .iter()
            .all(|&f| a.read_face_state(f).unwrap().unwrap() & FaceStates::SELECTED != 0)
    });
}

#[test]
fn double_click_on_a_far_side_hole_faces_it_from_outside() {
    require_gpu!("double_click_on_a_far_side_hole_faces_it_from_outside");
    let mut h = opened(model(vec![("box", box_open_at_the_back(), IDENTITY)]));
    let eye = h.state().camera.unwrap().eye();
    assert!(eye.y < 0.0, "the default view looks from the front: {eye}");
    let hole = h.state().current_analysis().unwrap().defects.holes[0].clone();
    let points = defect_points(h.state(), &hole);
    assert!(
        points.iter().all(|p| (p.y - 40.0).abs() < 1e-9),
        "the hole is the back wall"
    );

    let label = row_of(h.state(), &hole);
    double_click_row(&mut h, &label);
    let Defect::Hole { loop_id, .. } = hole else {
        panic!("a fillable hole: {hole:?}");
    };
    assert_eq!(
        selected(h.state()),
        Some((SelectionKind::BoundaryLoop, vec![loop_id]))
    );
    finish_move(&mut h);
    let cam = h.state().camera.unwrap();
    let eye = cam.eye();
    assert!(
        eye.y > 40.0,
        "the camera turned to look at the hole from outside the box: eye {eye}"
    );
    let towards = (eye - centre(&points)).normalize();
    assert!(towards.y > 0.5, "looking into the opening: {towards}");
    let (off_centre, size) = on_screen(h.state(), &points);
    assert!(off_centre < 0.05, "{off_centre}");
    assert!(size >= 0.25, "{size}");
}

#[test]
fn single_click_selects_without_moving_the_camera() {
    require_gpu!("single_click_selects_without_moving_the_camera");
    let plate = mf_fixtures::scaled(&mf_fixtures::grid(10), 4.0);
    let mut h = opened(model(vec![(
        "plate and speck",
        mf_fixtures::merge(&[plate, speck([46.0, 30.0, 2.0])]),
        IDENTITY,
    )]));
    let piece = h.state().current_analysis().unwrap().defects.loose[0].clone();
    let before = h.state().camera.unwrap();
    let pos = row_pos(&h, &row_of(h.state(), &piece));
    click_at(&mut h, pos);
    assert_eq!(
        selected(h.state()),
        Some((SelectionKind::Component, vec![1]))
    );
    let mut pulse = 0.0f32;
    // Longer than a double-click window and a camera move.
    for _ in 0..60 {
        h.step();
        pulse = pulse.max(h.state().selection_emphasis());
        assert!(!h.state().camera_animating());
    }
    assert_eq!(h.state().camera.unwrap(), before, "the camera stays");
    assert_eq!(pulse, 0.0, "no pulse on an ordinary click");
}

#[test]
fn hovering_a_row_highlights_it_in_the_view() {
    require_gpu!("hovering_a_row_highlights_it_in_the_view");
    let cube = mf_fixtures::translated(&mf_fixtures::cube(4.0), [14.0, 3.0, 0.0]);
    let mut h = opened(model(vec![(
        "plate and cube",
        mf_fixtures::merge(&[mf_fixtures::plate_with_defects(), cube]),
        IDENTITY,
    )]));
    let before = h.state().camera.unwrap();
    let defects = h.state().current_analysis().unwrap().defects.clone();

    // A loose piece glows (the hover face state)…
    let piece = defects.loose[0].clone();
    let faces = h.state().active_mesh().unwrap().topology().components[1].clone();
    let pos = row_pos(&h, &row_of(h.state(), &piece));
    h.event(egui::Event::PointerMoved(pos));
    wait_until(&mut h, "piece hover glow", |a| {
        faces
            .iter()
            .all(|&f| a.read_face_state(f).unwrap().unwrap() & FaceStates::HOVER != 0)
    });
    // …a hole is outlined in the hover colour.
    let hole = defects
        .holes
        .iter()
        .find(|d| matches!(d, Defect::Hole { edges: 4, .. }))
        .unwrap()
        .clone();
    let pos = row_pos(&h, &row_of(h.state(), &hole));
    h.event(egui::Event::PointerMoved(pos));
    wait_until(&mut h, "hole hover outline", |a| {
        a.view_layers()
            .iter()
            .any(|l| l.color == mf_desktop::theme::HOVER)
            && faces
                .iter()
                .all(|&f| a.read_face_state(f).unwrap().unwrap() & FaceStates::HOVER == 0)
    });
    assert!(h.state().selection.is_none(), "hovering selects nothing");
    assert_eq!(h.state().camera.unwrap(), before, "hovering moves nothing");
}

/// The cyan of the selection, whatever the lighting and blending: blue and
/// green well above red.
fn cyan(p: [i32; 3]) -> bool {
    p[2] * 10 > p[0] * 16 && p[1] * 10 > p[0] * 14
}

fn pixel(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2) -> [i32; 3] {
    h.event(egui::Event::PointerGone);
    h.run_steps(3);
    let image = h.render().unwrap();
    let p = image.get_pixel(pos.x as u32, pos.y as u32).0;
    [0, 1, 2].map(|k| i32::from(p[k]))
}

#[test]
fn hidden_selection_is_drawn_through_the_surface() {
    require_gpu!("hidden_selection_is_drawn_through_the_surface");
    // A closed 40 mm box with a 4 mm loose cube shut inside it.
    let inner = mf_fixtures::translated(&mf_fixtures::cube(4.0), [18.0, 18.0, 18.0]);
    let mut h = opened(model(vec![(
        "box with a piece inside",
        mf_fixtures::merge(&[mf_fixtures::cube(40.0), inner]),
        IDENTITY,
    )]));
    let piece = h.state().current_analysis().unwrap().defects.loose[0].clone();
    let at = h.state().screen_point([20.0, 20.0, 20.0]).unwrap();
    let hidden = pixel(&mut h, at);
    assert!(!cyan(hidden), "the box hides the piece: {hidden:?}");

    let pos = row_pos(&h, &row_of(h.state(), &piece));
    click_at(&mut h, pos);
    assert_eq!(
        selected(h.state()),
        Some((SelectionKind::Component, vec![1]))
    );
    let faces = h.state().active_mesh().unwrap().topology().components[1].clone();
    wait_until(&mut h, "selection state", |a| {
        faces
            .iter()
            .all(|&f| a.read_face_state(f).unwrap().unwrap() & FaceStates::SELECTED != 0)
    });
    assert_eq!(
        h.state().screen_point([20.0, 20.0, 20.0]).unwrap(),
        at,
        "a single click doesn't move the camera"
    );
    let through = pixel(&mut h, at);
    assert!(
        cyan(through),
        "the selected piece shows through the box in the selection colour: {through:?} (was {hidden:?})"
    );
    // Beside the piece, the box looks as before.
    let beside = h.state().screen_point([20.0, 20.0, 30.0]).unwrap();
    let p = pixel(&mut h, beside);
    assert!(!cyan(p), "only the piece shows through: {p:?}");
}

#[test]
fn framing_animation_takes_about_200ms_and_does_no_geometry_work() {
    require_gpu!("framing_animation_takes_about_200ms_and_does_no_geometry_work");
    let plate = mf_fixtures::scaled(&mf_fixtures::grid(10), 4.0);
    let mut h = opened(model(vec![(
        "plate and speck",
        mf_fixtures::merge(&[plate, speck([46.0, 30.0, 2.0])]),
        IDENTITY,
    )]));
    let shown = |app: &MeshFixerApp| {
        app.object_views()
            .into_iter()
            .find(|v| v.active)
            .unwrap()
            .render
    };
    let render = shown(h.state());
    let (uploaded, total) = (
        h.state().geometry_bytes_uploaded(),
        render.gpu.total_uploaded(),
    );
    let piece = h.state().current_analysis().unwrap().defects.loose[0].clone();
    let start = h.state().camera.unwrap();
    let label = row_of(h.state(), &piece);
    double_click_row(&mut h, &label);
    let clicked = now(&h);

    // Step until the camera stops; note when it started and ended.
    let mut moving_since = None;
    let mut last = h.state().camera.unwrap();
    let mut ended = None;
    let mut path = vec![last];
    for _ in 0..120 {
        h.step();
        let cam = h.state().camera.unwrap();
        if cam != last {
            moving_since.get_or_insert(now(&h));
            path.push(cam);
        } else if moving_since.is_some() && !h.state().camera_animating() {
            ended = Some(now(&h));
            break;
        }
        last = cam;
    }
    let ended = ended.expect("the move ended");
    let took = ended - clicked;
    assert!(
        (0.15..=0.30).contains(&took),
        "the move takes about 200 ms of frames: {took:.3} s"
    );
    assert!(
        path.len() >= 6,
        "it's animated, not a jump: {} cameras",
        path.len()
    );
    // Eased out: the first step covers more ground than the last.
    let d = |a: &OrbitCamera, b: &OrbitCamera| (a.target - b.target).length();
    let end = *path.last().unwrap();
    assert!(d(&path[0], &path[1]) > d(&path[path.len() - 2], &end));
    assert_ne!(start, end);

    // No geometry work: the same render mesh, nothing uploaded.
    assert!(std::sync::Arc::ptr_eq(&render, &shown(h.state())));
    assert_eq!(render.gpu.total_uploaded(), total);
    assert_eq!(h.state().geometry_bytes_uploaded(), uploaded);
}

#[test]
fn rows_show_piece_size_and_hole_measurements() {
    require_gpu!("rows_show_piece_size_and_hole_measurements");
    // An open box (a 4-edge hole, 40 mm around) and a one-triangle speck
    // 0.2 mm across, placed at twice their size: rows measure after the
    // transform.
    let open_box = mf_fixtures::scaled(&mf_fixtures::cube_with_missing_face(), 10.0);
    let cube = speck([20.0, 0.0, 0.0]);
    let scale2 = [2.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0];
    let h = opened(model(vec![(
        "box and speck",
        mf_fixtures::merge(&[open_box, cube]),
        scale2,
    )]));
    h.get_by_label("Piece 1 · 1 triangle · 0.400 mm");
    h.get_by_label("Hole 1 · 4 edges · 80.0 mm");
    let defects = h.state().current_analysis().unwrap().defects.clone();
    let copy = defect_copy(&defects.loose[0], Unit::Millimeter);
    assert_eq!(copy.row, "Piece 1 · 1 triangle · 0.400 mm");
    assert!(
        copy.detail.starts_with("1 triangle"),
        "the detail keeps its words: {copy:?}"
    );
    // A 3MF in inches measures in inches.
    let copy = defect_copy(&defects.holes[0], Unit::Inch);
    assert_eq!(copy.row, "Hole 1 · 4 edges · 80.0 in");
}

#[test]
fn double_click_on_an_object_row_frames_that_object_and_activates_it() {
    require_gpu!("double_click_on_an_object_row_frames_that_object_and_activates_it");
    // Two cubes at their own transforms: "near" (active) and "far", small,
    // turned and far away.
    let mut h = opened(model(vec![
        (
            "near",
            mf_fixtures::cube(20.0),
            mf_fixtures::rotation_translation(2, 10.0, [0.0, 0.0, 0.0]),
        ),
        (
            "far",
            mf_fixtures::cube(4.0),
            mf_fixtures::rotation_translation(0, 30.0, [300.0, 120.0, 40.0]),
        ),
    ]));
    let far = h.state().session.document().objects()[1].id;
    assert_ne!(h.state().active, Some(far));
    let points = object_points(h.state(), far);
    let (_, before) = on_screen(h.state(), &points);
    assert!(before < 0.1, "small at first: {before}");

    double_click_row(&mut h, "far");
    assert_eq!(h.state().active, Some(far), "it becomes the active object");
    finish_move(&mut h);
    let (off_centre, size) = on_screen(h.state(), &points);
    assert!(off_centre < 0.05, "{off_centre}");
    assert!(size >= 0.4, "the object fills the view: {size}");
    assert!(size <= 1.2, "all of it shows: {size}");
}

/// What the view shows for a hovered row: the hover glow on every face
/// of `faces`, or a hover outline.
fn hover_shown(app: &MeshFixerApp, object: ObjectId, faces: &[u32]) -> bool {
    if faces.is_empty() {
        return app
            .view_layers()
            .iter()
            .any(|l| l.color == mf_desktop::theme::HOVER);
    }
    faces.iter().all(|&f| {
        app.read_object_face_state(object, f)
            .unwrap()
            .is_some_and(|s| s & FaceStates::HOVER != 0)
    })
}

#[test]
fn every_row_kind_uses_the_shared_row_behaviour() {
    require_gpu!("every_row_kind_uses_the_shared_row_behaviour");
    // "plate": a hole (and the open rim), a narrow crack, a tangled edge
    // and a loose cube; "other": a second object.
    let cube = mf_fixtures::translated(&mf_fixtures::cube(4.0), [50.0, 10.0, 0.0]);
    let mut h = opened(model(vec![
        (
            "plate",
            mf_fixtures::merge(&[mf_fixtures::plate_with_defects(), cube]),
            IDENTITY,
        ),
        (
            "other",
            mf_fixtures::cube(10.0),
            mf_fixtures::rotation_translation(2, 0.0, [-40.0, 0.0, 0.0]),
        ),
    ]));
    let doc = h.state().session.document();
    let (plate, other) = (doc.objects()[0].id, doc.objects()[1].id);
    let defects = h.state().current_analysis().unwrap().defects.clone();
    assert!(!defects.holes.is_empty());
    assert_eq!(defects.cracks.len(), 1);
    assert_eq!(defects.loose.len(), 1);
    assert_eq!(defects.tangled.len(), 1);
    let rows: Vec<Defect> = [
        &defects.holes[..],
        &defects.cracks[..],
        &defects.loose[..],
        &defects.tangled[..],
    ]
    .concat();
    for d in &rows {
        let label = row_of(h.state(), d);
        let faces: Vec<u32> = match d {
            Defect::LoosePiece { component, .. } => {
                h.state().active_mesh().unwrap().topology().components[*component as usize].clone()
            }
            _ => Vec::new(),
        };
        // Hover highlights it.
        let pos = row_pos(&h, &label);
        h.event(egui::Event::PointerMoved(pos));
        wait_until(&mut h, &format!("hover on {label}"), |a| {
            hover_shown(a, plate, &faces)
        });
        // Double-click selects and frames it.
        let before = h.state().camera.unwrap();
        double_click_at(&mut h, pos);
        let targeted = match d.selection() {
            Some((kind, id)) => selected(h.state()) == Some((kind, vec![id])),
            None => h
                .state()
                .current_inspected()
                .is_some_and(|i| i.defect == *d),
        };
        assert!(targeted, "{label} is the target");
        finish_move(&mut h);
        assert_ne!(h.state().camera.unwrap(), before, "{label} framed");
        let (off_centre, _) = on_screen(h.state(), &defect_points(h.state(), d));
        assert!(
            off_centre < 0.05,
            "{label} at the view centre: {off_centre}"
        );
    }

    // "Model": the whole active object. With a defect selected, hovering
    // it lights the object up; double-click targets and frames it.
    let all: Vec<u32> = h
        .state()
        .active_mesh()
        .unwrap()
        .faces()
        .map(|(f, _)| f.0)
        .collect();
    let pos = row_pos(&h, "Model");
    h.event(egui::Event::PointerMoved(pos));
    wait_until(&mut h, "hover on Model", |a| hover_shown(a, plate, &all));
    double_click_at(&mut h, pos);
    assert!(h.state().selection.is_none() && h.state().current_inspected().is_none());
    finish_move(&mut h);
    let (off_centre, _) = on_screen(h.state(), &object_points(h.state(), plate));
    assert!(off_centre < 0.05, "Model at the view centre: {off_centre}");

    // An object row: hovering another object lights it up, double-click
    // activates and frames it.
    let all: Vec<u32> = (0..12).collect();
    let pos = row_pos(&h, "other");
    h.event(egui::Event::PointerMoved(pos));
    wait_until(&mut h, "hover on other", |a| hover_shown(a, other, &all));
    double_click_at(&mut h, pos);
    assert_eq!(h.state().active, Some(other));
    finish_move(&mut h);
    let (off_centre, _) = on_screen(h.state(), &object_points(h.state(), other));
    assert!(off_centre < 0.05, "other at the view centre: {off_centre}");

    // Enter on a focused row does what a double-click does.
    let pos = row_pos(&h, "plate");
    click_at(&mut h, pos);
    assert_eq!(h.state().active, Some(plate));
    wait_until(&mut h, "plate analysed", |a| a.current_analysis().is_some());
    // The tree lists the plate's defects once a double-click's second
    // click could no longer land (half a second).
    h.run_steps(30);
    let piece = h.state().current_analysis().unwrap().defects.loose[0].clone();
    let label = row_of(h.state(), &piece);
    // Move the camera away, focus the row (as Tab would), then Enter.
    h.state_mut().fit_camera();
    h.run_steps(3);
    h.get_by_label(&label).focus();
    h.run_steps(2);
    let before = h.state().camera.unwrap();
    h.event(egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.event(egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    finish_move(&mut h);
    assert_ne!(h.state().camera.unwrap(), before, "Enter frames the row");
    let (off_centre, _) = on_screen(h.state(), &defect_points(h.state(), &piece));
    assert!(
        off_centre < 0.05,
        "Enter: piece at the view centre: {off_centre}"
    );
}

/// A double-click is one row's: a first click on one row and a second on
/// another (the tree moved, or the pointer did) frames nothing.
#[test]
fn a_double_click_across_two_rows_frames_nothing() {
    require_gpu!("a_double_click_across_two_rows_frames_nothing");
    let cube = mf_fixtures::translated(&mf_fixtures::cube(4.0), [50.0, 10.0, 0.0]);
    let mut h = opened(model(vec![(
        "plate",
        mf_fixtures::merge(&[mf_fixtures::plate_with_defects(), cube]),
        IDENTITY,
    )]));
    let defects = h.state().current_analysis().unwrap().defects.clone();
    let first = row_pos(&h, &row_of(h.state(), &defects.holes[0]));
    let second = row_pos(&h, &row_of(h.state(), &defects.loose[0]));
    let before = h.state().camera.unwrap();
    click_at(&mut h, first);
    h.event(egui::Event::PointerMoved(second));
    press(&mut h, second, true);
    press(&mut h, second, false);
    for _ in 0..30 {
        h.step();
        assert!(!h.state().camera_animating());
    }
    assert_eq!(
        selected(h.state()),
        Some((SelectionKind::Component, vec![1])),
        "the second click selects its row"
    );
    assert_eq!(h.state().camera.unwrap(), before, "and frames nothing");
}

/// Fix round 1, I1: switching to another object while the first one is
/// still being analysed (its analysis cancelled mid-topology) must not
/// leave picking waiting forever, with an object's BVH built but its
/// topology never cached: a defect row of the new object still frames
/// when double-clicked.
#[test]
fn switching_objects_mid_analysis_still_lets_a_row_be_framed() {
    require_gpu!("switching_objects_mid_analysis_still_lets_a_row_be_framed");
    // "big": 2M triangles, so its analysis is still running when its
    // picking BVH is built; "box": an open box beside it.
    let open_box = mf_fixtures::scaled(&mf_fixtures::cube_with_missing_face(), 10.0);
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(|cc| MeshFixerApp::new(cc, None).unwrap());
    h.state_mut().import_model(model(vec![
        ("big", mf_fixtures::grid(1000), IDENTITY),
        (
            "box",
            open_box,
            mf_fixtures::rotation_translation(2, 0.0, [1100.0, 0.0, 0.0]),
        ),
    ]));
    let (big, open) = {
        let doc = h.state().session.document();
        (doc.objects()[0].id, doc.objects()[1].id)
    };
    wait_until(&mut h, "the big object's BVH", |a| a.has_picking_bvh(big));
    // Switch while (most likely) its analysis is still computing.
    let busy = h.state().is_analyzing();
    h.state_mut().set_active(open);
    wait_until(&mut h, "the box analysed and drawn", |a| {
        a.current_analysis().is_some() && a.shows_current_revision() && a.camera.is_some()
    });
    h.run_steps(30);
    println!("big object still analysing at the switch: {busy}");
    // Whether or not that switch caught the analysis mid-topology, make
    // sure of the state it can leave: the big object's BVH is built, but
    // its topology isn't cached and nothing is computing it.
    h.state()
        .session
        .document()
        .object(big)
        .unwrap()
        .mesh()
        .unwrap()
        .release_topology();
    assert!(!h.state().picking_ready());
    let hole = h.state().current_analysis().unwrap().defects.holes[0].clone();
    let label = row_of(h.state(), &hole);
    double_click_row(&mut h, &label);
    finish_move(&mut h);
    let (off_centre, size) = on_screen(h.state(), &defect_points(h.state(), &hole));
    assert!(
        off_centre < 0.05 && size >= 0.25,
        "framed: {off_centre} {size}"
    );
}

/// M4 final review I4 (the flaky `switching_objects_mid_analysis_…`): a
/// framing that waits for picking (a topology still being computed on the
/// background pool) waits on wall time, as that work does. Input time
/// runs ahead of it whenever frames come faster than real time (the test
/// harness steps 1/60 s per frame however fast it runs; a busy machine
/// drops frames): it must not make the framing give up before the work
/// had its chance.
#[test]
fn a_framing_waiting_for_picking_does_not_give_up_on_input_time_alone() {
    require_gpu!("a_framing_waiting_for_picking_does_not_give_up_on_input_time_alone");
    let open_box = mf_fixtures::scaled(&mf_fixtures::cube_with_missing_face(), 10.0);
    let mut h = opened(model(vec![
        ("box", open_box, IDENTITY),
        (
            "big",
            mf_fixtures::grid(1000),
            mf_fixtures::rotation_translation(2, 0.0, [100.0, 0.0, 0.0]),
        ),
    ]));
    let big = h.state().session.document().objects()[1].id;
    // The big object's topology is gone, and recomputing it (2M faces)
    // takes far longer than the next few frames.
    h.state()
        .session
        .document()
        .object(big)
        .unwrap()
        .mesh()
        .unwrap()
        .release_topology();
    assert!(!h.state().picking_ready());
    let hole = h.state().current_analysis().unwrap().defects.holes[0].clone();
    let label = row_of(h.state(), &hole);
    double_click_row(&mut h, &label);
    assert!(h.state().framing_pending(), "the framing waits for picking");
    // Input time leaps past the give-up time; hardly any wall time passes.
    let later = now(&h) + mf_desktop::app::FIND_GIVE_UP_SECONDS + 1.0;
    h.input_mut().time = Some(later);
    h.step();
    h.input_mut().time = None;
    assert!(
        h.state().framing_pending() || h.state().camera_animating(),
        "the framing gave up after {:?}",
        h.state().status
    );
    finish_move(&mut h);
    let (off_centre, size) = on_screen(h.state(), &defect_points(h.state(), &hole));
    assert!(
        off_centre < 0.05 && size >= 0.25,
        "framed: {off_centre} {size} ({:?})",
        h.state().status
    );
}

/// The area of the convex hull of `points` (screen space).
fn hull_area(points: &[egui::Pos2]) -> f32 {
    let mut p: Vec<egui::Pos2> = points.to_vec();
    p.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    let cross = |o: egui::Pos2, a: egui::Pos2, b: egui::Pos2| {
        (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x)
    };
    let mut hull: Vec<egui::Pos2> = Vec::new();
    for pass in 0..2 {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &egui::Pos2>> = if pass == 0 {
            Box::new(p.iter())
        } else {
            Box::new(p.iter().rev())
        };
        for &q in iter {
            while hull.len() >= start + 2
                && cross(hull[hull.len() - 2], hull[hull.len() - 1], q) <= 0.0
            {
                hull.pop();
            }
            hull.push(q);
        }
        hull.pop();
    }
    let n = hull.len();
    (0..n)
        .map(|i| {
            let (a, b) = (hull[i], hull[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum::<f32>()
        .abs()
        * 0.5
}

/// Where the corners of `points`' bounding box land for camera `cam` in a
/// view of `rect`.
fn corners_for(cam: &OrbitCamera, rect: egui::Rect, points: &[DVec3]) -> Vec<egui::Pos2> {
    let lo = points.iter().fold(DVec3::splat(f64::MAX), |a, p| a.min(*p));
    let hi = points.iter().fold(DVec3::splat(f64::MIN), |a, p| a.max(*p));
    let size = (rect.width() as f64, rect.height() as f64);
    (0..8)
        .map(|i| {
            let pick = |k: usize| if i & (1 << k) == 0 { lo[k] } else { hi[k] };
            let (x, y) =
                mf_render::screen_position(cam, cam.target, size, [pick(0), pick(1), pick(2)])
                    .unwrap();
            egui::pos2(x as f32, y as f32)
        })
        .collect()
}

/// Fix round 1, I2: an object row double-clicked while the view is edge-on
/// to a flat object turns to show its broad side (its projected area is
/// at least 40% of a face-on view's), keeping "up".
#[test]
fn double_click_on_an_object_row_shows_a_flat_object_face_on() {
    require_gpu!("double_click_on_an_object_row_shows_a_flat_object_face_on");
    // A 100 × 100 × 2 mm slab.
    let slab = [
        100.0, 0.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0,
    ];
    let mut h = opened(model(vec![("slab", mf_fixtures::cube(1.0), slab)]));
    // Look at it edge-on.
    h.state_mut().camera.as_mut().unwrap().pitch = 0.0;
    h.run_steps(3);
    let id = h.state().session.document().objects()[0].id;
    let points = object_points(h.state(), id);
    // One object with nothing to fix: the Browser starts folded away.
    h.get_by_label("Show Browser").click();
    h.run_steps(3);
    double_click_row(&mut h, "slab");
    finish_move(&mut h);
    let cam = h.state().camera.unwrap();
    let rect = h.state().viewport.last_rect.unwrap();
    let mut face_on = cam;
    face_on.look_from(DVec3::Z);
    let shown = hull_area(&corners_for(&cam, rect, &points));
    let best = hull_area(&corners_for(&face_on, rect, &points));
    assert!(
        shown >= 0.4 * best,
        "the broad side shows: {shown} of a face-on {best} (view {})",
        cam.view_dir()
    );
    assert!(cam.view_dir().z > 0.0, "from above: up stays up");
    let (off_centre, _) = on_screen(h.state(), &points);
    assert!(off_centre < 0.05, "{off_centre}");
}

/// Fix round 1, P2: double-clicking an object's row frames the object and
/// doesn't pulse whatever was selected on it.
#[test]
fn double_click_on_an_object_row_does_not_pulse_the_selection() {
    require_gpu!("double_click_on_an_object_row_does_not_pulse_the_selection");
    let mut h = opened(model(vec![(
        "plate",
        mf_fixtures::plate_with_defects(),
        IDENTITY,
    )]));
    let hole = h.state().current_analysis().unwrap().defects.holes[0].clone();
    let pos = row_pos(&h, &row_of(h.state(), &hole));
    click_at(&mut h, pos);
    assert!(h.state().selection.is_some());
    // Somewhere else first, so framing the object moves the camera.
    h.state_mut().camera.as_mut().unwrap().zoom(0.2);
    h.step();
    // Right after the click on the hole's row: egui counts the next two
    // clicks as a triple-click.
    double_click_row(&mut h, "plate");
    let (cameras, pulse) = finish_move(&mut h);
    assert!(cameras.windows(2).any(|w| w[0] != w[1]), "framed");
    assert!(h.state().selection.is_some(), "the selection stays");
    assert_eq!(pulse, 0.0, "no pulse for an object row");
}
