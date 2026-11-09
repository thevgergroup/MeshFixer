#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Diagnostic overlays (MF-RENDER-005) and selection tints through the
//! per-face state buffer (MF-RENDER-003): non-manifold edges and small
//! loose pieces are highlighted in the view and can be clicked.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::MeshFixerApp;
use mf_desktop::defects::Defect;
use mf_render::FaceStates;
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
            start.elapsed() < Duration::from_secs(20),
            "timed out waiting for: {what}"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
}

fn open(path: std::path::PathBuf) -> Harness<'static, MeshFixerApp> {
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::new(cc, Some(path.clone())).unwrap());
    wait_until(&mut h, "import + analysis", |a| {
        a.current_analysis().is_some() && a.shows_current_revision()
    });
    h.run();
    h
}

fn write(dir: &std::path::Path, name: &str, mesh: &mf_geometry::Mesh) -> std::path::PathBuf {
    let path = dir.join(name);
    mf_io::write_path(mesh, &path, mf_io::Format::Obj).unwrap();
    path
}

fn click_at(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2) {
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
    h.step();
}

/// The state bits the GPU holds for `face` of the active object.
fn state(app: &MeshFixerApp, face: u32) -> u8 {
    app.read_face_state(face).unwrap().unwrap()
}

/// The colour at `pos`, with the pointer moved out of the way (it's drawn).
fn pixel(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2) -> [i32; 3] {
    h.event(egui::Event::PointerGone);
    h.run_steps(2);
    let image = h.render().unwrap();
    let p = image.get_pixel(pos.x as u32, pos.y as u32).0;
    [0, 1, 2].map(|k| i32::from(p[k]))
}

/// Amber whatever the lighting: red and green well above blue.
fn amber(p: [i32; 3]) -> bool {
    p[0] * 10 > p[2] * 16 && p[1] * 10 > p[2] * 14
}

/// Cyan whatever the lighting: blue and green well above red.
fn cyan(p: [i32; 3]) -> bool {
    p[2] * 10 > p[0] * 16 && p[1] * 10 > p[0] * 14
}

#[test]
fn clicking_a_non_manifold_edge_selects_its_faces() {
    require_gpu!("clicking_a_non_manifold_edge_selects_its_faces");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(write(
        dir.path(),
        "plate.obj",
        &mf_fixtures::plate_with_defects(),
    ));
    let (edge, faces) = match &h.state().current_analysis().unwrap().defects.tangled[..] {
        [Defect::Tangled { edge, faces }] => (*edge, faces.clone()),
        other => panic!("one tangled edge: {other:?}"),
    };
    assert_eq!(faces.len(), 3, "the fin and the two plate faces");
    // Its faces are tinted as next to a tangled edge.
    wait_until(&mut h, "non-manifold tint", |a| {
        faces
            .iter()
            .all(|&f| state(a, f) & FaceStates::NON_MANIFOLD_ADJ != 0)
    });

    let mesh = h.state().active_mesh().unwrap().clone();
    let (a, b) = (mesh.corner_position(edge[0]), mesh.corner_position(edge[1]));
    // A third of the way along: clear of the fin's own boundary edges,
    // which meet the tangled edge at its ends.
    let at = [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) / 3.0);
    let pos = h.state().screen_point(at).unwrap();
    click_at(&mut h, pos);
    wait_until(&mut h, "pick result", |a| a.selection.is_some());

    let sel = h.state().selection.clone().unwrap();
    assert_eq!(sel.kind, SelectionKind::Faces, "{sel:?}");
    let mut expected = faces.clone();
    expected.sort_unstable();
    assert_eq!(sel.ids, expected, "the edge's incident faces");
    wait_until(&mut h, "selection tint", |a| {
        faces
            .iter()
            .all(|&f| state(a, f) & FaceStates::SELECTED != 0)
    });
    // Properties names what's selected.
    h.get_by_label("3 faces selected");
}

#[test]
fn small_components_are_tinted_and_clickable() {
    require_gpu!("small_components_are_tinted_and_clickable");
    let dir = tempfile::tempdir().unwrap();
    // The model (a 10×10 plate, 200 faces) and a 12-face loose cube beside
    // it: under remove_small_components' 100-triangle default.
    let plate = mf_fixtures::grid(10);
    let cube = mf_fixtures::translated(&mf_fixtures::cube(4.0), [14.0, 3.0, 0.0]);
    let mut h = open(write(
        dir.path(),
        "plate_and_cube.obj",
        &mf_fixtures::merge(&[plate, cube]),
    ));
    let mesh = h.state().active_mesh().unwrap().clone();
    let topo = mesh.topology();
    assert_eq!(topo.components.len(), 2);
    let piece = topo.components[1].clone();
    assert_eq!(piece.len(), 12);
    wait_until(&mut h, "small-piece tint", |a| {
        piece
            .iter()
            .all(|&f| state(a, f) & FaceStates::SMALL_COMPONENT != 0)
    });
    assert!(
        topo.components[0]
            .iter()
            .all(|&f| state(h.state(), f) & FaceStates::SMALL_COMPONENT == 0),
        "the model isn't tinted"
    );

    // The cube's top face, seen from the default view, is amber.
    let top = h.state().screen_point([16.0, 5.0, 4.0]).unwrap();
    let p = pixel(&mut h, top);
    assert!(amber(p), "the loose piece is tinted amber: {p:?}");

    // Its legend entry hides the tint, and shows it again.
    h.get_by_label("Loose pieces").click();
    h.run_steps(3);
    let p = pixel(&mut h, top);
    assert!(!amber(p), "hidden: {p:?}");
    h.get_by_label("Loose pieces").click();
    h.run_steps(3);
    assert!(amber(pixel(&mut h, top)));

    // Clicking it selects the piece, which then shows the selection colour.
    click_at(&mut h, top);
    wait_until(&mut h, "pick result", |a| a.selection.is_some());
    let sel = h.state().selection.clone().unwrap();
    assert_eq!(
        (sel.kind, sel.ids.clone()),
        (SelectionKind::Component, vec![1])
    );
    wait_until(&mut h, "selection tint", |a| {
        piece
            .iter()
            .all(|&f| state(a, f) & FaceStates::SELECTED != 0)
    });
    h.run_steps(3);
    // (Properties now shows the piece's tool: the view may have moved.)
    let top = h.state().screen_point([16.0, 5.0, 4.0]).unwrap();
    let p = pixel(&mut h, top);
    assert!(cyan(p), "the selected piece is cyan: {p:?}");
    h.get_by_label("Remove piece");
}
