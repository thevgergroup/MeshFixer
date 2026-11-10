#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The Transform tool (MF-OBJ-001, Phase 1 Task 5): the gizmo drags an
//! object (one undo per drag), Esc mid-drag commits nothing, the numeric
//! fields commit with Enter, a sheared file transform keeps its rotation
//! and scale read-only, the defect overlays give way to the axis legend,
//! and Drop to plate lands the object on the plate.
//!
//! With `MF_ARTIFACTS` set, these write Checkpoint P1-A frames there:
//! `transform-move.png`, `transform-rotate.png`, `transform-scale.png`,
//! `transform-mid-drag.png`, `transform-numeric.png`,
//! `transform-drop-before.png`, `transform-drop-after.png`,
//! `transform-sheared.png` and `transform-laptop-1280x720.png`.
//!
//! The two 30M-triangle timing tests are in `scripts/run-tests.sh`'s
//! heavy list: they run on demand only.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::{egui, egui_wgpu};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use mf_desktop::MeshFixerApp;
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_render::{Decomposed, FaceStates, GizmoMode, Handle, HandleGeometry, decompose};

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
    h.run_steps(3);
}

macro_rules! require_gpu {
    ($name:literal) => {
        if !mf_render::offscreen::gpu_available() {
            mf_fixtures::skip::gpu_unavailable($name);
            return;
        }
    };
}

fn save(h: &mut Harness<'_, MeshFixerApp>, name: &str) {
    if let Ok(dir) = std::env::var("MF_ARTIFACTS") {
        let frame = h.render().unwrap();
        frame.save(std::path::Path::new(&dir).join(name)).unwrap();
    }
}

/// A 3MF with one object `name` placed by `transform`.
fn model_file(
    dir: &std::path::Path,
    name: &str,
    mesh: mf_geometry::Mesh,
    transform: [f64; 12],
) -> std::path::PathBuf {
    let path = dir.join(format!("{name}.3mf"));
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![ModelObject {
            name: name.into(),
            mesh,
            transform,
        }],
    };
    mf_io::write_model_path(&model, &path).unwrap();
    path
}

fn open_size(path: std::path::PathBuf, size: (f32, f32)) -> Harness<'static, MeshFixerApp> {
    let mut h = Harness::builder()
        .with_size(size)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::new(cc, Some(path.clone())).unwrap());
    wait_until(&mut h, "import + analysis", |a| {
        a.current_analysis().is_some() && a.viewport.last_rect.is_some()
    });
    h
}

fn open(path: std::path::PathBuf) -> Harness<'static, MeshFixerApp> {
    open_size(path, (1440.0, 900.0))
}

/// Opens Transform with T and waits until its gizmo is drawn.
fn open_transform(h: &mut Harness<'_, MeshFixerApp>) {
    h.key_press(egui::Key::T);
    wait_until(h, "the gizmo", |a| a.transform_gizmo().is_some());
}

fn transform(h: &Harness<'_, MeshFixerApp>) -> [f64; 12] {
    let app = h.state();
    app.session
        .document()
        .object(app.active.unwrap())
        .unwrap()
        .transform
}

fn steps(h: &Harness<'_, MeshFixerApp>) -> usize {
    h.state().session.history_labels().len()
}

/// Where the X arrow's shaft is (window points), its screen direction,
/// and that a press there hits it.
fn x_arrow(h: &Harness<'_, MeshFixerApp>) -> (egui::Pos2, egui::Vec2) {
    let app = h.state();
    let g = app.transform_gizmo().unwrap();
    let cam = app.camera.unwrap();
    let rect = app.viewport.last_rect.unwrap();
    let view = (rect.width() as f64, rect.height() as f64);
    let shape = g
        .handles(&cam, view)
        .into_iter()
        .find(|s| s.handle == Handle::Axis(0))
        .expect("the X arrow is drawn");
    let HandleGeometry::Arrow {
        shaft_start,
        shaft_end,
        ..
    } = shape.geometry
    else {
        panic!("an axis is an arrow");
    };
    let mid = (
        (shaft_start.0 + shaft_end.0) / 2.0,
        (shaft_start.1 + shaft_end.1) / 2.0,
    );
    assert_eq!(g.hit(&cam, view, mid), Some(Handle::Axis(0)));
    let dir = egui::vec2(
        (shaft_end.0 - shaft_start.0) as f32,
        (shaft_end.1 - shaft_start.1) as f32,
    )
    .normalized();
    (
        rect.left_top() + egui::vec2(mid.0 as f32, mid.1 as f32),
        dir,
    )
}

