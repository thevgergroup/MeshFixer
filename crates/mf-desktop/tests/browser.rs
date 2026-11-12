#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The defect browser at scale (M4 Task 12): sculpts and scans have
//! hundreds of holes. Clicking one in the view or the list selects it and
//! offers Fill; rows fill in one click; holes are grouped by size with a
//! group fill; a slider batch-fills up to a size as one undo; holes can be
//! marked as intentional openings; N / Shift+N walk them; B box-selects
//! the visible ones; the hole overview tints their surroundings and marks
//! the tiny ones; numbering never shifts after a fill; Esc in a popup
//! leaves the selection; F frames with the animated move.
//!
//! With `MF_ARTIFACTS` set, `defect_list_click_selects_and_frames_the_hole`
//! also writes a frame (list row selected, hole framed) there as
//! `defect-browser.png` (milestone evidence).

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use glam::DVec3;
use mf_desktop::MeshFixerApp;
use mf_desktop::defects::{Defect, HoleGroup, choice_copy, defect_copy};
use mf_geometry::{ContentHash, FaceId, Mesh};
use mf_io::threemf::{IDENTITY, ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_render::{FaceStates, model_matrix};
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
            start.elapsed() < Duration::from_secs(60),
            "timed out waiting for: {what} (status {:?}, error {:?}, busy {:?})",
            h.state().status,
            h.state().error,
            h.state().busy
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A flat n×n-cell plate (cells `cell` mm) with a k×k-cell hole cut at each
/// `(i, j, k)` (its lower-left cell), plus its open rim.
fn plate(n: u32, cell: f64, holes: &[(u32, u32, u32)]) -> Mesh {
    let grid = mf_fixtures::scaled(&mf_fixtures::grid(n), cell);
    let mut e = grid.edit();
    for &(i, j, k) in holes {
        for b in j..j + k {
            for a in i..i + k {
                let f = 2 * (b * n + a);
                e.remove_face(FaceId(f)).unwrap();
                e.remove_face(FaceId(f + 1)).unwrap();
            }
        }
    }
    e.finish().unwrap().0
}

/// 300 holes: 250 one-cell (4 edges, tiny) and 50 three-cell (12 edges,
/// small), on a 73×73-cell plate (whose 292-edge rim is a large hole).
fn three_hundred_holes() -> Mesh {
    let mut holes = Vec::new();
    for b in 0..18u32 {
        for a in 0..18u32 {
            let n = holes.len();
            if n < 300 {
                holes.push((1 + 4 * a, 1 + 4 * b, if n < 250 { 1 } else { 3 }));
            }
        }
    }
    plate(73, 1.0, &holes)
}

/// Three square holes on a 12×12 plate of 10 mm cells (1, 2 and 3 cells
/// across: 4, 8 and 12 edges), and its 48-edge rim (Hole 1, the largest).
fn three_holes() -> Mesh {
    plate(12, 10.0, &[(1, 1, 1), (5, 1, 2), (2, 6, 3)])
}

fn model(mesh: Mesh) -> ImportedModel {
    ImportedModel {
        metadata: Default::default(),
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: "plate".into(),
            mesh,
            transform: IDENTITY,
        }],
        unit: Unit::Millimeter,
        source: SourceInfo {
            path: "plate.3mf".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
        },
        warnings: vec![],
    }
}

fn opened(mesh: Mesh) -> Harness<'static, MeshFixerApp> {
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(|cc| MeshFixerApp::new(cc, None).unwrap());
    h.state_mut().import_model(model(mesh));
    wait_until(&mut h, "analysis, hole view and picking", |a| {
        a.current_analysis().is_some()
            && a.hole_view().is_some()
            && a.shows_current_revision()
            && a.picking_ready()
            && a.camera.is_some()
    });
    h.run();
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

fn key(h: &mut Harness<'_, MeshFixerApp>, key: egui::Key, modifiers: egui::Modifiers) {
    for pressed in [true, false] {
        h.event(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        });
    }
    h.step();
}

/// Opens the Repair tool from the rail (its whole-object actions live
/// there, ui-plan.md §7).
fn open_repair(h: &mut Harness<'_, MeshFixerApp>) {
    if h.state().active_tool != "repair" {
        h.get_by_label("Repair").click();
        h.run_steps(3);
    }
}

/// Runs a Browser row's right-click action (its registered context
/// action, e.g. "Fill Hole 3").
fn row_action(h: &mut Harness<'_, MeshFixerApp>, row: &str, action: &str) {
    h.get_by_label(row).click_secondary();
    h.run_steps(3);
    h.get_by_label(action).click();
    h.step();
}

