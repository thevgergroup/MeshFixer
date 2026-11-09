#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Layout and visual language (M4 Task 2): the viewport dominates, the left
//! tree lists the model's defects, the right panel is contextual to the
//! current target, the bottom breadcrumb jumps through history, and the
//! camera answers to mouse and trackpad the way the Help ▸ Controls says.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use mf_desktop::MeshFixerApp;
use mf_geometry::FaceId;

const WINDOW: (f32, f32) = (1440.0, 900.0);

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

/// A 6×6 grid with two one-cell holes (plus its open rim).
fn two_hole_grid(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("two_holes.obj");
    let n = 6;
    let mut e = mf_fixtures::grid(n).edit();
    e.remove_face(FaceId(2 * (n + 1))).unwrap();
    e.remove_face(FaceId(2 * (4 * n + 4))).unwrap();
    let (m, _) = e.finish().unwrap();
    mf_io::write_path(&m, &path, mf_io::Format::Obj).unwrap();
    path
}

fn open(path: std::path::PathBuf) -> Harness<'static, MeshFixerApp> {
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::new(cc, Some(path.clone())).unwrap());
    wait_until(&mut h, "import + analysis", |a| {
        a.current_analysis().is_some()
    });
    // Settle so the accessibility check runs.
    h.run();
    h
}

macro_rules! require_gpu {
    ($name:literal) => {
        if !mf_render::offscreen::gpu_available() {
            mf_fixtures::skip::gpu_unavailable($name);
            return;
        }
    };
}

#[test]
fn layout_has_viewport_tree_properties_and_history() {
    require_gpu!("layout_has_viewport_tree_properties_and_history");
    let dir = tempfile::tempdir().unwrap();
    let h = open(two_hole_grid(dir.path()));

    // The three named regions exist.
    let tree = h.get_by_label("Objects & Defects").rect();
    let props = h.get_by_label("Properties & Tools").rect();
    let view = h.state().viewport.last_rect.expect("the 3D view is drawn");
    assert!(
        tree.right() <= view.left() + 1.0,
        "tree {tree:?} is left of the view {view:?}"
    );
    assert!(
        props.left() >= view.right() - 1.0,
        "properties {props:?} are right of the view {view:?}"
    );

    // The tree counts every defect category of the model.
    for row in [
        "Holes (3)",
        "Narrow cracks (0)",
        "Loose pieces (0)",
        "Non-manifold edges (0)",
    ] {
        h.get_by_label(row);
    }

    // The history breadcrumb sits below the view.
    let crumb = h.get_by_label("Import two_holes").rect();
    assert!(
        crumb.top() >= view.bottom() - 1.0,
        "breadcrumb {crumb:?} is below the view {view:?}"
    );

    // The viewport dominates: at least 60% of the window.
    let share = view.area() / (WINDOW.0 * WINDOW.1);
    assert!(
        share >= 0.60,
        "viewport is {:.1}% of the window",
        share * 100.0
    );

    // No operation drop-down: operations come from the target.
    assert!(h.query_by_label("Operation").is_none());
}