fn press(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

fn move_to(h: &mut Harness<'_, MeshFixerApp>, from: egui::Pos2, to: egui::Pos2) {
    for i in 1..=10 {
        h.event(egui::Event::PointerMoved(
            from + (to - from) * (i as f32 / 10.0),
        ));
        h.step();
    }
}

fn release(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2) {
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

/// A 30 x 10 x 5 mm box, so a turn is plain to see.
fn slab_file(dir: &std::path::Path) -> std::path::PathBuf {
    let (positions, triangles) = mf_fixtures::cube(10.0).to_arrays();
    let stretched = positions
        .iter()
        .map(|p| [p[0] * 3.0, p[1], p[2] * 0.5])
        .collect();
    let mesh = mf_geometry::Mesh::new(stretched, triangles).unwrap();
    model_file(dir, "slab", mesh, IDENTITY)
}

fn cube_file(dir: &std::path::Path) -> std::path::PathBuf {
    model_file(dir, "cube", mf_fixtures::cube(10.0), IDENTITY)
}

#[test]
fn drag_x_arrow_moves_object_and_one_undo_restores_it() {
    require_gpu!("drag_x_arrow_moves_object_and_one_undo_restores_it");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(cube_file(dir.path()));
    // Properties lists Transform first among the whole-object tools.
    assert_eq!(h.state().tools()[0].label, "Transform");
    open_transform(&mut h);
    h.run_steps(3);
    save(&mut h, "transform-move.png");
    let (before, camera, history) = (transform(&h), h.state().camera, steps(&h));
    let revision = h.state().session.current();

    let (at, dir_x) = x_arrow(&h);
    let to = at + dir_x * 100.0;
    press(&mut h, at);
    move_to(&mut h, at, to);
    // Mid-drag only the drawn placement moved: no command yet.
    assert_eq!(steps(&h), history);
    let drawn = h
        .state()
        .shown_transform(h.state().active.unwrap())
        .unwrap();
    assert!(drawn[9] > before[9], "the drag moves the object along X");
    save(&mut h, "transform-mid-drag.png");
    release(&mut h, to);
    wait_until(&mut h, "the move commits", |a| {
        a.busy.is_none() && a.session.history_labels().len() == history + 1
    });

    let after = transform(&h);
    assert!(after[9] > before[9] + 1.0, "{after:?}");
    assert_eq!(after[..9], before[..9], "only the translation changes");
    assert_eq!((after[10], after[11]), (before[10], before[11]));
    assert_eq!(h.state().camera, camera, "the drag didn't orbit the camera");
    let status = h.state().status.clone().unwrap();
    assert!(
        status.starts_with("Moved cube by ") && status.ends_with(", 0, 0 mm"),
        "{status}"
    );
    let record = h
        .state()
        .session
        .node(h.state().session.current())
        .unwrap()
        .record
        .clone()
        .unwrap();
    assert_eq!(record.op_id, "object.set_transform");

    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(3);
    assert_eq!(transform(&h), before, "one undo puts it back");
    assert_eq!(h.state().session.current(), revision);
}

#[test]
fn escape_mid_drag_commits_nothing() {
    require_gpu!("escape_mid_drag_commits_nothing");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(cube_file(dir.path()));
    open_transform(&mut h);
    h.run_steps(3);
    let (before, history) = (transform(&h), steps(&h));
    let (at, dir_x) = x_arrow(&h);
    let to = at + dir_x * 80.0;
    press(&mut h, at);
    move_to(&mut h, at, to);
    assert_ne!(
        h.state().shown_transform(h.state().active.unwrap()),
        None,
        "dragging"
    );
    h.key_press(egui::Key::Escape);
    h.step();
    assert_eq!(
        h.state().shown_transform(h.state().active.unwrap()),
        None,
        "Esc puts the start placement back"
    );
    move_to(&mut h, to, to + dir_x * 20.0);
    release(&mut h, to + dir_x * 20.0);
    h.run_steps(5);
    assert!(h.state().busy.is_none());
    assert_eq!(steps(&h), history, "nothing committed");
    assert_eq!(transform(&h), before);
    assert!(
        h.state().transform_tool().is_some(),
        "the first Esc cancels the drag, not the tool"
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(
        h.state().transform_tool().is_none(),
        "the next Esc leaves it"
    );
}

/// Types `text` into the field labelled `label` and presses Enter.
fn type_into(h: &mut Harness<'_, MeshFixerApp>, label: &str, text: &str) {
    h.get_by_label(label).focus();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_steps(1);
    h.get_by_label(label).type_text(text);
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.step();
}

#[test]
fn numeric_rotation_commits_one_command() {
    require_gpu!("numeric_rotation_commits_one_command");
    let dir = tempfile::tempdir().unwrap();
    // A 30 x 10 x 5 slab: a quarter turn shows.
    let mut h = open(slab_file(dir.path()));
    open_transform(&mut h);
    h.key_press(egui::Key::E);
    h.run_steps(3);
    assert_eq!(h.state().transform_tool().unwrap().mode, GizmoMode::Rotate);
    save(&mut h, "transform-rotate.png");
    let history = steps(&h);
    let centre = h.state().active_placement().unwrap().position;
    type_into(&mut h, "Rotation Z", "90");
    wait_until(&mut h, "the rotation commits", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    assert_eq!(steps(&h), history + 1, "one command");
    match decompose(&transform(&h)) {
        Decomposed::Trs { rotate_deg_xyz, .. } => {
            assert!((rotate_deg_xyz.z - 90.0).abs() < 1e-9, "{rotate_deg_xyz:?}");
        }
        Decomposed::Sheared => panic!("a rotation isn't a shear"),
    }
    let p = h.state().active_placement().unwrap();
    assert!(
        (p.position - centre).length() < 1e-9,
        "it turns about its centre"
    );
    assert_eq!(
        h.state().status.as_deref(),
        Some("Rotated slab 90° about Z")
    );
    h.run_steps(3);
    save(&mut h, "transform-numeric.png");
    // W and R pick the other modes.
    h.key_press(egui::Key::R);
    h.run_steps(3);
    assert_eq!(h.state().transform_tool().unwrap().mode, GizmoMode::Scale);
    save(&mut h, "transform-scale.png");
    h.key_press(egui::Key::W);
    h.run_steps(1);
    assert_eq!(h.state().transform_tool().unwrap().mode, GizmoMode::Move);
}

#[test]
fn sheared_transform_disables_rotate_and_scale_fields() {
    require_gpu!("sheared_transform_disables_rotate_and_scale_fields");
    let dir = tempfile::tempdir().unwrap();
    let mut sheared = IDENTITY;
    sheared[3] = 0.5; // y picks up half of x: a slant, not a rotation or scale
    let path = model_file(dir.path(), "slanted", mf_fixtures::cube(10.0), sheared);
    let mut h = open(path);
    open_transform(&mut h);
    h.run_steps(3);
    h.get_by_label(mf_desktop::transform_tool::SHEARED_NOTE);
    for axis in ["X", "Y", "Z"] {
        for field in ["Rotation", "Scale"] {
            let label = format!("{field} {axis}");
            assert!(
                h.get_by_label(&label).accesskit_node().is_disabled(),
                "{label} is read-only"
            );
        }
        let label = format!("Position {axis}");
        assert!(
            !h.get_by_label(&label).accesskit_node().is_disabled(),
            "{label} still edits"
        );
    }
    assert!(h.get_by_label("Rotate").accesskit_node().is_disabled());
    // Only Move: the gizmo stays a move gizmo.
    h.key_press(egui::Key::E);
    h.run_steps(2);
    assert_eq!(h.state().transform_gizmo().unwrap().mode, GizmoMode::Move);
    save(&mut h, "transform-sheared.png");
    // Its move handles still work.
    let history = steps(&h);
    let (at, dir_x) = x_arrow(&h);
    press(&mut h, at);
    move_to(&mut h, at, at + dir_x * 80.0);
    release(&mut h, at + dir_x * 80.0);
    wait_until(&mut h, "the move commits", |a| {
        a.busy.is_none() && a.session.history_labels().len() == history + 1
    });
    let t = transform(&h);
    assert_eq!(t[..9], sheared[..9], "the shear is kept");
}

/// Every kind of defect: the plate's holes, crack and tangled fin, and a
/// loose tetrahedron above it.
fn defective_file(dir: &std::path::Path) -> std::path::PathBuf {
    let piece = mf_fixtures::translated(
        &mf_fixtures::scaled(&mf_fixtures::tetrahedron(), 4.0),
        [10.0, 10.0, 8.0],
    );
    let mesh = mf_fixtures::merge(&[mf_fixtures::plate_with_defects(), piece]);
    model_file(dir, "scan", mesh, IDENTITY)
}

#[test]
fn transform_tool_hides_defect_tints_and_shows_axes_in_legend() {
    require_gpu!("transform_tool_hides_defect_tints_and_shows_axes_in_legend");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(defective_file(dir.path()));
    h.run_steps(3);
    let defect_bits = u32::from(FaceStates::LOOSE_PIECE)
        | u32::from(FaceStates::NON_MANIFOLD_ADJ)
        | u32::from(FaceStates::RECENT_REPAIR);
    assert!(h.state().holes_drawn());
    assert_ne!(h.state().shown_states() & defect_bits, 0);
    // The Overlays popover lists the defect colours…
    h.get_by_label("Overlays").click();
    h.run_steps(3);
    h.get_by_label("Loose pieces");

    open_transform(&mut h);
    h.run_steps(3);
    let app = h.state();
    assert!(!app.holes_drawn(), "hole outlines hidden");
    assert_eq!(app.shown_states() & defect_bits, 0, "defect tints hidden");
    assert!(app.view_layers().is_empty(), "no defect lines");
    assert!(app.hole_markers().is_none(), "no hole markers");
    for axis in ["X axis", "Y axis", "Z axis"] {
        h.get_by_label(axis);
    }
    assert!(h.query_by_label("Loose pieces").is_none());
    assert!(h.query_by_label("Recent repairs").is_none());

    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.state().holes_drawn(), "leaving the tool shows them again");
    h.get_by_label("Loose pieces");
}

#[test]
fn drop_to_plate_button_lands_on_plate() {
    require_gpu!("drop_to_plate_button_lands_on_plate");
    let dir = tempfile::tempdir().unwrap();
    let mut raised = IDENTITY;
    raised[11] = 7.5;
    let path = model_file(dir.path(), "part", mf_fixtures::cube(10.0), raised);
    let mut h = open(path);
    // The tree row makes the object the target, and Properties offers
    // Transform first.
    // (The first "part" is its tree row; Properties' title says it too.)
    h.get_all_by_label("part").next().unwrap().click();
    h.run_steps(2);
    h.get_by_label("Transform").click();
    wait_until(&mut h, "the gizmo", |a| a.transform_gizmo().is_some());
    h.run_steps(3);
    save(&mut h, "transform-drop-before.png");
    let history = steps(&h);
    h.get_by_label("Drop to plate").click();
    wait_until(&mut h, "the drop commits", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    assert_eq!(steps(&h), history + 1);
    let t = transform(&h);
    assert_eq!(t[11], 0.0, "the cube's base rests on z = 0: {t:?}");
    assert_eq!(t[..11], raised[..11]);
    assert_eq!(
        h.state().status.as_deref(),
        Some("Dropped part onto the plate: moved 7.50 mm down")
    );
    h.run_steps(3);
    save(&mut h, "transform-drop-after.png");
}

/// Checkpoint P1-A: the tool at a laptop size, everything reachable.
#[test]
fn transform_tool_fits_a_laptop_window() {
    require_gpu!("transform_tool_fits_a_laptop_window");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open_size(cube_file(dir.path()), (1280.0, 720.0));
    open_transform(&mut h);
    h.run_steps(3);
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));
    for label in [
        "Position X",
        "Rotation Z",
        "Scale Z",
        "Drop to plate",
        "Reset",
    ] {
        let r = h.get_by_label(label).rect();
        assert!(window.contains_rect(r), "{label} at {r:?} is on screen");
    }
    save(&mut h, "transform-laptop-1280x720.png");
}

/// The midpoint of the first boundary edge of the active object's hole,
/// in window points.
fn hole_edge_point(h: &Harness<'_, MeshFixerApp>) -> egui::Pos2 {
    let app = h.state();
    let mesh = app.active_mesh().unwrap();
    let topo = mesh.topology();
    let l = &topo.boundary_loops[0];
    let t = transform(h);
    let p = |v: u32| {
        let c = mesh.corner_position(v);
        mf_desktop::transform_tool::world_point(&t, glam::DVec3::from(c))
    };
    let mid = (p(l.vertices[0]) + p(l.vertices[1])) * 0.5;
    app.screen_point(mid.to_array()).unwrap()
}

/// Rests the pointer at `pos` until the hover pick answers.
fn hover(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2) {
    h.event(egui::Event::PointerMoved(pos + egui::vec2(30.0, 30.0)));
    h.step();
    h.event(egui::Event::PointerMoved(pos));
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn tooltip_shown(h: &Harness<'_, MeshFixerApp>) -> bool {
    h.query_all_by(|n| {
        n.label()
            .or(n.value())
            .is_some_and(|l| l.contains("Click to select"))
    })
    .next()
    .is_some()
}

/// The tree's defect rows ("Holes (2)", "Narrow cracks (1)", "Loose
/// pieces (1)", "Tangled edges (1)"), as shown.
fn defect_rows(h: &Harness<'_, MeshFixerApp>) -> Vec<String> {
    [
        "Holes (",
        "Narrow cracks (",
        "Loose pieces (",
        "Tangled edges (",
    ]
    .iter()
    .map(|prefix| {
        h.query_by_label_contains(prefix)
            .and_then(|n| n.accesskit_node().label())
            .unwrap_or_else(|| panic!("the tree lists {prefix}…)"))
    })
    .collect()
}

/// Steps from a transform-only change until it has committed (or been
/// undone) and the object is measured at its new placement. Every frame
/// keeps the analysis, never shows "Analysing…" and keeps the tree's
/// defect rows (counts included) as they were (review C1, F4, F5).
fn settles_keeping_the_analysis(
    h: &mut Harness<'_, MeshFixerApp>,
    rows: &[String],
    done: impl Fn(&MeshFixerApp) -> bool,
) {
    let start = Instant::now();
    loop {
        let app = h.state();
        assert!(app.current_analysis().is_some(), "the analysis never goes");
        assert!(!app.is_analyzing(), "no \"Analysing…\" after a move");
        assert_eq!(defect_rows(h), rows, "the tree keeps its counts");
        if app.busy.is_none() && done(app) && !app.is_remeasuring() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "never settled: busy {:?}, remeasuring {}",
            app.busy,
            app.is_remeasuring(),
        );
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    let a = h.state().current_analysis().unwrap();
    assert_eq!(a.transform, transform(h), "measured at the new placement");
}

/// Review C1: a move keeps the analysis. The panels, the tree's counts
/// (holes, cracks, loose pieces, tangled edges) and hovering never go
/// blank while the object's measurements are made again (topology, render
/// mesh and overlays belong to the mesh, not to where it is).
#[test]
fn a_move_keeps_the_analysis_and_hover_working() {
    require_gpu!("a_move_keeps_the_analysis_and_hover_working");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(defective_file(dir.path()));
    h.run_steps(3);
    let rows = defect_rows(&h);
    open_transform(&mut h);
    // Off the 1 mm grid, so any drag moves it.
    h.state_mut().transform_tool_mut().unwrap().snap = false;
    h.run_steps(3);
    let history = steps(&h);
    let (at, dir_x) = x_arrow(&h);
    press(&mut h, at);
    move_to(&mut h, at, at + dir_x * 80.0);
    release(&mut h, at + dir_x * 80.0);
    settles_keeping_the_analysis(&mut h, &rows, |a| {
        a.session.history_labels().len() == history + 1
    });
    let status = h.state().status.clone().unwrap();
    assert!(status.starts_with("Moved scan by "), "{status}");
    // Hover still works (with the tool closed: it hides defect hover).
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    let pos = hole_edge_point(&h);
    hover(&mut h, pos);
    assert!(
        h.state().view_hover().is_some(),
        "hovering the moved hole finds it"
    );
}

/// F4: a turn keeps the analysis too (its measurements are made again in
/// full, on a worker), and so does undoing it (F5): the counts of every
/// kind of defect stay on screen throughout.
#[test]
fn a_turn_and_its_undo_keep_the_analysis() {
    require_gpu!("a_turn_and_its_undo_keep_the_analysis");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(defective_file(dir.path()));
    h.run_steps(3);
    let rows = defect_rows(&h);
    open_transform(&mut h);
    h.run_steps(3);
    let (history, revision) = (steps(&h), h.state().session.current());
    type_into(&mut h, "Rotation Z", "90");
    settles_keeping_the_analysis(&mut h, &rows, |a| {
        a.session.history_labels().len() == history + 1
    });
    assert_eq!(
        h.state().status.as_deref(),
        Some("Rotated scan 90° about Z")
    );
    // Undo the turn (the field isn't focused any more: Cmd+Z is undo).
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.step();
    settles_keeping_the_analysis(&mut h, &rows, |a| a.session.current() == revision);
    assert_eq!(transform(&h), IDENTITY);
}

/// Review I2: with Transform open, hovering a defect neither outlines it
/// nor offers "Click to select" (a click there targets the object).
#[test]
fn transform_tool_hides_defect_hover() {
    require_gpu!("transform_tool_hides_defect_hover");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(defective_file(dir.path()));
    h.run_steps(3);
    let pos = hole_edge_point(&h);
    hover(&mut h, pos);
    assert!(
        h.state().view_hover().is_some(),
        "the hole is under the pointer"
    );
    assert!(tooltip_shown(&h));
    open_transform(&mut h);
    hover(&mut h, pos);
    assert!(h.state().view_hover().is_none(), "no defect hover");
    assert!(!tooltip_shown(&h), "no \"Click to select\" tooltip");
}

/// Review I3: the tools' keys go to the open tool only (W, E and R are
/// Transform's while it's open, and nobody's otherwise: see
/// `tools::Interactive::keys`); typed into one of its fields, they're text.
#[test]
fn tool_keys_go_to_the_open_tool_only() {
    require_gpu!("tool_keys_go_to_the_open_tool_only");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(cube_file(dir.path()));
    // Shut, its keys do nothing (and aren't taken: `tools` unit tests).
    h.key_press(egui::Key::E);
    h.run_steps(2);
    assert!(h.state().transform_tool().is_none());
    open_transform(&mut h);
    assert_eq!(h.state().transform_tool().unwrap().mode, GizmoMode::Move);
    h.key_press(egui::Key::E);
    h.run_steps(2);
    assert_eq!(h.state().transform_tool().unwrap().mode, GizmoMode::Rotate);
    h.key_press(egui::Key::W);
    h.run_steps(2);
    // Typing in a field: the key is text, not a mode switch.
    h.get_by_label("Rotation Z").focus();
    h.run_steps(2);
    h.key_press(egui::Key::E);
    h.run_steps(2);
    assert_eq!(h.state().transform_tool().unwrap().mode, GizmoMode::Move);
}

/// Review minors 2 and 4: mid-drag, the wheel doesn't move the camera
/// and Cmd/Ctrl+Z cancels the drag (undoing nothing).
#[test]
fn mid_drag_the_camera_and_history_hold_still() {
    require_gpu!("mid_drag_the_camera_and_history_hold_still");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(cube_file(dir.path()));
    open_transform(&mut h);
    h.run_steps(3);
    let (camera, revision, before) = (h.state().camera, h.state().session.current(), transform(&h));
    let (at, dir_x) = x_arrow(&h);
    let to = at + dir_x * 60.0;
    press(&mut h, at);
    move_to(&mut h, at, to);
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, 3.0),
        modifiers: egui::Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
    });
    h.step();
    assert_eq!(h.state().camera, camera, "the wheel waits for the drag");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.step();
    assert_eq!(
        h.state().shown_transform(h.state().active.unwrap()),
        None,
        "drag cancelled"
    );
    assert_eq!(h.state().session.current(), revision, "nothing undone");
    release(&mut h, to);
    h.run_steps(3);
    assert_eq!(transform(&h), before);
    assert_eq!(h.state().session.current(), revision, "nothing committed");
}