/// The holes the browser lists, in its order.
fn listed(app: &MeshFixerApp) -> Vec<Defect> {
    let a = app.current_analysis().unwrap();
    let v = app.hole_view().unwrap();
    v.listed
        .iter()
        .map(|&i| a.defects.holes[i].clone())
        .collect()
}

fn loop_id(d: &Defect) -> u32 {
    match d {
        Defect::Hole { loop_id, .. } | Defect::OverCap { loop_id, .. } => *loop_id,
        other => panic!("not a hole: {other:?}"),
    }
}

fn number(d: &Defect) -> usize {
    match d {
        Defect::Hole { number, .. } | Defect::OverCap { number, .. } => *number,
        other => panic!("not a hole: {other:?}"),
    }
}

fn row(app: &MeshFixerApp, d: &Defect) -> String {
    defect_copy(d, app.session.document().units()).row
}

fn selected_loops(app: &MeshFixerApp) -> Vec<u32> {
    app.selection
        .as_ref()
        .filter(|s| s.kind == SelectionKind::BoundaryLoop && s.revision == app.session.current())
        .map_or_else(Vec::new, |s| s.ids.clone())
}

/// World points of loop `id` on the active object.
fn loop_points(app: &MeshFixerApp, id: u32) -> Vec<DVec3> {
    let o = app.session.document().object(app.active.unwrap()).unwrap();
    let m = o.mesh().unwrap();
    let model = model_matrix(&o.transform);
    m.topology()
        .boundary_loops
        .iter()
        .find(|l| l.id == id)
        .unwrap()
        .vertices
        .iter()
        .map(|&v| model.transform_point3(DVec3::from(m.corner_position(v))))
        .collect()
}

fn centre(points: &[DVec3]) -> DVec3 {
    points.iter().copied().sum::<DVec3>() / points.len() as f64
}

/// Steps until the camera stops moving; returns how many frames it moved.
fn settle_camera(h: &mut Harness<'_, MeshFixerApp>) -> usize {
    let mut moved = 0;
    let mut steps = 0;
    while h.state().camera_animating() || steps < 3 {
        let before = h.state().camera;
        h.step();
        if h.state().camera != before {
            moved += 1;
        }
        steps += 1;
        assert!(steps < 600, "the camera never settled");
    }
    moved
}

/// Distance of `world` from the view centre, as a fraction of its height.
fn off_centre(app: &MeshFixerApp, world: DVec3) -> f32 {
    let view = app.viewport.last_rect.unwrap();
    app.screen_point(world.to_array())
        .unwrap()
        .distance(view.center())
        / view.height()
}

fn hole_count(app: &MeshFixerApp) -> usize {
    app.active_mesh().unwrap().topology().boundary_loops.len()
}

#[test]
fn clicking_a_hole_in_the_viewport_opens_fill_options_for_it() {
    require_gpu!("clicking_a_hole_in_the_viewport_opens_fill_options_for_it");
    let mut h = opened(three_holes());
    let hole = listed(h.state())
        .into_iter()
        .find(|d| matches!(d, Defect::Hole { edges: 12, .. }))
        .unwrap();
    let pts = loop_points(h.state(), loop_id(&hole));
    let edge = (pts[0] + pts[1]) * 0.5;
    let at = h.state().screen_point(edge.to_array()).unwrap();
    click_at(&mut h, at);
    wait_until(&mut h, "the hole selected", |a| {
        selected_loops(a) == vec![loop_id(&hole)]
    });
    h.run();
    // Properties: what it is, the fill tool with its options, and Apply.
    // (The view's hover tooltip may name it too.)
    assert!(
        h.get_all_by_label(&defect_copy(&hole, Unit::Millimeter).title)
            .next()
            .is_some()
    );
    // The inspector card offers its operation; it opens in Repair.
    h.get_by_label("Fill hole").click();
    h.run();
    h.get_by_label("Refine");
    h.get_by_label("Apply");
}

#[test]
fn defect_list_click_selects_and_frames_the_hole() {
    require_gpu!("defect_list_click_selects_and_frames_the_hole");
    let mut h = opened(three_holes());
    let hole = listed(h.state())
        .into_iter()
        .find(|d| matches!(d, Defect::Hole { edges: 4, .. }))
        .unwrap();
    let label = row(h.state(), &hole);
    let pos = h.get_by_label(&label).rect().center();
    click_at(&mut h, pos);
    press(&mut h, pos, true);
    press(&mut h, pos, false);
    assert!(settle_camera(&mut h) > 3, "an animated move, not a jump");
    assert_eq!(selected_loops(h.state()), vec![loop_id(&hole)]);
    let c = centre(&loop_points(h.state(), loop_id(&hole)));
    assert!(off_centre(h.state(), c) < 0.05, "framed at the centre");
    if let Ok(dir) = std::env::var("MF_ARTIFACTS") {
        h.render()
            .unwrap()
            .save(std::path::Path::new(&dir).join("defect-browser.png"))
            .unwrap();
    }
}