#[test]
fn clicking_a_target_shows_its_operations_in_properties() {
    require_gpu!("clicking_a_target_shows_its_operations_in_properties");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(two_hole_grid(dir.path()));

    // Nothing selected: Properties shows the whole object and its tools.
    h.get_by_label("Fill all holes");
    h.get_by_label("Remove loose pieces");
    assert!(h.query_by_label("Fill hole").is_none());

    // Click one of the one-cell holes in the tree: it becomes the target.
    let label = h
        .state()
        .current_analysis()
        .unwrap()
        .defects
        .holes
        .iter()
        .find(|d| matches!(d, mf_desktop::defects::Defect::Hole { edges: 3, .. }))
        .map(|d| mf_desktop::defects::defect_copy(d, mf_io::threemf::Unit::Millimeter).row)
        .expect("a 3-edge hole");
    let row = h.get_by_label(&label).rect();
    h.event(egui::Event::PointerMoved(row.center()));
    h.event(egui::Event::PointerButton {
        pos: row.center(),
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.event(egui::Event::PointerButton {
        pos: row.center(),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
    let sel = h.state().selection.clone().expect("the hole is selected");
    assert_eq!(sel.kind, mf_selection::SelectionKind::BoundaryLoop);

    // Properties now describes the hole in plain language, offers the
    // operation valid for it, and nothing for the whole object.
    h.get_by_label("3 boundary edges · 3.41 mm perimeter");
    h.get_by_label("Fill hole");
    h.get_by_label("Apply");
    assert!(h.query_by_label("Fill all holes").is_none());
    assert!(h.query_by_label("Remove loose pieces").is_none());

    // Apply fills exactly that hole.
    let before = h.state().session.current();
    h.get_by_label("Apply").click();
    wait_until(&mut h, "fill + analysis", |a| {
        a.session.current() != before
            && a.current_analysis()
                .is_some_and(|x| x.diagnostics.boundary_loops.len() == 2)
    });
    assert!(h.state().error.is_none(), "{:?}", h.state().error);
}

#[test]
fn history_breadcrumb_jumps_to_a_revision() {
    require_gpu!("history_breadcrumb_jumps_to_a_revision");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(two_hole_grid(dir.path()));
    let imported = h.state().session.current();

    h.get_by_label("Fill all holes").click();
    h.run();
    h.get_by_label("Apply").click();
    wait_until(&mut h, "fill all + analysis", |a| {
        a.session.current() != imported && a.current_analysis().is_some()
    });
    let filled = h.state().session.current();

    // Both steps are in the breadcrumb, the current one highlighted.
    assert_eq!(crumb_selected(&h, "Fill all holes"), Some(true));
    assert_eq!(crumb_selected(&h, "Import two_holes"), Some(false));

    // Click the import step: the document jumps back to it.
    click_crumb(&mut h, "Import two_holes");
    assert_eq!(h.state().session.current(), imported);
    assert_eq!(crumb_selected(&h, "Import two_holes"), Some(true));
    assert_eq!(crumb_selected(&h, "Fill all holes"), Some(false));

    // And forward again (the later step stays reachable, greyed).
    click_crumb(&mut h, "Fill all holes");
    assert_eq!(h.state().session.current(), filled);
}

/// Click the tree row of the first one-cell (3-edge) hole and return it.
fn click_small_hole_row(h: &mut Harness<'_, MeshFixerApp>) -> egui::Pos2 {
    let label = h
        .state()
        .current_analysis()
        .unwrap()
        .defects
        .holes
        .iter()
        .find(|d| matches!(d, mf_desktop::defects::Defect::Hole { edges: 3, .. }))
        .map(|d| mf_desktop::defects::defect_copy(d, mf_io::threemf::Unit::Millimeter).row)
        .expect("a 3-edge hole");
    let pos = h.get_by_label(&label).rect().center();
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
    h.run();
    pos
}

/// A new target clears a status that described an earlier one (fix round
/// 1, I1): the status line never contradicts the screen.
#[test]
fn selecting_a_target_clears_a_stale_status() {
    require_gpu!("selecting_a_target_clears_a_stale_status");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(two_hole_grid(dir.path()));
    h.state_mut().status = Some("Hole filled (10 faces); selection cleared".into());
    click_small_hole_row(&mut h);
    assert!(h.state().selection.is_some());
    assert_eq!(h.state().status, None);
    h.get_by_label("Ready");
}

/// Right after a click the pointer is still on the row, so hover and
/// selection name the same hole: it must show the selection colour, not
/// the hover outline (fix round 1, I2).
#[test]
fn hover_does_not_hide_the_selection() {
    require_gpu!("hover_does_not_hide_the_selection");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(two_hole_grid(dir.path()));
    click_small_hole_row(&mut h);
    wait_until(&mut h, "selection overlay", |a| {
        a.view_layers()
            .iter()
            .any(|l| l.color == mf_desktop::theme::SELECTED)
    });
    let colors: Vec<_> = h.state().view_layers().iter().map(|l| l.color).collect();
    assert!(
        !colors.contains(&mf_desktop::theme::HOVER),
        "hover drawn over the selection: {colors:?}"
    );
}

/// Cyan means "selected" only (fix round 1, P2): with a hole selected, the
/// Properties panel (which has no selected widget) has no cyan in it.
#[test]
fn properties_use_the_selection_colour_for_nothing() {
    require_gpu!("properties_use_the_selection_colour_for_nothing");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(two_hole_grid(dir.path()));
    click_small_hole_row(&mut h);
    h.get_by_label("Fill hole");
    let props = h.get_by_label("Properties & Tools").rect();
    let image = h.render().unwrap();
    let c = mf_desktop::theme::SELECTED;
    let cyan = (props.left() as u32..WINDOW.0 as u32)
        .flat_map(|x| (props.top() as u32..props.top() as u32 + 400).map(move |y| (x, y)))
        .filter(|&(x, y)| {
            let p = image.get_pixel(x, y).0;
            (0..3).all(|k| (i32::from(p[k]) - i32::from([c.r(), c.g(), c.b()][k])).abs() < 40)
        })
        .count();
    assert_eq!(cyan, 0, "{cyan} cyan pixels in Properties");
}

/// The breadcrumb step labelled `label` (below the 3D view; a tool of the
/// same name may also be in the Properties panel).
fn crumb_rect(h: &Harness<'_, MeshFixerApp>, label: &str) -> (egui::Rect, Option<bool>) {
    let view = h.state().viewport.last_rect.unwrap();
    let found: Vec<_> = h
        .get_all_by_label(label)
        .filter(|n| n.rect().top() >= view.bottom() - 1.0)
        // egui reports a selectable button's state as accesskit "toggled".
        .map(|n| {
            let on = n
                .accesskit_node()
                .toggled()
                .map(|t| t == egui::accesskit::Toggled::True);
            (n.rect(), on)
        })
        .collect();
    assert_eq!(found.len(), 1, "one breadcrumb step {label:?}");
    found[0]
}

fn crumb_selected(h: &Harness<'_, MeshFixerApp>, label: &str) -> Option<bool> {
    crumb_rect(h, label).1
}

fn click_crumb(h: &mut Harness<'_, MeshFixerApp>, label: &str) {
    let pos = crumb_rect(h, label).0.center();
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
    h.run();
}

fn wheel(unit: egui::MouseWheelUnit, delta: egui::Vec2, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::MouseWheel {
        unit,
        delta,
        phase: egui::TouchPhase::Move,
        source: match unit {
            egui::MouseWheelUnit::Point => egui::MouseWheelSource::Trackpad,
            _ => egui::MouseWheelSource::Wheel,
        },
        modifiers,
    }
}

#[test]
fn pinch_zooms_and_two_finger_drag_orbits() {
    require_gpu!("pinch_zooms_and_two_finger_drag_orbits");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(two_hole_grid(dir.path()));
    let view = h.state().viewport.last_rect.unwrap();
    h.event(egui::Event::PointerMoved(view.center()));
    h.run();
    let start = h.state().camera.unwrap();

    // Pinch (zoom gesture) zooms in.
    h.event(egui::Event::Zoom(1.25));
    h.run();
    let pinched = h.state().camera.unwrap();
    assert!(
        pinched.distance < start.distance * 0.9,
        "pinch zooms in: {} -> {}",
        start.distance,
        pinched.distance
    );
    assert_eq!((pinched.yaw, pinched.pitch), (start.yaw, start.pitch));

    // Two-finger drag (trackpad scroll, in points) orbits, without zooming.
    h.event(wheel(
        egui::MouseWheelUnit::Point,
        egui::vec2(40.0, 20.0),
        egui::Modifiers::NONE,
    ));
    h.run();
    let orbited = h.state().camera.unwrap();
    assert_ne!(orbited.yaw, pinched.yaw, "two-finger drag orbits");
    assert_ne!(orbited.pitch, pinched.pitch, "two-finger drag orbits");
    assert_eq!(orbited.distance, pinched.distance, "and doesn't zoom");
    assert_eq!(orbited.target, pinched.target, "and doesn't pan");

    // Shift + two-finger drag pans.
    h.event(wheel(
        egui::MouseWheelUnit::Point,
        egui::vec2(40.0, 0.0),
        egui::Modifiers::SHIFT,
    ));
    h.run();
    let panned = h.state().camera.unwrap();
    assert_ne!(
        panned.target, orbited.target,
        "shift + two-finger drag pans"
    );
    assert_eq!((panned.yaw, panned.pitch), (orbited.yaw, orbited.pitch));
    assert_eq!(panned.distance, orbited.distance);

    // A mouse wheel (in lines) zooms towards the cursor: off-centre, the
    // target moves towards the point under it.
    let off = view.center() + egui::vec2(view.width() * 0.25, 0.0);
    h.event(egui::Event::PointerMoved(off));
    h.event(wheel(
        egui::MouseWheelUnit::Line,
        egui::vec2(0.0, 2.0),
        egui::Modifiers::NONE,
    ));
    h.run();
    let wheeled = h.state().camera.unwrap();
    assert!(wheeled.distance < panned.distance, "wheel up zooms in");
    assert_eq!((wheeled.yaw, wheeled.pitch), (panned.yaw, panned.pitch));
    assert_ne!(wheeled.target, panned.target, "zoom is towards the cursor");

    // F with nothing selected frames everything, keeping the orientation:
    // the document's centre comes back to the middle of the view.
    h.event(egui::Event::Key {
        key: egui::Key::F,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
    let framed = h.state().camera.unwrap();
    assert_eq!((framed.yaw, framed.pitch), (wheeled.yaw, wheeled.pitch));
    let centre = h.state().screen_point([3.0, 3.0, 0.0]).unwrap();
    let view = h.state().viewport.last_rect.unwrap();
    assert!(
        (centre - view.center()).length() < 2.0,
        "F centres the document: {centre:?} vs {:?}",
        view.center()
    );

    // Home resets the view.
    h.event(egui::Event::Key {
        key: egui::Key::Home,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
    let home = h.state().camera.unwrap();
    assert_eq!((home.yaw, home.pitch), (start.yaw, start.pitch));
    assert!((home.distance - start.distance).abs() < 1e-9 * start.distance);
}