/// Drags the X arrow `px` points with `modifiers` held throughout, and
/// returns the committed X translation.
fn drag_x(h: &mut Harness<'_, MeshFixerApp>, px: f32, modifiers: egui::Modifiers) -> f64 {
    let history = steps(h);
    let (at, dir_x) = x_arrow(h);
    h.event(egui::Event::ModifiersChanged(modifiers));
    press(h, at);
    move_to(h, at, at + dir_x * px);
    release(h, at + dir_x * px);
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    wait_until(h, "the move commits", |a| {
        a.busy.is_none() && a.session.history_labels().len() == history + 1
    });
    transform(h)[9]
}

/// Snapping is on by default (a 1 mm grid); Cmd/Ctrl while dragging
/// turns it off for that drag.
#[test]
fn moves_snap_to_the_grid_unless_cmd_is_held() {
    require_gpu!("moves_snap_to_the_grid_unless_cmd_is_held");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(cube_file(dir.path()));
    open_transform(&mut h);
    h.run_steps(3);
    let snapped = drag_x(&mut h, 73.0, egui::Modifiers::NONE);
    assert!(
        snapped > 0.5 && snapped.fract() == 0.0,
        "on the 1 mm grid: {snapped}"
    );
    let free = drag_x(&mut h, 41.0, egui::Modifiers::COMMAND);
    assert!(
        free > snapped && free.fract() != 0.0,
        "off the grid with Cmd: {free}"
    );
}