#[test]
fn one_click_fill_from_defect_list_fills_only_that_hole() {
    require_gpu!("one_click_fill_from_defect_list_fills_only_that_hole");
    let mut h = opened(three_holes());
    let before = hole_count(h.state());
    let hole = listed(h.state())
        .into_iter()
        .find(|d| matches!(d, Defect::Hole { edges: 8, .. }))
        .unwrap();
    let rev = h.state().session.current();
    let label = row(h.state(), &hole);
    row_action(&mut h, &label, &format!("Fill Hole {}", number(&hole)));
    wait_until(&mut h, "the fill", |a| {
        a.session.current() != rev && a.busy.is_none() && a.hole_view().is_some()
    });
    assert_eq!(hole_count(h.state()), before - 1);
    let record = h.state().session.records_to_current().pop().unwrap();
    assert_eq!(record.op_id, "mesh.fill_holes");
    assert_eq!(record.selection.unwrap().ids, vec![loop_id(&hole)]);
    assert!(
        listed(h.state())
            .iter()
            .all(|d| !matches!(d, Defect::Hole { edges: 8, .. })),
        "the 8-edge hole is closed, the others are open"
    );
}

/// Addendum C: "Hole N" never renumbers after a fill.
#[test]
fn hole_numbers_stay_after_a_fill() {
    require_gpu!("hole_numbers_stay_after_a_fill");
    let mut h = opened(three_holes());
    let holes = listed(h.state());
    let three = holes.iter().find(|d| number(d) == 3).unwrap().clone();
    let rev = h.state().session.current();
    let one = holes.iter().find(|d| number(d) == 1).unwrap();
    let label = row(h.state(), one);
    row_action(&mut h, &label, "Fill Hole 1");
    wait_until(&mut h, "the fill", |a| {
        a.session.current() != rev && a.busy.is_none() && a.hole_view().is_some()
    });
    h.run();
    let after = listed(h.state());
    assert!(after.iter().all(|d| number(d) != 1), "hole 1 is filled");
    let still = after.iter().find(|d| number(d) == 3).unwrap();
    assert_eq!(row(h.state(), still), row(h.state(), &three));
    h.get_by_label(&row(h.state(), &three));
    // Undo: hole 1 is back, with its number.
    h.get_by_label("Undo").click();
    wait_until(&mut h, "undo", |a| {
        a.hole_view().is_some() && listed(a).iter().any(|d| number(d) == 1)
    });
}

#[test]
fn size_groups_count_and_fill_a_group_in_one_command() {
    require_gpu!("size_groups_count_and_fill_a_group_in_one_command");
    let mut h = opened(three_hundred_holes());
    let view = h.state().hole_view().unwrap();
    assert_eq!(view.group(HoleGroup::Tiny).len(), 250);
    assert_eq!(view.group(HoleGroup::Small).len(), 50);
    assert_eq!(view.group(HoleGroup::Large).len(), 1, "the rim");
    open_repair(&mut h);
    h.get_by_label("Tiny holes (up to 8 edges) · 250");
    h.get_by_label("Small holes (9–30 edges) · 50");
    let rev = h.state().session.current();
    h.get_by_label("Fill all 250 tiny holes").click();
    wait_until(&mut h, "the group fill", |a| {
        a.session.current() != rev && a.busy.is_none() && a.hole_view().is_some()
    });
    let s = &h.state().session;
    assert_eq!(
        s.node(s.current()).unwrap().parent,
        Some(rev),
        "one command"
    );
    let record = s.records_to_current().pop().unwrap();
    assert_eq!(record.selection.unwrap().ids.len(), 250);
    let view = h.state().hole_view().unwrap();
    assert_eq!(view.group(HoleGroup::Tiny).len(), 0);
    assert_eq!(view.group(HoleGroup::Small).len(), 50);
}

