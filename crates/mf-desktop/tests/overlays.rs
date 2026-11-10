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
    mf_io::write_path(
        mesh,
        &path,
        mf_io::Format::Obj,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();
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
    h.get_by_label("3 triangles selected");
}

#[test]
fn small_components_are_tinted_and_clickable() {
    require_gpu!("small_components_are_tinted_and_clickable");
    let dir = tempfile::tempdir().unwrap();
    // The model (a 10×10 plate, 200 faces) and a 12-face loose cube beside
    // it.
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
            .all(|&f| state(a, f) & FaceStates::LOOSE_PIECE != 0)
    });
    assert!(
        topo.components[0]
            .iter()
            .all(|&f| state(h.state(), f) & FaceStates::LOOSE_PIECE == 0),
        "the model isn't tinted"
    );

    // The cube's top face, seen from the default view, is amber.
    let top = h.state().screen_point([16.0, 5.0, 4.0]).unwrap();
    let p = pixel(&mut h, top);
    assert!(amber(p), "the loose piece is tinted amber: {p:?}");

    // Its entry in the Overlays popover hides the tint, and shows it
    // again.
    h.get_by_label("Overlays").click();
    h.run_steps(3);
    h.get_by_label("Loose pieces").click();
    h.run_steps(3);
    let p = pixel(&mut h, top);
    assert!(!amber(p), "hidden: {p:?}");
    h.get_by_label("Loose pieces").click();
    h.run_steps(3);
    assert!(amber(pixel(&mut h, top)));
    h.get_by_label("Overlays").click();
    h.run_steps(3);

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

/// "Loose pieces" means one set everywhere (fix round 1, I1): every piece
/// but the main body is listed in the tree and tinted in the view, however
/// big; the remove tool's size limit doesn't decide what is a loose piece.
#[test]
fn every_loose_piece_is_listed_and_tinted() {
    require_gpu!("every_loose_piece_is_listed_and_tinted");
    let dir = tempfile::tempdir().unwrap();
    // The model (200 faces) and a 150-triangle loose sphere: over
    // remove_small_components' 100-triangle default.
    let sphere = mf_fixtures::translated(
        &mf_fixtures::scaled(&mf_fixtures::uv_sphere(6, 15), 2.0),
        [15.0, 5.0, 2.0],
    );
    assert_eq!(sphere.triangle_count(), 150);
    let mut h = open(write(
        dir.path(),
        "plate_and_sphere.obj",
        &mf_fixtures::merge(&[mf_fixtures::grid(10), sphere]),
    ));
    let defects = h.state().current_analysis().unwrap().defects.clone();
    assert_eq!(defects.loose.len(), 1);
    h.get_by_label(&copy_of(h.state(), &defects.loose[0]).row);
    let mesh = h.state().active_mesh().unwrap().clone();
    let piece = mesh.topology().components[1].clone();
    assert_eq!(piece.len(), 150);
    wait_until(&mut h, "loose-piece tint", |a| {
        piece
            .iter()
            .all(|&f| state(a, f) & FaceStates::LOOSE_PIECE != 0)
    });
}

/// A selection change in the app re-uploads no geometry (fix round 1,
/// P1): the render mesh on screen stays the same, its upload totals don't
/// move, and only its face states change.
#[test]
fn selecting_in_the_view_uploads_no_geometry() {
    require_gpu!("selecting_in_the_view_uploads_no_geometry");
    let dir = tempfile::tempdir().unwrap();
    let cube = mf_fixtures::translated(&mf_fixtures::cube(4.0), [14.0, 3.0, 0.0]);
    let mut h = open(write(
        dir.path(),
        "plate_and_cube.obj",
        &mf_fixtures::merge(&[mf_fixtures::grid(10), cube]),
    ));
    let shown = |app: &MeshFixerApp| {
        app.object_views()
            .into_iter()
            .find(|v| v.active)
            .unwrap()
            .render
    };
    let before = shown(h.state());
    let (total, last, generation) = (
        before.gpu.total_uploaded(),
        before.gpu.last_upload(),
        before.gpu.state_generation(),
    );
    let piece = h.state().active_mesh().unwrap().topology().components[1].clone();
    let uploaded = h.state().geometry_bytes_uploaded();
    assert!(uploaded > 0, "the model's geometry was counted");

    let top = h.state().screen_point([16.0, 5.0, 4.0]).unwrap();
    click_at(&mut h, top);
    wait_until(&mut h, "selection tint", |a| {
        a.selection.is_some()
            && piece
                .iter()
                .all(|&f| state(a, f) & FaceStates::SELECTED != 0)
    });
    // Let anything the click set off finish (an analysis, a rebuild).
    wait_until(&mut h, "idle", |a| {
        !a.is_analyzing() && a.shows_current_revision()
    });
    h.run_steps(10);
    let after = shown(h.state());
    assert!(
        std::sync::Arc::ptr_eq(&before, &after),
        "the same render mesh is on screen: no geometry was rebuilt"
    );
    assert_eq!(after.gpu.total_uploaded(), total, "no geometry uploaded");
    assert_eq!(
        h.state().geometry_bytes_uploaded(),
        uploaded,
        "no render mesh was rebuilt anywhere"
    );
    assert_eq!(after.gpu.last_upload(), last);
    assert!(
        after.gpu.state_generation() > generation,
        "the face states changed"
    );
}