/// Review minor 3: Enter on a field showing its value unchanged commits
/// nothing; a real change commits once.
#[test]
fn enter_on_an_unchanged_field_commits_nothing() {
    require_gpu!("enter_on_an_unchanged_field_commits_nothing");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(cube_file(dir.path()));
    open_transform(&mut h);
    h.run_steps(3);
    let history = steps(&h);
    type_into(&mut h, "Position X", "5");
    h.run_steps(5);
    assert_eq!(steps(&h), history, "5 is what it shows");
    type_into(&mut h, "Scale Y", "100");
    h.run_steps(5);
    assert_eq!(steps(&h), history, "100% is what it shows");
    type_into(&mut h, "Position X", "6");
    wait_until(&mut h, "the move commits", |a| {
        a.busy.is_none() && a.session.history_labels().len() == history + 1
    });
}

/// A plain click on a handle is the tool's: it picks nothing.
#[test]
fn clicking_a_handle_is_not_a_pick() {
    require_gpu!("clicking_a_handle_is_not_a_pick");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(defective_file(dir.path()));
    open_transform(&mut h);
    h.run_steps(3);
    let (status, revision) = (h.state().status.clone(), h.state().session.current());
    let (at, _) = x_arrow(&h);
    press(&mut h, at);
    release(&mut h, at);
    h.run_steps(5);
    assert!(h.state().selection.is_none());
    assert_eq!(h.state().status, status);
    assert_eq!(h.state().session.current(), revision);
    assert!(h.state().transform_tool().is_some());
}