#[test]
fn batch_fill_slider_previews_count_and_undoes_in_one_step() {
    require_gpu!("batch_fill_slider_previews_count_and_undoes_in_one_step");
    let mut h = opened(three_hundred_holes());
    open_repair(&mut h);
    h.get_by_label("Fill all holes").click();
    h.run();
    h.get_by_label("Fill holes up to");
    for (edges, preview) in [
        (8, "250 holes · 51 left open"),
        (30, "300 holes · 1 left open"),
    ] {
        h.state_mut()
            .params_for("mesh.fill_holes")
            .insert("max_loop_edges".into(), mf_spec::ParamValue::Int(edges));
        h.run();
        h.get_by_label(preview);
    }
    let rev = h.state().session.current();
    let holes = hole_count(h.state());
    h.get_by_label("Apply").click();
    wait_until(&mut h, "the batch fill", |a| {
        a.session.current() != rev && a.busy.is_none() && a.hole_view().is_some()
    });
    assert_eq!(hole_count(h.state()), 1, "only the rim is left open");
    h.get_by_label("Undo").click();
    wait_until(&mut h, "one undo", |a| a.session.current() == rev);
    assert_eq!(hole_count(h.state()), holes);
}

#[test]
fn intentional_openings_survive_edits_and_are_excluded_from_fill_all() {
    require_gpu!("intentional_openings_survive_edits_and_are_excluded_from_fill_all");
    let mut h = opened(three_holes());
    let holes = listed(h.state());
    let keep = holes
        .iter()
        .find(|d| matches!(d, Defect::Hole { edges: 4, .. }))
        .unwrap()
        .clone();
    let keep_points = loop_points(h.state(), loop_id(&keep));
    h.state_mut().select_defect(&keep);
    h.run();
    h.get_by_label("Mark as intentional opening").click();
    wait_until(&mut h, "the mark", |a| {
        a.hole_view().is_some_and(|v| v.marked.len() == 1)
    });
    h.run();
    h.get_by_label("Intentional openings (1)");
    assert!(
        listed(h.state()).iter().all(|d| number(d) != number(&keep)),
        "a marked opening isn't listed as a hole"
    );
    // Fill another hole: the mark follows.
    let other = holes
        .iter()
        .find(|d| matches!(d, Defect::Hole { edges: 8, .. }))
        .unwrap();
    let rev = h.state().session.current();
    let label = row(h.state(), other);
    row_action(&mut h, &label, &format!("Fill Hole {}", number(other)));
    wait_until(&mut h, "the fill", |a| {
        a.session.current() != rev && a.busy.is_none() && a.hole_view().is_some()
    });
    let marks = h
        .state()
        .session
        .document()
        .object(h.state().active.unwrap())
        .unwrap()
        .openings
        .clone();
    assert_eq!(marks.len(), 1);
    assert_eq!(
        centre(&loop_points(h.state(), marks[0])),
        centre(&keep_points),
        "the same opening, at its new id"
    );
    // Fill all holes leaves it open.
    h.state_mut().clear_selection();
    h.run();
    open_repair(&mut h);
    h.get_by_label("Fill all holes").click();
    h.run();
    let rev = h.state().session.current();
    h.get_by_label("Apply").click();
    wait_until(&mut h, "fill all", |a| {
        a.session.current() != rev && a.busy.is_none() && a.hole_view().is_some()
    });
    let topo = h.state().active_mesh().unwrap().topology();
    assert_eq!(
        topo.boundary_loops.len(),
        1,
        "only the marked opening is left"
    );
    let marks = h
        .state()
        .session
        .document()
        .object(h.state().active.unwrap())
        .unwrap()
        .openings
        .clone();
    assert_eq!(
        centre(&loop_points(h.state(), marks[0])),
        centre(&keep_points)
    );
}

#[test]
fn next_hole_selects_and_frames_in_order() {
    require_gpu!("next_hole_selects_and_frames_in_order");
    let mut h = opened(three_holes());
    let order: Vec<u32> = listed(h.state()).iter().map(loop_id).collect();
    for (k, modifiers, want) in [
        (0, egui::Modifiers::NONE, order[0]),
        (1, egui::Modifiers::NONE, order[1]),
        (2, egui::Modifiers::SHIFT, order[0]),
    ] {
        key(&mut h, egui::Key::N, modifiers);
        assert!(settle_camera(&mut h) > 0, "step {k}: the camera moves");
        assert_eq!(selected_loops(h.state()), vec![want], "step {k}");
        let c = centre(&loop_points(h.state(), want));
        assert!(off_centre(h.state(), c) < 0.05, "step {k}: framed");
    }
    // Repair's buttons do the same.
    open_repair(&mut h);
    h.get_by_label("Next hole").click();
    settle_camera(&mut h);
    assert_eq!(selected_loops(h.state()), vec![order[1]]);
}