/// Moves the pointer to `pos` (no button) and lets a frame see it.
fn point_at(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    h.step();
}

/// The words the tooltip uses for the active object's `defect`.
fn copy_of(app: &MeshFixerApp, defect: &Defect) -> mf_desktop::defects::DefectCopy {
    mf_desktop::defects::defect_copy(defect, app.session.document().units())
}

/// MF-RENDER-004: hovering a hole in the view outlines it (the hover
/// line layer) and a tooltip gives its size, in the tree's words; moving
/// off the model clears both.
#[test]
fn hovering_a_hole_highlights_it_and_shows_its_size() {
    require_gpu!("hovering_a_hole_highlights_it_and_shows_its_size");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(write(
        dir.path(),
        "open_box.obj",
        &mf_fixtures::scaled(&mf_fixtures::cube_with_missing_face(), 10.0),
    ));
    wait_until(&mut h, "picking ready", MeshFixerApp::picking_ready);
    let hole = h.state().current_analysis().unwrap().defects.holes[0].clone();
    let Defect::Hole { loop_id, .. } = hole else {
        panic!("a fillable hole: {hole:?}");
    };
    let copy = copy_of(h.state(), &hole);
    assert!(copy.detail.contains("4 boundary edges"), "{copy:?}");
    assert!(copy.detail.contains("40.0 mm perimeter"), "{copy:?}");

    // A third of the way along one of its edges.
    let mesh = h.state().active_mesh().unwrap().clone();
    let topo = mesh.topology();
    let l = topo
        .boundary_loops
        .iter()
        .find(|l| l.id == loop_id)
        .unwrap();
    let (a, b) = (
        mesh.corner_position(l.vertices[0]),
        mesh.corner_position(l.vertices[1]),
    );
    let at = [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) / 3.0);
    let pos = h.state().screen_point(at).unwrap();
    point_at(&mut h, pos);
    wait_until(&mut h, "hover outline", |a| {
        a.view_hover()
            .is_some_and(|v| v.target == mf_desktop::worker::OverlayTarget::Loops(vec![loop_id]))
            && a.view_layers()
                .iter()
                .any(|l| l.color == mf_desktop::theme::HOVER)
    });
    // The tooltip: the hole's title and its size (its tree row gives the
    // size too).
    h.get_by_label(&copy.title);
    h.get_by_label(&copy.row);
    h.get_by_label(&copy.detail);
    assert!(h.state().selection.is_none(), "hovering selects nothing");

    // Off the model: no hover, no tooltip.
    let view = h.state().viewport.last_rect.unwrap();
    point_at(&mut h, view.left_top() + egui::vec2(20.0, 20.0));
    wait_until(&mut h, "hover cleared", |a| a.view_hover().is_none());
    assert!(h.query_by_label(&copy.detail).is_none());
    assert!(
        !h.state()
            .view_layers()
            .iter()
            .any(|l| l.color == mf_desktop::theme::HOVER)
    );
}