// --- Timing (heavy list, on demand: scripts/run-tests.sh) ---

const BIG: usize = 30_000_000;
const FRAME_BUDGET: Duration = Duration::from_millis(50);

fn big_model(transform: [f64; 12]) -> mf_io::ImportedModel {
    let mesh = mf_fixtures::damaged_sphere(&mf_fixtures::DamageSpec::for_triangles(BIG, 3));
    mf_io::ImportedModel {
        objects: vec![ModelObject {
            name: "big".into(),
            mesh,
            transform,
        }],
        unit: Unit::Millimeter,
        source: mf_io::SourceInfo {
            path: "big.stl".into(),
            content_hash: mf_geometry::ContentHash([0; 32]),
            format: mf_io::Format::StlBinary,
            precision: mf_io::SourcePrecision::F32,
        },
        warnings: vec![],
    }
}

/// The app with the 30M sphere open and analysed, and its render state.
/// Prints how long each setup phase took (fix round 2, F1): generating the
/// sphere, adding it to the document, and the first analysis to the first
/// frame drawn. Setup has its own (generous) budget, apart from what a
/// test then asserts about a move.
fn opened_big(transform: [f64; 12]) -> (Harness<'static, MeshFixerApp>, egui_wgpu::RenderState) {
    let mut setup = egui_kittest::wgpu::default_wgpu_setup();
    if let egui_wgpu::WgpuSetup::CreateNew(create) = &mut setup {
        create.device_descriptor = Arc::new(mf_desktop::gpu::device_descriptor);
    }
    let render_state =
        egui_kittest::wgpu::create_render_state(setup, egui_wgpu::RendererOptions::PREDICTABLE);
    let mut h = Harness::builder()
        .with_size((1440.0, 900.0))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::from_render_state(
            render_state.clone(),
        ))
        .build_eframe(|cc| MeshFixerApp::with_settings(cc, None, Default::default()).unwrap());
    let t0 = Instant::now();
    let model = big_model(transform);
    let generated = t0.elapsed();
    let t1 = Instant::now();
    h.state_mut().import_model(model);
    let imported = t1.elapsed();
    let t2 = Instant::now();
    while !(h.state().current_analysis().is_some() && h.state().viewport.frames_rendered > 0) {
        assert!(t2.elapsed() < Duration::from_secs(1200), "30M analysis");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    eprintln!(
        "setup: 30M generated {:.2} s, added to the document {:.2} s, first analysis and frame {:.2} s",
        generated.as_secs_f64(),
        imported.as_secs_f64(),
        t2.elapsed().as_secs_f64()
    );
    (h, render_state)
}