/// Two plates, one above the other: seen from above, the lower plate's
/// holes are hidden by the upper one.
fn two_plates() -> Mesh {
    let upper =
        mf_fixtures::translated(&plate(10, 10.0, &[(1, 1, 1), (7, 7, 1)]), [0.0, 0.0, 50.0]);
    let lower = plate(10, 10.0, &[(1, 7, 1), (7, 1, 1)]);
    mf_fixtures::merge(&[upper, lower])
}

#[test]
fn box_select_picks_only_visible_holes_inside_the_box() {
    require_gpu!("box_select_picks_only_visible_holes_inside_the_box");
    let mut h = opened(two_plates());
    // Look straight down at the plates.
    {
        let app = h.state_mut();
        let cam = app.camera.as_mut().unwrap();
        cam.target = DVec3::new(50.0, 50.0, 25.0);
        cam.look_from(DVec3::new(0.0, -0.01, 1.0));
        cam.distance = 400.0;
    }
    h.run();
    let topo = h.state().active_mesh().unwrap().topology();
    let hole_at = |x: f64, y: f64, z: f64| {
        topo.boundary_loops
            .iter()
            .find(|l| {
                let c = centre(&loop_points(h.state(), l.id));
                (c - DVec3::new(x, y, z)).length() < 1.0
            })
            .unwrap()
            .id
    };
    let upper_near = hole_at(15.0, 15.0, 50.0);
    let upper_far = hole_at(75.0, 75.0, 50.0);
    // A box around the upper plate's near hole and the lower plate's hole
    // at (15, 75) (hidden), not around the far one.
    let a = h.state().screen_point([5.0, 5.0, 50.0]).unwrap();
    let b = h.state().screen_point([40.0, 90.0, 50.0]).unwrap();
    key(&mut h, egui::Key::B, egui::Modifiers::NONE);
    h.event(egui::Event::PointerMoved(a));
    h.step();
    press(&mut h, a, true);
    for t in 1..=10 {
        h.event(egui::Event::PointerMoved(a + (b - a) * (t as f32 / 10.0)));
        h.step();
    }
    press(&mut h, b, false);
    wait_until(&mut h, "the box selection", |app| {
        !selected_loops(app).is_empty()
    });
    assert_eq!(selected_loops(h.state()), vec![upper_near]);
    assert!(!selected_loops(h.state()).contains(&upper_far));
    h.run();
    h.get_by_label("Fill hole");
    // Shift + drag adds.
    let c = h.state().screen_point([60.0, 60.0, 50.0]).unwrap();
    let d = h.state().screen_point([95.0, 95.0, 50.0]).unwrap();
    key(&mut h, egui::Key::B, egui::Modifiers::NONE);
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
    h.event(egui::Event::PointerMoved(c));
    h.step();
    h.event(egui::Event::PointerButton {
        pos: c,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::SHIFT,
    });
    h.step();
    for t in 1..=10 {
        h.event(egui::Event::PointerMoved(c + (d - c) * (t as f32 / 10.0)));
        h.step();
    }
    h.event(egui::Event::PointerButton {
        pos: d,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::SHIFT,
    });
    h.step();
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    wait_until(&mut h, "the added box", |app| {
        selected_loops(app).len() == 2
    });
    h.run();
    h.get_by_label("2 holes selected");
    h.get_by_label("Fill holes (2)");
}

#[test]
fn tiny_holes_get_screen_markers_when_zoomed_out() {
    require_gpu!("tiny_holes_get_screen_markers_when_zoomed_out");
    let mut h = opened(three_hundred_holes());
    // Markers are on by default (M4 final UX I6); switched off, the
    // overview brings them back with its tint.
    h.state_mut().show_hole_markers = false;
    h.run_steps(2);
    assert!(h.state().hole_markers().is_none(), "off when switched off");
    h.state_mut().show_hole_overview = true;
    h.state_mut().camera.as_mut().unwrap().distance *= 12.0;
    wait_until(&mut h, "markers", |a| {
        a.hole_markers().is_some_and(|m| !m.is_empty())
    });
    h.run();
    let markers = h.state().hole_markers().unwrap().to_vec();
    // Zoomed out, every one of the 300 small holes is under 4 px.
    assert_eq!(markers.len(), 300, "one marker per tiny hole");
    let view = h.state().viewport.last_rect.unwrap();
    assert!(markers.iter().all(|p| view.contains(*p)));
    // The faces around a hole carry the hole-neighbourhood tint.
    let hole = listed(h.state())
        .into_iter()
        .find(|d| matches!(d, Defect::Hole { edges: 4, .. }))
        .unwrap();
    let mesh = h.state().active_mesh().unwrap().clone();
    let topo = mesh.topology();
    let v = topo
        .boundary_loops
        .iter()
        .find(|l| l.id == loop_id(&hole))
        .unwrap()
        .vertices[0];
    let face = mesh.faces().find(|(_, t)| t.contains(&v)).unwrap().0;
    wait_until(&mut h, "the neighbourhood tint", |a| {
        a.read_face_state(face.0)
            .unwrap()
            .is_some_and(|s| s & FaceStates::HOLE_NEIGHBOURHOOD != 0)
    });
    // Zoomed in, the holes are big enough to see: no markers.
    h.state_mut().camera.as_mut().unwrap().distance /= 40.0;
    wait_until(&mut h, "markers gone", |a| {
        a.hole_markers().is_some_and(|m| m.len() < 300)
    });
}