/// Hovering a loose piece tints its faces (the HOVER state bit) and the
/// tooltip gives its triangle count.
#[test]
fn hovering_a_loose_piece_tints_it_and_shows_its_triangle_count() {
    require_gpu!("hovering_a_loose_piece_tints_it_and_shows_its_triangle_count");
    let dir = tempfile::tempdir().unwrap();
    let cube = mf_fixtures::translated(&mf_fixtures::cube(4.0), [14.0, 3.0, 0.0]);
    let mut h = open(write(
        dir.path(),
        "plate_and_cube.obj",
        &mf_fixtures::merge(&[mf_fixtures::grid(10), cube]),
    ));
    wait_until(&mut h, "picking ready", MeshFixerApp::picking_ready);
    let piece_defect = h.state().current_analysis().unwrap().defects.loose[0].clone();
    let copy = copy_of(h.state(), &piece_defect);
    assert!(copy.detail.starts_with("12 triangles"), "{copy:?}");
    let piece = h.state().active_mesh().unwrap().topology().components[1].clone();

    let top = h.state().screen_point([16.0, 5.0, 4.0]).unwrap();
    point_at(&mut h, top);
    wait_until(&mut h, "hover tint", |a| {
        a.view_hover()
            .is_some_and(|v| v.target == mf_desktop::worker::OverlayTarget::Components(vec![1]))
            && piece.iter().all(|&f| state(a, f) & FaceStates::HOVER != 0)
    });
    h.get_by_label(&copy.detail);

    // Over the main body: not a defect, so nothing is hovered.
    let body = h.state().screen_point([5.0, 5.0, 0.0]).unwrap();
    point_at(&mut h, body);
    wait_until(&mut h, "hover cleared", |a| {
        a.view_hover().is_none() && piece.iter().all(|&f| state(a, f) & FaceStates::HOVER == 0)
    });
    assert!(h.query_by_label(&copy.detail).is_none());
}

/// How many of `n` points along the edge `a`–`b` (between 20% and 80% of
/// the way) show a colour `want` accepts within a pixel of the line, in
/// the frame rendered now (with the pointer wherever it is).
fn along_edge(
    h: &mut Harness<'_, MeshFixerApp>,
    a: [f64; 3],
    b: [f64; 3],
    n: usize,
    want: impl Fn([i32; 3]) -> bool,
) -> usize {
    let image = h.render().unwrap();
    (0..n)
        .filter(|&i| {
            let t = 0.2 + 0.6 * i as f64 / (n - 1) as f64;
            let p = [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * t);
            let s = h.state().screen_point(p).unwrap();
            (-1..=1).any(|dx| {
                (-1..=1).any(|dy| {
                    let px = image
                        .get_pixel((s.x as i32 + dx) as u32, (s.y as i32 + dy) as u32)
                        .0;
                    want([0, 1, 2].map(|k| i32::from(px[k])))
                })
            })
        })
        .count()
}

/// The hover colour (a light, slightly blue white), not the hole's orange
/// or the tangled edge's red.
fn hover_white(p: [i32; 3]) -> bool {
    p.iter().all(|&c| c >= 190) && p[2] >= p[0]
}

/// Fix round 1, I1: a selected hole's outline is drawn in the selection
/// colour over the hole's own outline, and a hovered tangled edge in the
/// hover colour over the tangled-edge line, along their whole length (no
/// z-fighting with the defect lines underneath).
#[test]
fn selection_and_hover_outlines_show_their_colours() {
    require_gpu!("selection_and_hover_outlines_show_their_colours");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(write(
        dir.path(),
        "plate.obj",
        &mf_fixtures::plate_with_defects(),
    ));
    wait_until(&mut h, "picking ready", MeshFixerApp::picking_ready);
    let mesh = h.state().active_mesh().unwrap().clone();
    let defects = h.state().current_analysis().unwrap().defects.clone();
    let Defect::Hole { loop_id, .. } = defects.holes[0] else {
        panic!("a hole: {:?}", defects.holes);
    };
    let topo = mesh.topology();
    let l = topo
        .boundary_loops
        .iter()
        .find(|l| l.id == loop_id)
        .unwrap();
    let (a, b) = (
        mesh.corner_position(l.vertices[0]),
        mesh.corner_position(l.vertices[1]),
    );

    // Select the hole by clicking its edge.
    let at = [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) / 3.0);
    let pos = h.state().screen_point(at).unwrap();
    click_at(&mut h, pos);
    h.event(egui::Event::PointerGone);
    wait_until(&mut h, "selection outline", |a| {
        a.selection.is_some()
            && a.view_layers()
                .iter()
                .any(|l| l.color == mf_desktop::theme::SELECTED)
    });
    let selected = along_edge(&mut h, a, b, 10, cyan);
    assert!(selected >= 9, "{selected} of 10 points cyan");

    // Hover the tangled edge.
    let Defect::Tangled { edge, .. } = &defects.tangled[0] else {
        panic!("a tangled edge");
    };
    let (a, b) = (mesh.corner_position(edge[0]), mesh.corner_position(edge[1]));
    let at = [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) / 3.0);
    let pos = h.state().screen_point(at).unwrap();
    point_at(&mut h, pos);
    wait_until(&mut h, "hover outline", |a| {
        a.view_layers()
            .iter()
            .any(|l| l.color == mf_desktop::theme::HOVER)
    });
    let hovered = along_edge(&mut h, a, b, 10, hover_white);
    assert!(hovered >= 9, "{hovered} of 10 points in the hover colour");
}