/// Paints a frame as eframe does and waits for the GPU (as progress.rs's
/// `Painter`: headless, this stands in for presenting).
fn paint_and_wait(
    h: &Harness<'_, MeshFixerApp>,
    rs: &egui_wgpu::RenderState,
    frame: &wgpu::TextureView,
) {
    let ppp = h.ctx.pixels_per_point();
    let jobs = h.ctx.tessellate(h.output().shapes.clone(), ppp);
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [1440, 900],
        pixels_per_point: ppp,
    };
    let mut renderer = rs.renderer.write();
    let mut encoder = rs.device.create_command_encoder(&Default::default());
    let uploads = renderer.update_buffers(&rs.device, &rs.queue, &mut encoder, &jobs, &screen);
    {
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("test egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: frame,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            })
            .forget_lifetime();
        renderer.render(&mut pass, &jobs, &screen);
    }
    rs.queue
        .submit(uploads.into_iter().chain(std::iter::once(encoder.finish())));
    drop(renderer);
    rs.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(10)),
        })
        .unwrap();
}

fn frame_texture(rs: &egui_wgpu::RenderState) -> wgpu::TextureView {
    rs.device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("test frame"),
            size: wgpu::Extent3d {
                width: 1440,
                height: 900,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: rs.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

fn p95(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[(times.len() * 95 / 100).min(times.len() - 1)]
}

/// A drag only changes a uniform (MF-OBJ-001): dragging a 30M object
/// costs no more than orbiting it, within 1 ms at the 95th percentile.
#[test]
fn dragging_a_30m_object_keeps_frames_under_orbit_plus_1ms() {
    require_gpu!("dragging_a_30m_object_keeps_frames_under_orbit_plus_1ms");
    let (mut h, rs) = opened_big(IDENTITY);
    let frame = frame_texture(&rs);
    open_transform(&mut h);
    let timed = |h: &mut Harness<'_, MeshFixerApp>,
                 step: &mut dyn FnMut(&mut Harness<'_, MeshFixerApp>)| {
        let start = Instant::now();
        step(h);
        paint_and_wait(h, &rs, &frame);
        start.elapsed()
    };
    for _ in 0..5 {
        timed(&mut h, &mut |h| {
            h.state_mut().camera.as_mut().unwrap().orbit(0.01, 0.0);
            h.step();
        });
    }
    let orbit: Vec<Duration> = (0..60)
        .map(|_| {
            timed(&mut h, &mut |h| {
                h.state_mut().camera.as_mut().unwrap().orbit(0.002, 0.0);
                h.step();
            })
        })
        .collect();
    let (at, dir_x) = x_arrow(&h);
    press(&mut h, at);
    let mut drag = Vec::new();
    for i in 1..=60 {
        let p = at + dir_x * (2.0 * i as f32);
        drag.push(timed(&mut h, &mut |h| {
            h.event(egui::Event::PointerMoved(p));
            h.step();
        }));
    }
    assert!(
        h.state()
            .shown_transform(h.state().active.unwrap())
            .is_some()
    );
    let (o, d) = (p95(orbit), p95(drag[5..].to_vec()));
    eprintln!(
        "orbit p95 {:.2} ms, drag p95 {:.2} ms",
        o.as_secs_f64() * 1e3,
        d.as_secs_f64() * 1e3
    );
    assert!(
        d <= o + Duration::from_millis(1),
        "dragging ({d:?}) costs more than orbiting ({o:?}) + 1 ms"
    );
}

/// MF-RT-001: Drop to plate on 30M runs on the worker; no UI frame goes
/// over 50 ms from the click until it has committed.
#[test]
fn ui_frames_stay_under_50ms_while_dropping_30m_to_plate() {
    require_gpu!("ui_frames_stay_under_50ms_while_dropping_30m_to_plate");
    let mut raised = IDENTITY;
    raised[11] = 50.0;
    let (mut h, rs) = opened_big(raised);
    let frame = frame_texture(&rs);
    open_transform(&mut h);
    for _ in 0..5 {
        h.step();
        paint_and_wait(&h, &rs, &frame);
    }
    let history = steps(&h);
    h.get_by_label("Drop to plate").click();
    let mut frames = Vec::new();
    let mut started = false;
    loop {
        let start = Instant::now();
        h.state_mut().camera.as_mut().unwrap().orbit(0.002, 0.0);
        h.step();
        paint_and_wait(&h, &rs, &frame);
        frames.push(start.elapsed());
        started |= h.state().busy.is_some();
        assert!(started || frames.len() < 10, "the drop didn't start");
        if started && h.state().busy.is_none() {
            break;
        }
        assert!(frames.len() < 100_000, "never finished");
    }
    assert_eq!(steps(&h), history + 1, "the drop committed");
    let worst = frames.iter().max().unwrap();
    eprintln!(
        "worst frame {:.1} ms over {} frames",
        worst.as_secs_f64() * 1e3,
        frames.len()
    );
    assert!(*worst < FRAME_BUDGET, "a frame took {worst:?}");
}

/// Review C1 at scale: after a move of a 30M object commits, the analysis
/// stays (no "Analysing…", no blank panels) while it is measured again,
/// and the re-measurement settles promptly. Phase times are printed (fix
/// round 2, F1); setup has its own budget (`opened_big`).
#[test]
fn analysis_stays_available_after_moving_a_30m_object() {
    require_gpu!("analysis_stays_available_after_moving_a_30m_object");
    let (mut h, _rs) = opened_big(IDENTITY);
    let t = Instant::now();
    open_transform(&mut h);
    // Off the 1 mm grid: a short drag on a big sphere could round to no
    // move at all, and nothing would commit.
    h.state_mut().transform_tool_mut().unwrap().snap = false;
    eprintln!("open Transform: {:.2} s", t.elapsed().as_secs_f64());
    let history = steps(&h);
    let (at, dir_x) = x_arrow(&h);
    let t = Instant::now();
    press(&mut h, at);
    move_to(&mut h, at, at + dir_x * 60.0);
    let mid = h.state().shown_transform(h.state().active.unwrap());
    release(&mut h, at + dir_x * 60.0);
    eprintln!(
        "drag (12 frames): {:.2} s, mid-drag x {:?}",
        t.elapsed().as_secs_f64(),
        mid.map(|m| m[9])
    );
    assert!(mid.is_some_and(|m| m[9] != 0.0), "the drag moved it");
    let released = Instant::now();
    let mut committed = None;
    loop {
        let app = h.state();
        assert!(app.current_analysis().is_some(), "the analysis never goes");
        assert!(!app.is_analyzing(), "no \"Analysing…\" after a move");
        let done = app.busy.is_none() && steps(&h) == history + 1;
        if done && committed.is_none() {
            committed = Some(released.elapsed());
        }
        if done && !app.is_remeasuring() {
            break;
        }
        match committed {
            None => assert!(
                released.elapsed() < Duration::from_secs(30),
                "the move didn't commit in 30 s"
            ),
            Some(c) => assert!(
                released.elapsed() - c < Duration::from_secs(5),
                "the re-measurement took over 5 s"
            ),
        }
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
    let committed = committed.unwrap();
    eprintln!(
        "release to commit: {:.3} s; commit to measured: {:.3} s",
        committed.as_secs_f64(),
        (released.elapsed() - committed).as_secs_f64()
    );
}

/// F2: turning a 30M object measures it again in full (world size is
/// O(vertices)) on a worker: the analysis stays, and no UI frame (the
/// camera orbiting, so the 30M mesh is redrawn every frame) takes 50 ms
/// or more from the Enter that commits it until it is measured.
#[test]
fn turning_a_30m_object_keeps_the_analysis_and_frames_under_50ms() {
    require_gpu!("turning_a_30m_object_keeps_the_analysis_and_frames_under_50ms");
    let (mut h, rs) = opened_big(IDENTITY);
    let frame = frame_texture(&rs);
    open_transform(&mut h);
    for _ in 0..5 {
        h.step();
        paint_and_wait(&h, &rs, &frame);
    }
    let history = steps(&h);
    h.get_by_label("Rotation Z").focus();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_steps(1);
    h.get_by_label("Rotation Z").type_text("90");
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    let entered = Instant::now();
    let mut frames = Vec::new();
    let mut committed = None;
    loop {
        let start = Instant::now();
        h.state_mut().camera.as_mut().unwrap().orbit(0.002, 0.0);
        h.step();
        paint_and_wait(&h, &rs, &frame);
        frames.push(start.elapsed());
        let app = h.state();
        assert!(app.current_analysis().is_some(), "the analysis never goes");
        assert!(!app.is_analyzing());
        let done = app.busy.is_none() && steps(&h) == history + 1;
        if done && committed.is_none() {
            committed = Some(entered.elapsed());
        }
        if done && !app.is_remeasuring() {
            break;
        }
        assert!(entered.elapsed() < Duration::from_secs(60), "never settled");
    }
    let committed = committed.unwrap();
    let worst = frames.iter().max().unwrap();
    let mut sorted = frames.clone();
    sorted.sort();
    eprintln!(
        "turn: Enter to commit {:.3} s, commit to measured {:.3} s, {} frames, median {:.1} ms, worst {:.1} ms",
        committed.as_secs_f64(),
        (entered.elapsed() - committed).as_secs_f64(),
        frames.len(),
        sorted[sorted.len() / 2].as_secs_f64() * 1e3,
        worst.as_secs_f64() * 1e3
    );
    assert!(*worst < FRAME_BUDGET, "a frame took {worst:?}");
}

/// Where ring `i`'s grip is (window points): its band's middle, and the
/// centroid of the arrowhead its drag heads towards.
fn ring_grip(h: &Harness<'_, MeshFixerApp>, i: u8) -> (egui::Pos2, egui::Pos2) {
    let app = h.state();
    let g = app.transform_gizmo().unwrap();
    let cam = app.camera.unwrap();
    let rect = app.viewport.last_rect.unwrap();
    let view = (rect.width() as f64, rect.height() as f64);
    let (arc, heads) = g
        .handles(&cam, view)
        .into_iter()
        .find_map(|s| match (s.geometry, s.handle) {
            (HandleGeometry::RingGrip { arc, heads }, Handle::Ring(k)) if k == i => {
                Some((arc, heads))
            }
            _ => None,
        })
        .expect("each ring has a grip");
    let mid = arc[arc.len() / 2];
    let head = heads[0];
    let tip = (
        (head[0].0 + head[1].0 + head[2].0) / 3.0,
        (head[0].1 + head[1].1 + head[2].1) / 3.0,
    );
    let at = |p: (f64, f64)| rect.left_top() + egui::vec2(p.0 as f32, p.1 as f32);
    (at(mid), at(tip))
}

/// Phase 1 design stream (owner review): each rotation ring has a curved
/// arrow grip in its axis colour. Pressing it and dragging along the
/// arrow turns the object about that axis (one command, one undo), and
/// highlights the grip's ring while hovered.
#[test]
fn dragging_a_ring_grip_turns_the_object_about_its_axis() {
    require_gpu!("dragging_a_ring_grip_turns_the_object_about_its_axis");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(slab_file(dir.path()));
    open_transform(&mut h);
    h.key_press(egui::Key::E);
    h.run_steps(3);
    let (grip, toward) = ring_grip(&h, 2);
    h.event(egui::Event::PointerMoved(grip));
    h.run_steps(2);
    assert_eq!(
        h.state().transform_tool().unwrap().hover,
        Some(Handle::Ring(2)),
        "the grip is the Z ring's handle"
    );
    let history = steps(&h);
    press(&mut h, grip);
    // Past the arrowhead, along the ring.
    let end = grip + (toward - grip) * 2.5;
    move_to(&mut h, grip, end);
    release(&mut h, end);
    wait_until(&mut h, "the turn commits", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    assert_eq!(steps(&h), history + 1, "one command");
    match decompose(&transform(&h)) {
        Decomposed::Trs { rotate_deg_xyz, .. } => {
            assert!(rotate_deg_xyz.z.abs() >= 15.0 - 1e-9, "{rotate_deg_xyz:?}");
            assert!(rotate_deg_xyz.x.abs() < 1e-9 && rotate_deg_xyz.y.abs() < 1e-9);
        }
        Decomposed::Sheared => panic!("a turn isn't a shear"),
    }
}

/// No view ring at rest; pointing just outside the axis rings shows it
/// (it is the hovered handle, so it's drawn highlighted).
#[test]
fn the_view_ring_shows_only_under_the_pointer() {
    require_gpu!("the_view_ring_shows_only_under_the_pointer");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(slab_file(dir.path()));
    open_transform(&mut h);
    h.key_press(egui::Key::E);
    h.run_steps(3);
    let app = h.state();
    let g = app.transform_gizmo().unwrap();
    let cam = app.camera.unwrap();
    let rect = app.viewport.last_rect.unwrap();
    let view = (rect.width() as f64, rect.height() as f64);
    assert!(
        g.handles(&cam, view)
            .iter()
            .all(|s| s.handle != Handle::ViewRing),
        "no view ring at rest"
    );
    let centre = mf_render::screen_position(&cam, g.pivot, view, g.pivot.to_array()).unwrap();
    let r = g.size_pt * mf_render::VIEW_RING_SCALE;
    let a = 200f64.to_radians();
    let at = rect.left_top()
        + egui::vec2(
            (centre.0 + r * a.cos()) as f32,
            (centre.1 + r * a.sin()) as f32,
        );
    h.event(egui::Event::PointerMoved(at));
    h.run_steps(2);
    assert_eq!(
        h.state().transform_tool().unwrap().hover,
        Some(Handle::ViewRing)
    );
    save(&mut h, "transform-rotate-view-ring.png");
}