/// MF-ISSUE-041: Esc that closes a menu doesn't also clear the selection.
#[test]
fn esc_in_a_popup_keeps_the_selection() {
    require_gpu!("esc_in_a_popup_keeps_the_selection");
    let mut h = opened(three_holes());
    let hole = listed(h.state())[0].clone();
    h.state_mut().select_defect(&hole);
    h.run();
    h.get_by_label("View").click();
    h.run();
    assert!(egui::Popup::is_any_open(&h.ctx), "the menu is open");
    key(&mut h, egui::Key::Escape, egui::Modifiers::NONE);
    h.run();
    assert!(!egui::Popup::is_any_open(&h.ctx), "Esc closed the menu");
    assert_eq!(selected_loops(h.state()), vec![loop_id(&hole)]);
    key(&mut h, egui::Key::Escape, egui::Modifiers::NONE);
    h.run();
    assert!(
        selected_loops(h.state()).is_empty(),
        "a second Esc clears it"
    );
}

/// F frames the selection with the same eased move as the tree.
#[test]
fn f_frames_the_selection_with_an_animated_move() {
    require_gpu!("f_frames_the_selection_with_an_animated_move");
    let mut h = opened(three_holes());
    let hole = listed(h.state())[0].clone();
    h.state_mut().select_defect(&hole);
    h.run();
    key(&mut h, egui::Key::F, egui::Modifiers::NONE);
    let moved = settle_camera(&mut h);
    assert!(moved > 3, "eased over several frames, not a jump ({moved})");
    let c = centre(&loop_points(h.state(), loop_id(&hole)));
    assert!(off_centre(h.state(), c) < 0.05);
}

/// Every parameter's tooltip is its spec description.
#[test]
fn every_operation_parameter_has_a_tooltip() {
    require_gpu!("every_operation_parameter_has_a_tooltip");
    let mut h = opened(three_holes());
    for spec in mf_spec::builtin::specs()
        .iter()
        .filter(|s| !s.is_read_only())
    {
        for p in &spec.parameters {
            assert!(!p.description.trim().is_empty(), "{}.{}", spec.id, p.name);
        }
        let Some(label) = mf_desktop::defects::operation_label(&spec.id, None) else {
            continue;
        };
        if !mf_desktop::tools::CATALOGUE.iter().any(|e| e.id == spec.id) {
            continue;
        }
        open_repair(&mut h);
        h.get_by_label(&label).click();
        h.run();
        for p in &spec.parameters {
            let name = mf_desktop::defects::param_label(&spec.id, &p.name);
            let at = h.get_by_label(&name).rect().center();
            h.event(egui::Event::PointerMoved(at));
            // Past egui's tooltip delay.
            for _ in 0..60 {
                h.step();
            }
            assert!(
                h.get_all_by_label(&p.description).next().is_some(),
                "{}.{}: hovering {name:?} shows {:?}",
                spec.id,
                p.name,
                p.description
            );
            h.event(egui::Event::PointerGone);
            h.run();
        }
        // Back to Repair's list.
        h.get_by_label("All repairs").click();
        h.run();
    }
}

