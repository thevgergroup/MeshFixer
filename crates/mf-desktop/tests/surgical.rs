#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Exit criterion 9 (headless): click one hole, fill only it, undo, change a
//! parameter, fill again, export 3MF.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::MeshFixerApp;
use mf_geometry::FaceId;
use mf_selection::SelectionKind;
use mf_spec::ParamValue;

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

fn loops(app: &MeshFixerApp) -> Option<usize> {
    app.current_analysis()
        .map(|a| a.diagnostics.boundary_loops.len())
}

/// ID of the 3-edge loop through vertex `v` in the active mesh, if traced.
fn hole_through(app: &MeshFixerApp, v: u32) -> Option<u32> {
    let mesh = app.active_mesh()?;
    let topo = mesh.topology();
    topo.boundary_loops
        .iter()
        .find(|l| l.vertices.contains(&v) && l.edge_count == 3)
        .map(|l| l.id)
}

fn two_hole_grid(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let n = 6;
    let mut e = mf_fixtures::grid(n).edit();
    e.remove_face(FaceId(2 * (n + 1))).unwrap(); // cell (1,1): hole at vertex (1,1) = 8
    e.remove_face(FaceId(2 * (4 * n + 4))).unwrap(); // cell (4,4): hole at vertex (4,4) = 32
    let (m, _) = e.finish().unwrap();
    mf_io::write_path(&m, &path, mf_io::Format::Obj).unwrap();
    path
}

fn save(h: &mut Harness<'_, MeshFixerApp>, name: &str) {
    if let Ok(dir) = std::env::var("MF_ARTIFACTS") {
        h.render()
            .unwrap()
            .save(std::path::Path::new(&dir).join(format!("{name}.png")))
            .unwrap();
    }
}

/// Click at the screen position of the midpoint of the first edge of the
/// boundary loop containing grid vertex `v`, and wait until something is
/// selected.
fn click_hole(h: &mut Harness<'_, MeshFixerApp>, v: u32) {
    let mesh = h.state().active_mesh().unwrap().clone();
    let topo = mesh.topology();
    let l = topo
        .boundary_loops
        .iter()
        .find(|l| l.vertices.contains(&v) && l.edge_count == 3)
        .unwrap();
    let (a, b) = (
        mesh.corner_position(l.vertices[0]),
        mesh.corner_position(l.vertices[1]),
    );
    let mid = [0, 1, 2].map(|k| (a[k] + b[k]) / 2.0);
    let pos = h.state().screen_point(mid).unwrap();
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
    // Picking runs on a worker; wait for its result.
    wait_until(h, "pick result", |a| a.selection.is_some());
}