/// Fix round 1, P3: moving off a hovered hole and back to the very same
/// spot hovers it again.
#[test]
fn hover_returns_when_the_pointer_comes_back() {
    require_gpu!("hover_returns_when_the_pointer_comes_back");
    let dir = tempfile::tempdir().unwrap();
    let mut h = open(write(
        dir.path(),
        "open_box.obj",
        &mf_fixtures::scaled(&mf_fixtures::cube_with_missing_face(), 10.0),
    ));
    wait_until(&mut h, "picking ready", MeshFixerApp::picking_ready);
    let mesh = h.state().active_mesh().unwrap().clone();
    let l = mesh.topology().boundary_loops[0].clone();
    let (a, b) = (
        mesh.corner_position(l.vertices[0]),
        mesh.corner_position(l.vertices[1]),
    );
    let at = [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) / 3.0);
    let pos = h.state().screen_point(at).unwrap();
    let hovered = |a: &MeshFixerApp| a.view_hover().is_some();
    point_at(&mut h, pos);
    wait_until(&mut h, "hover", hovered);
    // Off the view entirely, then back.
    h.event(egui::Event::PointerGone);
    wait_until(&mut h, "hover cleared", |a| !hovered(a));
    point_at(&mut h, pos);
    wait_until(&mut h, "hover again", hovered);
}

/// Fix round 1, P5: a hovered loose piece stands out: it glows near-white,
/// clearly brighter than its amber at rest.
#[test]
fn hovered_loose_piece_stands_out() {
    require_gpu!("hovered_loose_piece_stands_out");
    let dir = tempfile::tempdir().unwrap();
    let cube = mf_fixtures::translated(&mf_fixtures::cube(4.0), [14.0, 3.0, 0.0]);
    let mut h = open(write(
        dir.path(),
        "plate_and_cube.obj",
        &mf_fixtures::merge(&[mf_fixtures::grid(10), cube]),
    ));
    wait_until(&mut h, "picking ready", MeshFixerApp::picking_ready);
    let piece = h.state().active_mesh().unwrap().topology().components[1].clone();
    // A side face of the cube, lit at an angle (so shading would dim it).
    let side = h.state().screen_point([18.0, 4.0, 2.0]).unwrap();
    let rest = pixel(&mut h, side);
    assert!(amber(rest), "{rest:?}");
    point_at(&mut h, side);
    wait_until(&mut h, "hover tint", |a| {
        a.view_hover().is_some() && piece.iter().all(|&f| state(a, f) & FaceStates::HOVER != 0)
    });
    h.run_steps(2);
    // Just above and left of the pointer (the harness draws the cursor
    // down and right of its tip).
    let p = h
        .render()
        .unwrap()
        .get_pixel(side.x as u32 - 4, side.y as u32 - 4)
        .0;
    let p = [0, 1, 2].map(|k| i32::from(p[k]));
    let sum = |p: [i32; 3]| p.iter().sum::<i32>();
    assert!(
        hover_white(p) && sum(p) >= sum(rest) + 120,
        "hovered {p:?} vs at rest {rest:?}"
    );
}