/// Fix round 1, P2: before every object's picking BVH is built, a box
/// selection can't tell which holes are hidden, so it says so rather than
/// select hidden ones; the overview's markers wait too.
#[test]
fn box_select_waits_for_picking() {
    require_gpu!("box_select_waits_for_picking");
    // One background thread: the analysis runs first, the BVH after it.
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(|cc| {
            let settings = mf_desktop::Settings {
                threads: Some(1),
                ..mf_desktop::Settings::default()
            };
            MeshFixerApp::with_settings(cc, None, settings).unwrap()
        });
    h.state_mut().show_hole_overview = true;
    h.state_mut()
        .import_model(model(plate(500, 1.0, &[(10, 10, 1), (40, 40, 1)])));
    wait_until(&mut h, "the holes listed", |a| {
        a.hole_view().is_some() && a.camera.is_some() && a.viewport.last_rect.is_some()
    });
    assert!(!h.state().picking_ready(), "the BVH is still being built");
    assert!(h.state().hole_markers().is_none_or(|m| m.is_empty()));
    let view = h.state().viewport.last_rect.unwrap();
    let (a, b) = (
        view.left_top() + egui::vec2(5.0, 5.0),
        view.right_bottom() - egui::vec2(60.0, 60.0),
    );
    key(&mut h, egui::Key::B, egui::Modifiers::NONE);
    h.event(egui::Event::PointerMoved(a));
    h.step();
    press(&mut h, a, true);
    for t in 1..=5 {
        h.event(egui::Event::PointerMoved(a + (b - a) * (t as f32 / 5.0)));
        h.step();
    }
    press(&mut h, b, false);
    if h.state().picking_ready() {
        panic!("the BVH finished during the drag: make the plate bigger");
    }
    assert!(selected_loops(h.state()).is_empty());
    assert_eq!(
        h.state().status.as_deref(),
        Some("Picking isn't ready yet: try the box again in a moment")
    );
}

/// Fix round 1, P1: the tree scrolls to the row N / Shift+N selects.
#[test]
fn next_hole_scrolls_the_tree_to_its_row() {
    require_gpu!("next_hole_scrolls_the_tree_to_its_row");
    let mut h = opened(three_hundred_holes());
    let last = listed(h.state()).pop().unwrap();
    let label = row(h.state(), &last);
    assert!(
        h.query_by_label(&label).is_none(),
        "the last of 301 rows starts out of sight (not laid out)"
    );
    // Shift+N from nothing selected: the last hole.
    key(&mut h, egui::Key::N, egui::Modifiers::SHIFT);
    settle_camera(&mut h);
    h.run_steps(60);
    assert_eq!(selected_loops(h.state()), vec![loop_id(&last)]);
    let r = h.get_by_label(&label).rect();
    let tree = h.get_by_label("Browser").rect();
    let view = h.state().viewport.last_rect.unwrap();
    assert!(
        r.left() >= view.right() - 1.0 && r.top() >= tree.top() && r.bottom() <= view.bottom(),
        "the row is in view in the Browser: {r:?}"
    );
}

/// MF-OP-002 fill styles in Properties: each style is listed with a plain
/// one-line explanation, "Smooth (matches slope)" is preselected, and
/// choosing Flat instead gives a visibly different (flat) patch on a
/// curved surface.
#[test]
fn fill_style_choice_changes_the_patch() {
    require_gpu!("fill_style_choice_changes_the_patch");
    // A 20 mm ball with its top cut off: one round hole on a curved surface.
    let ball = mf_fixtures::uv_sphere(24, 32);
    let mut e = ball.edit();
    for (id, t) in ball.faces() {
        if t.iter().all(|&v| ball.corner_position(v)[2] > 0.8) {
            e.remove_face(id).unwrap();
        }
    }
    let mut h = opened(mf_fixtures::scaled(&e.finish().unwrap().0, 20.0));
    let hole = listed(h.state()).into_iter().next().unwrap();
    // Largest |distance from the centre − 20 mm| over the patch's new
    // vertices, after filling the hole with the style chosen in the UI.
    let fill_with = |h: &mut Harness<'static, MeshFixerApp>, label: &str| {
        h.state_mut().select_defect(&hole);
        // Selecting frames the hole with an animated move.
        h.run_steps(60);
        if h.state().active_tool != "repair" {
            h.get_by_label("Fill hole").click();
            h.run();
        }
        assert!(h.get_all_by_label("Fill hole").next().is_some());
        h.get_by_label(label).click();
        h.run_steps(2);
        let rev = h.state().session.current();
        h.get_by_label("Apply").click();
        wait_until(h, "the fill", |a| {
            a.session.current() != rev && a.busy.is_none() && a.hole_view().is_some()
        });
        let record = h.state().session.records_to_current().pop().unwrap();
        let m = h.state().active_mesh().unwrap().clone();
        let s = &h.state().session;
        let prov = &s.node(s.current()).unwrap().provenance[&h.state().active.unwrap()];
        let worst = prov
            .created_vertices
            .iter()
            .map(|&v| {
                let p = m.position(v).unwrap();
                ((p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt() - 20.0).abs()
            })
            .fold(0.0, f64::max);
        (record.params.0["fill_style"].clone(), worst)
    };
    h.state_mut().select_defect(&hole);
    h.run_steps(60);
    h.get_by_label("Fill hole").click();
    h.run();
    // Each style: its name, then one plain line saying what it does.
    for (value, label, words) in [
        ("flat", "Flat", "flat"),
        ("smooth", "Smooth (matches slope)", "without a crease"),
        ("curvature", "Curvature-matched", "curves"),
    ] {
        let (name, line) = choice_copy("mesh.fill_holes", "fill_style", value).unwrap();
        assert_eq!(name, label);
        assert!(line.contains(words), "{label}: {line}");
        h.get_by_label(label);
        h.get_by_label(line);
    }
    let preselected = h
        .get_by_label("Smooth (matches slope)")
        .accesskit_node()
        .toggled();
    assert_eq!(preselected, Some(egui::accesskit::Toggled::True));

    let (style, flat) = fill_with(&mut h, "Flat");
    assert_eq!(style, mf_spec::ParamValue::Text("flat".into()));
    let rev = h.state().session.current();
    h.get_by_label("Undo").click();
    wait_until(&mut h, "undo", |a| {
        a.session.current() != rev && a.hole_view().is_some()
    });
    let (style, smooth) = fill_with(&mut h, "Smooth (matches slope)");
    assert_eq!(style, mf_spec::ParamValue::Text("smooth".into()));
    eprintln!("20 mm ball: patch off the sphere by up to {flat:.3} mm flat, {smooth:.3} mm smooth");
    assert!(
        smooth < flat / 3.0,
        "flat {flat:.3} mm, smooth {smooth:.3} mm"
    );
}