#[test]
fn select_hole_fill_undo_refill() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("select_hole_fill_undo_refill");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = two_hole_grid(dir.path(), "two_holes.obj");

    let mut h = Harness::builder()
        .with_size((1440.0, 900.0))
        .wgpu()
        .build_eframe(|cc| MeshFixerApp::new(cc, Some(path.clone())).unwrap());
    wait_until(&mut h, "import + analysis", |a| loops(a).is_some());
    h.run();
    assert_eq!(loops(h.state()), Some(3), "rim + two holes");

    // 1. Click the hole at vertex 8: it becomes the selection.
    click_hole(&mut h, 8);
    let sel = h
        .state()
        .selection
        .clone()
        .expect("a click on the hole selects it");
    assert_eq!(sel.kind, SelectionKind::BoundaryLoop);
    assert_eq!(
        sel.ids,
        vec![hole_through(h.state(), 8).unwrap()],
        "the clicked hole, not the rim or the other hole"
    );
    assert_eq!(h.state().highlighted_loops(), Some(sel.ids.clone()));
    save(&mut h, "01-hole-selected");

    // 2. Fill only that hole (no refinement). Properties offers the one
    // operation valid for a hole, already open.
    h.get_by_label("Fill hole");
    let params = h.state_mut().params_for("mesh.fill_holes");
    params.insert("refine".into(), ParamValue::Bool(false));
    params.insert("fill_style".into(), ParamValue::Text("flat".into()));
    h.run_steps(2);
    let before = h.state().session.current();
    h.get_by_label("Apply").click();
    wait_until(&mut h, "fill + analysis", |a| {
        a.session.current() != before && loops(a) == Some(2)
    });
    assert!(h.state().error.is_none(), "{:?}", h.state().error);
    assert!(
        h.state().selection.is_none(),
        "the filled loop's selection is cleared"
    );
    assert!(
        h.state()
            .status
            .as_deref()
            .unwrap_or("")
            .contains("Hole filled"),
        "{:?}",
        h.state().status
    );
    assert_eq!(
        hole_through(h.state(), 8),
        None,
        "the clicked hole is closed"
    );
    assert!(
        hole_through(h.state(), 32).is_some(),
        "the other hole is still open and traced"
    );
    save(&mut h, "02-one-hole-filled");

    // 3. Undo: both holes are open again.
    h.get_by_label("Undo").click();
    wait_until(&mut h, "undo", |a| loops(a) == Some(3));

    // 4. Change a parameter and fill again: refinement adds vertices.
    click_hole(&mut h, 8);
    assert!(h.state().selection.is_some());
    h.state_mut()
        .params_for("mesh.fill_holes")
        .insert("refine".into(), ParamValue::Bool(true));
    h.run_steps(2);
    let before = h.state().session.current();
    h.get_by_label("Apply").click();
    wait_until(&mut h, "refined fill", |a| {
        a.session.current() != before && loops(a) == Some(2)
    });
    let current = h.state().session.current();
    let node = h.state().session.node(current).unwrap();
    let record = node.record.as_ref().unwrap();
    assert_eq!(
        record.selection.as_ref().unwrap().kind,
        SelectionKind::BoundaryLoop
    );
    save(&mut h, "03-refilled");

    // 5. Export 3MF (the File > Export path, without the dialog).
    let out = dir.path().join("repaired.3mf");
    mf_io::write_model_path(&h.state().document_model().unwrap(), &out).unwrap();
    let back = mf_io::read_model(&out).unwrap();
    assert_eq!(back.objects.len(), 1);
    assert_eq!(
        mf_geometry::inspect(&back.objects[0].mesh)
            .boundary_loops
            .len(),
        2
    );
}

#[test]
fn selection_belongs_to_the_active_object() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("selection_belongs_to_the_active_object");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let first = two_hole_grid(dir.path(), "first.obj");
    let second = two_hole_grid(dir.path(), "second.obj");
    let mut h = Harness::builder()
        .with_size((1440.0, 900.0))
        .wgpu()
        .build_eframe(|cc| MeshFixerApp::new(cc, Some(first.clone())).unwrap());
    wait_until(&mut h, "import + analysis", |a| loops(a).is_some());
    h.run();
    click_hole(&mut h, 8);
    let sel = h.state().selection.clone().expect("hole selected");
    let first_id = sel.object;

    // Opening another file makes it active: the old selection (another
    // object's loop IDs) must not be drawn on it or applied to it.
    h.state_mut().open(second);
    wait_until(&mut h, "second import + analysis", |a| {
        a.active != Some(first_id) && loops(a).is_some()
    });
    assert!(h.state().selection.is_none(), "{:?}", h.state().selection);
    assert_eq!(h.state().highlighted_loops(), None);

    // Switching back by clicking the object list doesn't resurrect it.
    h.state_mut().set_active(first_id);
    h.run_steps(3);
    assert!(h.state().selection.is_none());

    // A selection for a different object is never highlighted.
    h.state_mut().selection = Some(mf_selection::Selection {
        object: mf_command::ObjectId(first_id.0 + 1),
        ..sel
    });
    assert_eq!(h.state().highlighted_loops(), None);
}