/// MF-OP-002 (Task 12a review I1): Smooth and Curvature always refine, so
/// Properties shows Refine checked and greyed out, with a note saying why;
/// with Flat it is the user's choice again.
#[test]
fn smooth_styles_show_refine_as_always_on() {
    require_gpu!("smooth_styles_show_refine_as_always_on");
    let mut h = opened(three_holes());
    open_repair(&mut h);
    h.get_by_label("Fill all holes").click();
    h.run_steps(2);
    let note = "Smooth and Curvature always add points so the patch can curve";
    let refine = |h: &Harness<'static, MeshFixerApp>| {
        let node = h.get_by_label("Refine");
        let n = node.accesskit_node();
        (n.is_disabled(), n.toggled())
    };
    // Smooth, preselected: always on, disabled, explained.
    assert_eq!(
        refine(&h),
        (true, Some(egui::accesskit::Toggled::True)),
        "Refine with Smooth"
    );
    h.get_by_label(note);
    // Even if the setting underneath says off.
    h.state_mut()
        .params_for("mesh.fill_holes")
        .insert("refine".into(), mf_spec::ParamValue::Bool(false));
    h.run_steps(2);
    assert_eq!(refine(&h), (true, Some(egui::accesskit::Toggled::True)));
    h.get_by_label("Curvature-matched").click();
    h.run_steps(2);
    assert!(refine(&h).0, "Refine with Curvature");
    h.get_by_label(note);
    // Flat: the user's own setting, enabled, no note.
    h.get_by_label("Flat").click();
    h.run_steps(2);
    assert_eq!(refine(&h), (false, Some(egui::accesskit::Toggled::False)));
    assert!(h.query_by_label(note).is_none());
}

/// The batch fill (Fill all holes up to a size) uses the style chosen in
/// its options, here a non-default one, as one command.
#[test]
fn batch_fill_uses_the_chosen_style() {
    require_gpu!("batch_fill_uses_the_chosen_style");
    let mut h = opened(three_hundred_holes());
    open_repair(&mut h);
    h.get_by_label("Fill all holes").click();
    h.run_steps(3);
    h.get_by_label("Curvature-matched").click();
    h.run_steps(2);
    let rev = h.state().session.current();
    h.get_by_label("Apply").click();
    wait_until(&mut h, "the batch fill", |a| {
        a.session.current() != rev && a.busy.is_none() && a.hole_view().is_some()
    });
    let s = &h.state().session;
    assert_eq!(
        s.node(s.current()).unwrap().parent,
        Some(rev),
        "one command"
    );
    let record = s.records_to_current().pop().unwrap();
    assert_eq!(
        record.params.0["fill_style"],
        mf_spec::ParamValue::Text("curvature".into())
    );
    // The 300 holes and the 292-edge rim (under the 500-edge default).
    assert_eq!(record.selection.unwrap().ids.len(), 301);
}
