#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Fast, end-to-end GUI tests for the core workflows (owner 2026-11-12):
//! each one drives the real UI (clicks, drags, keys) and checks the
//! resulting *document* — objects, counts, bounding boxes — not just
//! that no error appeared.
//!
//! Covers: Split (Keep both/above/below); Combine of solid+solid+hole
//! with Undo; Delete a split part by Backspace and by the trash button;
//! Connect (two dowels, Apply, holes and dowel objects); one Sculpt
//! inflate stroke with "stay on surface" leaving a split's cap
//! untouched; a Transform move and its Undo; and a parametric shape
//! param edit and its Undo; and Parts' "Combine with <target>" union
//! and its Undo; and Hollow (one undo step), then a vase by Split with
//! Cap off, Keep Bottom.
//!
//! Run with `scripts/core-workflows.sh` (kept under 3 minutes at `-j 4`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use glam::DVec3;
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::{Aabb, ContentHash, Mesh};
use mf_io::threemf::{ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};

const WINDOW: (f32, f32) = (1440.0, 1000.0);

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
            start.elapsed() < Duration::from_secs(90),
            "timed out waiting for: {what} (status {:?}, error {:?}, refusal {:?})",
            h.state().status,
            h.state().error,
            h.state().refusal,
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn settle(h: &mut Harness<'_, MeshFixerApp>) {
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(60), "never settled");
        h.step();
        if !h.state().background_work_in_flight() && !h.ctx.has_requested_repaint() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    h.run_steps(3);
}

fn at(t: [f64; 3]) -> [f64; 12] {
    [
        1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, t[0], t[1], t[2],
    ]
}

/// A fresh, isolated settings directory for one test's harness (a
/// `TempDir` leaked for the test binary's lifetime, same idea as
/// `connectors.rs`'s own per-harness tempdir): without this,
/// `Settings::default()`'s `config_dir: None` falls back to the real
/// user's `meshfixer_dir()` on disk (`settings_dir`'s documented
/// behaviour for "no settings folder"), so two test runs — or two
/// concurrent sessions on the same machine — would read and write the
/// same real Fit classes, Machine profiles and user parts library,
/// leaking state between runs (a stale saved part literally named
/// "Box" from an earlier run once made the Parts library list two
/// "Box" entries, failing `parametric_shape_edit_param_then_undo`'s
/// exact-match lookup).
fn isolated_settings_dir() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().expect("a temp dir for test settings"))
        .path()
}

fn opened(objects: Vec<(&str, Mesh, [f64; 12])>) -> Harness<'static, MeshFixerApp> {
    opened_with_units(objects, Unit::Millimeter)
}

/// As [`opened`], at `unit` instead of always millimetres (MF-UNIT-001):
/// every other test opens a document in mm, which is why the owner's
/// double mm-to-document-units conversion bug went uncaught — this is
/// the one seam a test can open a non-mm document through without
/// driving the unit prompt dialog itself.
fn opened_with_units(
    objects: Vec<(&str, Mesh, [f64; 12])>,
    unit: Unit,
) -> Harness<'static, MeshFixerApp> {
    let settings = Settings {
        config_dir: Some(isolated_settings_dir().to_owned()),
        ..Settings::default()
    };
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::with_settings(cc, None, settings).unwrap());
    h.state_mut().import_model(ImportedModel {
        metadata: Default::default(),
        objects: objects
            .into_iter()
            .map(|(name, mesh, transform)| ModelObject {
                metadata: Default::default(),
                name: name.into(),
                mesh,
                transform,
            })
            .collect(),
        unit: Some(unit),
        source: SourceInfo {
            path: "core-workflows.3mf".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
            exporter_hint: None,
        },
        warnings: Vec::new(),
    });
    wait_until(&mut h, "analysis and picking", |a| {
        a.current_analysis().is_some()
            && a.hole_view().is_some()
            && a.picking_ready()
            && a.camera.is_some()
    });
    h.event(egui::Event::PointerGone);
    settle(&mut h);
    h
}

/// A closed ball of `radius` mm.
fn ball(radius: f64) -> Mesh {
    mf_fixtures::scaled(&mf_fixtures::uv_sphere(32, 48), radius)
}

/// Two lobes of radius 20mm, centres 80mm apart in x: a horizontal
/// plane through both (z = their shared centre height) crosses each in
/// its own disjoint cap region, never nested (Phase 1b: cross-section
/// picking, bounded cut plane).
fn two_lobed() -> Mesh {
    let sphere = mf_fixtures::uv_sphere(32, 48);
    let lobe = mf_fixtures::scaled(&sphere, 20.0);
    mf_fixtures::merge(&[
        mf_fixtures::translated(&lobe, [-40.0, 0.0, 0.0]),
        mf_fixtures::translated(&lobe, [40.0, 0.0, 0.0]),
    ])
}

fn objects(h: &Harness<'_, MeshFixerApp>) -> Vec<String> {
    h.state()
        .session
        .document()
        .objects()
        .iter()
        .map(|o| o.name.clone())
        .collect()
}

fn id_of(h: &Harness<'_, MeshFixerApp>, name: &str) -> mf_command::ObjectId {
    h.state()
        .session
        .document()
        .objects()
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no object {name:?}"))
        .id
}

/// `id`'s mesh's world-space bounding box (its local box, offset by its
/// transform's translation: every object here is placed with no
/// rotation or scale, [`at`]).
fn world_bounds(h: &Harness<'_, MeshFixerApp>, id: mf_command::ObjectId) -> Aabb {
    let doc = h.state().session.document();
    let o = doc.object(id).unwrap();
    let bb = o.mesh().unwrap().bounding_box().unwrap();
    let t = DVec3::new(o.transform[9], o.transform[10], o.transform[11]);
    Aabb {
        min: (DVec3::from(bb.min) + t).to_array(),
        max: (DVec3::from(bb.max) + t).to_array(),
    }
}

fn history_len(h: &Harness<'_, MeshFixerApp>) -> usize {
    h.state().session.history_labels().len()
}

/// A press and release at `p` (window points), no drag.
fn click_at(h: &mut Harness<'_, MeshFixerApp>, p: egui::Pos2) {
    h.event(egui::Event::PointerMoved(p));
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    }
    h.run_steps(2);
}

/// A drag from `from` to `to` (window points), in steps.
fn drag(h: &mut Harness<'_, MeshFixerApp>, from: egui::Pos2, to: egui::Pos2, steps: u32) {
    h.event(egui::Event::PointerMoved(from));
    h.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    for i in 1..=steps {
        let p = from + (to - from) * (i as f32 / steps as f32);
        h.event(egui::Event::PointerMoved(p));
        h.step();
    }
    h.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.run_steps(2);
}

// ---------------------------------------------------------------------
// Split: Keep both, above and below.
// ---------------------------------------------------------------------

/// Clicks the Keep segmented control's button labelled `label`, and
/// Cut, then returns the document's object names.
fn cut_with_keep(h: &mut Harness<'_, MeshFixerApp>, label: &str) -> Vec<String> {
    h.get_by_label(label).click();
    settle(h);
    let history = history_len(h);
    // Scrolled, not a plain click (owner: the Split panel can run
    // taller than the window once the region checklist or the bounded
    // cut area's controls add rows — Phase 1b — same as Connect's
    // panel already needed [`scroll_and_click`] for).
    scroll_and_click(h, "Cut");
    wait_until(h, "the cut", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    objects(h)
}

/// Keep both: the target becomes "(top)" and a new "(bottom)" appears;
/// both parts' bounding boxes sit on their own side of the plane.
#[test]
fn split_keep_both_leaves_two_parts_on_either_side() {
    require_gpu!("split_keep_both_leaves_two_parts_on_either_side");
    let mut h = opened(vec![("ball", ball(30.0), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    // Off the ball's seam of vertices, through its centre.
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    // "Both" is the default; clicked explicitly so the test doesn't
    // depend on that default never changing.
    let names = cut_with_keep(&mut h, "Both");
    assert_eq!(names, ["ball (top)", "ball (bottom)"], "{names:?}");
    let top = world_bounds(&h, id_of(&h, "ball (top)"));
    let bottom = world_bounds(&h, id_of(&h, "ball (bottom)"));
    assert!(top.min[2] >= 31.3 - 1e-6, "{top:?}");
    assert!(bottom.max[2] <= 31.3 + 1e-6, "{bottom:?}");
}

/// Keep above (now labelled by orientation: "Top" for a horizontal cut):
/// only the part above the plane remains, as the renamed target; the
/// part below is gone.
#[test]
fn split_keep_above_leaves_only_the_top_part() {
    require_gpu!("split_keep_above_leaves_only_the_top_part");
    let mut h = opened(vec![("ball", ball(30.0), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    let names = cut_with_keep(&mut h, "Top");
    assert_eq!(names, ["ball (top)"], "{names:?}: the bottom must be gone");
    let kept = world_bounds(&h, id_of(&h, "ball (top)"));
    assert!(kept.min[2] >= 31.3 - 1e-6, "{kept:?}");
    assert!(
        kept.max[2] > 50.0,
        "most of the ball should remain: {kept:?}"
    );
}

/// Keep below ("Bottom" for a horizontal cut): only the part below the
/// plane remains.
#[test]
fn split_keep_below_leaves_only_the_bottom_part() {
    require_gpu!("split_keep_below_leaves_only_the_bottom_part");
    let mut h = opened(vec![("ball", ball(30.0), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    let names = cut_with_keep(&mut h, "Bottom");
    assert_eq!(names, ["ball (bottom)"], "{names:?}: the top must be gone");
    let kept = world_bounds(&h, id_of(&h, "ball (bottom)"));
    assert!(kept.max[2] <= 31.3 + 1e-6, "{kept:?}");
    assert!(
        kept.min[2] < 10.0,
        "most of the ball should remain: {kept:?}"
    );
}

/// The multi-target path: Keep above cuts every selected object, and
/// only the parts above the plane remain for both.
#[test]
fn split_keep_above_on_two_targets_leaves_only_their_top_parts() {
    require_gpu!("split_keep_above_on_two_targets_leaves_only_their_top_parts");
    let mut h = opened(vec![
        ("ball1", ball(30.0), at([-80.0, 0.0, 30.0])),
        ("ball2", ball(30.0), at([80.0, 0.0, 30.0])),
    ]);
    let ids: Vec<_> = h
        .state()
        .session
        .document()
        .objects()
        .iter()
        .map(|o| o.id)
        .collect();
    {
        let a = h.state_mut();
        a.picked = ids.to_vec();
        a.active = ids.first().copied();
    }
    settle(&mut h);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    let names = cut_with_keep(&mut h, "Top");
    assert_eq!(names, ["ball1 (top)", "ball2 (top)"], "{names:?}");
    for name in ["ball1 (top)", "ball2 (top)"] {
        let kept = world_bounds(&h, id_of(&h, name));
        assert!(kept.min[2] >= 31.3 - 1e-6, "{name}: {kept:?}");
    }
}

// ---------------------------------------------------------------------
// Split: orientation-named sides (owner 2026-11-12): "above"/"below"
// means nothing for a vertical or tilted cut, so the Keep buttons, the
// preview and the kept part's name are all named by the plane's actual
// orientation instead. Horizontal: Top/Bottom (already covered above).
// ---------------------------------------------------------------------

/// Vertical X (normal +X, the front view's right): Keep buttons read
/// Left/Right, and the kept part is named to match.
#[test]
fn split_vertical_x_names_sides_left_and_right() {
    require_gpu!("split_vertical_x_names_sides_left_and_right");
    let mut h = opened(vec![("ball", ball(30.0), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.get_by_label("Vertical X").click();
    settle(&mut h);
    let n = h.state().split.normal();
    assert!((n.x - 1.0).abs() < 1e-9, "{n:?}");
    h.get_by_label("Right");
    h.get_by_label("Left");
    assert!(h.query_by_label("Top").is_none());
    assert!(h.query_by_label("Bottom").is_none());
    h.state_mut().split.plane[9] += 1.3; // off the seam of vertices
    settle(&mut h);
    let names = cut_with_keep(&mut h, "Right");
    assert_eq!(names, ["ball (right)"], "{names:?}");
    let kept = world_bounds(&h, id_of(&h, "ball (right)"));
    assert!(kept.min[0] >= 1.3 - 1e-6, "{kept:?}");
}

/// Vertical Y (normal +Y): Keep buttons read Front/Back.
#[test]
fn split_vertical_y_names_sides_front_and_back() {
    require_gpu!("split_vertical_y_names_sides_front_and_back");
    let mut h = opened(vec![("ball", ball(30.0), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.get_by_label("Vertical Y").click();
    settle(&mut h);
    let n = h.state().split.normal();
    assert!((n.y - 1.0).abs() < 1e-9, "{n:?}");
    h.get_by_label("Back");
    h.get_by_label("Front");
    h.state_mut().split.plane[10] += 1.3;
    settle(&mut h);
    let names = cut_with_keep(&mut h, "Back");
    assert_eq!(names, ["ball (back)"], "{names:?}");
    let kept = world_bounds(&h, id_of(&h, "ball (back)"));
    assert!(kept.min[1] >= 1.3 - 1e-6, "{kept:?}");
}

/// A tilted plane (45 degrees, neither horizontal nor vertical): the
/// Keep buttons fall back to the plain "Side A"/"Side B", matching the
/// kept part's name.
#[test]
fn split_tilted_plane_names_sides_a_and_b() {
    require_gpu!("split_tilted_plane_names_sides_a_and_b");
    let mut h = opened(vec![("ball", ball(30.0), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    // Tilt 45 degrees about Y: the normal leans equally on X and Z.
    // Offset off the ball's exact centre (a plane dead through the
    // poles grazes the surface there): slide it a little along the
    // normal.
    let point = h.state().split.point();
    h.state_mut().split.plane =
        mf_render::compose(point, DVec3::new(0.0, 45.0, 0.0), DVec3::ONE, DVec3::ZERO);
    let n = h.state().split.normal();
    let nudged = point + n * 1.7;
    h.state_mut().split.plane =
        mf_render::compose(nudged, DVec3::new(0.0, 45.0, 0.0), DVec3::ONE, DVec3::ZERO);
    settle(&mut h);
    let n = h.state().split.normal();
    assert!(
        n.z.abs() > 0.1 && n.x.abs() > 0.1,
        "a genuine tilt, not axis-aligned: {n:?}"
    );
    h.get_by_label("Side A");
    h.get_by_label("Side B");
    assert!(h.query_by_label("Top").is_none());
    assert!(h.query_by_label("Left").is_none());
    let names = cut_with_keep(&mut h, "Side A");
    assert_eq!(names, ["ball (side a)"], "{names:?}");
}

/// Clicking a side's surface in the view (not the gizmo) sets Keep to
/// that side, for both the horizontal and the vertical-X orientation.
#[test]
fn clicking_a_side_in_the_view_sets_keep() {
    require_gpu!("clicking_a_side_in_the_view_sets_keep");
    let mut h = opened(vec![("ball", ball(30.0), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    assert_eq!(h.state().split.keep, "both");
    // Horizontal (the default plane, through the centre): click near
    // the top and the bottom.
    let top = h.state().screen_point([0.0, 0.0, 58.0]).unwrap();
    click_at(&mut h, top);
    settle(&mut h);
    assert_eq!(h.state().split.keep, "above");
    let bottom = h.state().screen_point([0.0, 0.0, 2.0]).unwrap();
    click_at(&mut h, bottom);
    settle(&mut h);
    assert_eq!(h.state().split.keep, "below");

    // Vertical X: click near +X and -X.
    h.get_by_label("Vertical X").click();
    settle(&mut h);
    let right = h.state().screen_point([28.0, 0.0, 30.0]).unwrap();
    click_at(&mut h, right);
    settle(&mut h);
    assert_eq!(h.state().split.keep, "above", "the +X side is side a");
    let left = h.state().screen_point([-28.0, 0.0, 30.0]).unwrap();
    click_at(&mut h, left);
    settle(&mut h);
    assert_eq!(h.state().split.keep, "below", "the -X side is side b");
}

/// "Cap the cut" (owner 2026-11-12), on by default: the split's parts
/// are closed, each with a cap, and the cut is recorded as a joint
/// Connect can add connectors to.
#[test]
fn cap_on_by_default_closes_the_parts_and_records_a_joint() {
    require_gpu!("cap_on_by_default_closes_the_parts_and_records_a_joint");
    let mut h = opened(vec![("ball", ball(30.0), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    assert!(h.state().split.cap, "on by default");
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    let joints_before = h.state().split_flow.joints.len();
    let names = cut_with_keep(&mut h, "Both");
    assert_eq!(names, ["ball (top)", "ball (bottom)"], "{names:?}");
    assert_eq!(
        h.state().split_flow.joints.len(),
        joints_before + 1,
        "the cut is recorded as a joint"
    );
    for name in ["ball (top)", "ball (bottom)"] {
        let id = id_of(&h, name);
        let mesh = h
            .state()
            .session
            .document()
            .object(id)
            .unwrap()
            .mesh()
            .unwrap()
            .clone();
        let topo = mesh.topology_with(&mf_runtime::RunContext::none()).unwrap();
        assert_eq!(topo.boundary_edges, 0, "{name}: closed, with a cap");
    }
}

/// "Cap the cut", off: for a vase or bowl. Each kept part is left open
/// at the cut (no cap faces, a real boundary there) and no joint is
/// recorded for it.
#[test]
fn cap_off_leaves_the_parts_open_with_no_joint() {
    require_gpu!("cap_off_leaves_the_parts_open_with_no_joint");
    let mut h = opened(vec![("ball", ball(30.0), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    h.state_mut().split.cap = false;
    settle(&mut h);
    let joints_before = h.state().split_flow.joints.len();
    let names = cut_with_keep(&mut h, "Both");
    assert_eq!(names, ["ball (top)", "ball (bottom)"], "{names:?}");
    assert_eq!(
        h.state().split_flow.joints.len(),
        joints_before,
        "no joint recorded when the cut isn't capped"
    );
    for name in ["ball (top)", "ball (bottom)"] {
        let id = id_of(&h, name);
        let mesh = h
            .state()
            .session
            .document()
            .object(id)
            .unwrap()
            .mesh()
            .unwrap()
            .clone();
        let topo = mesh.topology_with(&mf_runtime::RunContext::none()).unwrap();
        assert!(topo.boundary_edges > 0, "{name}: left open at the cut");
    }
}

// ---------------------------------------------------------------------
// Hollow, then a vase: Split with Cap off, Keep Bottom.
// ---------------------------------------------------------------------

/// Saves the current frame as `<MF_ARTIFACTS>/hollow/<name>.png` (the
/// Hollow tool's checkpoint captures), a no-op without `MF_ARTIFACTS`.
fn still(h: &mut Harness<'_, MeshFixerApp>, name: &str) {
    let Some(dir) = std::env::var_os("MF_ARTIFACTS") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir).join("hollow");
    std::fs::create_dir_all(&dir).unwrap();
    h.render()
        .unwrap()
        .save(dir.join(format!("{name}.png")))
        .unwrap();
}

fn mesh_of(h: &Harness<'_, MeshFixerApp>, name: &str) -> Mesh {
    h.state()
        .session
        .document()
        .object(id_of(h, name))
        .unwrap()
        .mesh()
        .unwrap()
        .clone()
}

/// Hollow (MF-ISSUE-112): the preview shows the ball hollowed and cut
/// open (the section view); "Hollow" makes it a closed shell of two
/// surfaces in one undo step (Undo puts the solid back, Redo the shell);
/// then Split with Cap off, Keep Bottom leaves an open-top vase: the
/// outside and the inside wall, each open at the rim, with a floor a wall
/// thick.
#[test]
fn hollow_then_split_cap_off_keep_bottom_makes_an_open_vase() {
    require_gpu!("hollow_then_split_cap_off_keep_bottom_makes_an_open_vase");
    let mut h = opened(vec![("ball", ball(30.0), at([0.0, 0.0, 30.0]))]);
    let solid = mesh_of(&h, "ball").content_hash();
    h.get_by_label("Hollow").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "hollow");
    let id = id_of(&h, "ball");
    wait_until(&mut h, "the hollow preview", |a| {
        mf_desktop::hollow_tool::preview_render(a, id).is_some()
    });
    settle(&mut h);
    assert!(
        mf_desktop::shape_tools::split_preview(h.state()).is_some(),
        "the section view cuts the preview open"
    );
    still(&mut h, "1-preview-section");
    let history = history_len(&h);
    panel_button(&h, "Hollow").click();
    wait_until(&mut h, "the hollow", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(history_len(&h), history + 1, "one undo step");
    let shell = mesh_of(&h, "ball");
    let topo = shell
        .topology_with(&mf_runtime::RunContext::none())
        .unwrap();
    assert_eq!(topo.boundary_edges, 0, "closed");
    assert_eq!(topo.components.len(), 2, "the outside and the inside wall");
    let hollowed = shell.content_hash();

    h.get_by_label("Undo").click();
    wait_until(&mut h, "the undo", |a| {
        a.busy.is_none()
            && a.session
                .document()
                .object(id)
                .and_then(|o| o.mesh())
                .is_some_and(|m| m.content_hash() == solid)
    });
    h.get_by_label("Redo").click();
    wait_until(&mut h, "the redo", |a| {
        a.busy.is_none()
            && a.session
                .document()
                .object(id)
                .and_then(|o| o.mesh())
                .is_some_and(|m| m.content_hash() == hollowed)
    });
    settle(&mut h);

    // The vase: cut 15 above the centre, no cap, keep the bottom.
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] = 45.0;
    h.state_mut().split.cap = false;
    settle(&mut h);
    let names = cut_with_keep(&mut h, "Bottom");
    assert_eq!(names, ["ball (bottom)"], "{names:?}");
    still(&mut h, "2-vase");
    let vase = mesh_of(&h, "ball (bottom)");
    let topo = vase.topology_with(&mf_runtime::RunContext::none()).unwrap();
    assert_eq!(topo.components.len(), 2, "outside and inside wall");
    assert_eq!(
        topo.boundary_loops.len(),
        2,
        "open at the rim, outside and in"
    );
    let bb = world_bounds(&h, id_of(&h, "ball (bottom)"));
    assert!((bb.max[2] - 45.0).abs() < 1e-6, "{bb:?}");
    assert!(bb.min[2] < 1.0, "{bb:?}");
    // The inside wall's floor is a wall (2 mm) above the outside's.
    let floors: Vec<f64> = topo
        .components
        .iter()
        .map(|c| {
            c.iter()
                .flat_map(|&f| vase.face(mf_geometry::FaceId(f)).unwrap())
                .map(|v| vase.corner_position(v)[2])
                .fold(f64::INFINITY, f64::min)
        })
        .collect();
    let (outer_floor, inner) = (floors[0].min(floors[1]), floors[0].max(floors[1]));
    assert!(
        (inner - outer_floor - 2.0).abs() < 0.6,
        "floor {inner} over {outer_floor}"
    );
}

/// A mushroom: a stem 30 across and 20 tall under a cap 60 across and 3
/// thick (a revolved profile, closed). With a 2 mm wall the cap stays
/// solid.
fn mushroom() -> Mesh {
    let profile = [(15.0, 0.0), (15.0, 20.0), (30.0, 20.0), (30.0, 23.0)];
    let segments = 64u32;
    let mut positions = vec![[0.0, 0.0, 0.0], [0.0, 0.0, 23.0]];
    for k in 0..segments {
        let a = std::f64::consts::TAU * f64::from(k) / f64::from(segments);
        for &(r, z) in &profile {
            positions.push([r * a.cos(), r * a.sin(), z]);
        }
    }
    let p = profile.len() as u32;
    let at = |k: u32, i: u32| 2 + (k % segments) * p + i;
    let mut tris = Vec::new();
    for k in 0..segments {
        tris.push([0, at(k + 1, 0), at(k, 0)]);
        tris.push([1, at(k, p - 1), at(k + 1, p - 1)]);
        for i in 0..p - 1 {
            let (a, b, c, d) = (at(k, i), at(k + 1, i), at(k + 1, i + 1), at(k, i + 1));
            tris.push([a, b, c]);
            tris.push([a, c, d]);
        }
    }
    Mesh::new(positions, tris).unwrap()
}

/// The preview outlines the parts left solid and says so in plain words;
/// a placed drain hole is clicked on the surface, and Hollow cuts it, so
/// the inside opens to the outside (one connected surface).
#[test]
fn hollow_shows_thin_parts_and_cuts_a_placed_drain_hole() {
    require_gpu!("hollow_shows_thin_parts_and_cuts_a_placed_drain_hole");
    let mut h = opened(vec![
        ("mushroom", mushroom(), at([0.0, 0.0, 0.0])),
        ("ball", ball(30.0), at([120.0, 0.0, 30.0])),
    ]);
    row(&h, "mushroom").click();
    settle(&mut h);
    h.get_by_label("Hollow").click();
    settle(&mut h);
    let mushroom_id = id_of(&h, "mushroom");
    wait_until(&mut h, "the mushroom's preview", |a| {
        mf_desktop::hollow_tool::preview_render(a, mushroom_id).is_some()
    });
    settle(&mut h);
    // Drain holes default to None; the hint points at the custom-opening
    // alternative (Parts + Combine) instead.
    assert_eq!(h.state().hollow.drain, "none", "default is None");
    h.get_by_label(
        "Or place a cylinder from Parts, mark it as a Hole, and Combine for a custom opening.",
    );
    assert!(
        mf_desktop::hollow_tool::thin_layer(h.state()).is_some(),
        "the cap is outlined as left solid"
    );
    h.get_by_label("Parts thinner than 4.00 mm were left solid");
    still(&mut h, "3-thin-parts");

    // A placed hole on the ball.
    row(&h, "ball").click();
    settle(&mut h);
    let ball_id = id_of(&h, "ball");
    h.get_by_label("Placed").click();
    settle(&mut h);
    wait_until(&mut h, "the ball's preview", |a| {
        mf_desktop::hollow_tool::preview_render(a, ball_id).is_some()
    });
    settle(&mut h);
    let p = facing_point(&h, [120.0, 0.0, 30.0], 30.0);
    click_at(&mut h, p);
    settle(&mut h);
    assert_eq!(h.state().hollow.points.len(), 1, "one hole placed");
    h.get_by_label("Holes placed: 1");
    let history = history_len(&h);
    panel_button(&h, "Hollow").click();
    wait_until(&mut h, "the hollow", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let shell = mesh_of(&h, "ball");
    let topo = shell
        .topology_with(&mf_runtime::RunContext::none())
        .unwrap();
    assert_eq!(topo.boundary_edges, 0, "closed");
    assert_eq!(
        topo.components.len(),
        1,
        "the hole joins the inside to the outside"
    );
    // The status line is plain words, not the operation's raw report
    // JSON (owner 2026-11-14 follow-up).
    let status = h.state().status.clone().unwrap_or_default();
    assert!(
        !status.contains('{'),
        "raw report leaked into the status: {status}"
    );
    assert!(
        status.contains("ball") && status.contains("mm walls"),
        "{status}"
    );
}

/// `open_through` (owner 2026-11-13): Split cuts a ball into a dome
/// with a flat cap (Keep: Top, Cap on); opening Hollow and clicking the
/// cap picks the whole region (its checkbox lights up); Hollow with
/// "Open through this surface" on gives a single open-topped shell —
/// closed, manifold, with the rim present (one undo).
#[test]
fn hollow_open_through_the_picked_cap_gives_an_open_topped_shell() {
    require_gpu!("hollow_open_through_the_picked_cap_gives_an_open_topped_shell");
    let mut h = opened(vec![("ball", ball(20.0), at([0.0, 0.0, 20.0]))]);
    let id = id_of(&h, "ball");

    // Split the ball near its own centre (world z = 21.3, off the seam
    // of vertex rings), Keep: Bottom, Cap on: a hemisphere, its flat
    // cap near the equator facing up (visible from the default
    // camera) — the common case `open_through` is for.
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] = 21.3;
    settle(&mut h);
    let names = cut_with_keep(&mut h, "Bottom");
    assert_eq!(names, ["ball (bottom)"], "{names:?}");
    let dome = mesh_of(&h, "ball (bottom)");
    let dome_topo = dome.topology_with(&mf_runtime::RunContext::none()).unwrap();
    assert_eq!(dome_topo.boundary_edges, 0, "capped, so still closed");

    // Open Hollow and click the cap (from above, where it faces): the
    // whole region is picked.
    h.get_by_label("Hollow").click();
    settle(&mut h);
    wait_until(&mut h, "the dome's preview", |a| {
        mf_desktop::hollow_tool::preview_render(a, id).is_some()
    });
    settle(&mut h);
    let p = facing_point(&h, [0.0, 0.0, 21.3], 1.0);
    click_at(&mut h, p);
    settle(&mut h);
    assert!(
        h.state().hollow.picked_region.is_some(),
        "the click should pick a region"
    );
    h.get_by_label("Open through this surface").click();
    settle(&mut h);
    assert!(h.state().hollow.open_through, "the checkbox should be on");

    let history = history_len(&h);
    panel_button(&h, "Hollow").click();
    wait_until(&mut h, "the hollow", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(history_len(&h), history + 1, "one undo step");
    let shell = mesh_of(&h, "ball (bottom)");
    let topo = shell
        .topology_with(&mf_runtime::RunContext::none())
        .unwrap();
    assert_eq!(topo.boundary_edges, 0, "closed, with a rim, not an opening");
    assert!(
        topo.non_manifold_edges.is_empty() && topo.non_manifold_vertices == 0,
        "{topo:?}"
    );
    assert_eq!(
        topo.components.len(),
        1,
        "the rim joins the outer and inner walls into one surface"
    );

    // The owner's own report (2026-11-14 follow-up): the rim stays at
    // the cut height, with the full wall thickness, not with the
    // whole cap region's radius cut away along with it.
    let wall = 2.0;
    let cell = 0.5;
    let cut_z = 1.3; // the cap's own local height (world 21.3, the ball at local z=0, placed at world z=20).
    let rim: Vec<[f64; 3]> = shell
        .vertices()
        .map(|(_, p)| p)
        .filter(|p| (p[2] - cut_z).abs() < cell * 2.0)
        .collect();
    assert!(!rim.is_empty(), "there should be a rim near the cut plane");
    let radii: Vec<f64> = rim
        .iter()
        .map(|p| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt())
        .collect();
    let outer_radius = radii.iter().cloned().fold(0.0_f64, f64::max);
    let inner_radius = radii.iter().cloned().fold(f64::INFINITY, f64::min);
    assert!(
        (outer_radius - 20.0).abs() <= cell,
        "outer rim radius {outer_radius}, expected near 20"
    );
    let measured_wall = outer_radius - inner_radius;
    assert!(
        (measured_wall - wall).abs() <= cell,
        "rim wall thickness {measured_wall}, expected near {wall}"
    );

    // The status line is plain words naming the opening, not raw JSON
    // (owner 2026-11-14 follow-up).
    let status = h.state().status.clone().unwrap_or_default();
    assert!(
        !status.contains('{'),
        "raw report leaked into the status: {status}"
    );
    assert!(
        status.contains("ball (bottom)") && status.contains("open at the cut face"),
        "{status}"
    );
}

/// Highlighting the picked surface (owner 2026-11-13): clicking a split
/// cap's flat face while Hollow is open selects the whole cap, up to
/// its own boundary (a box's cut face is exactly flat — every face
/// there is coplanar, and the sides it's capped onto meet it at a
/// sharp, real crease, so the crease-limited region grows all the way
/// to the rim and no further); the selection tint lights up every one
/// of those faces and no other; the panel names it by the object and
/// the side Split's own naming gives that cut.
#[test]
fn hollow_clicking_the_cap_highlights_the_whole_region_and_names_it() {
    require_gpu!("hollow_clicking_the_cap_highlights_the_whole_region_and_names_it");
    let mut h = opened(vec![(
        "block",
        mf_fixtures::cube(40.0),
        at([0.0, 0.0, 0.0]),
    )]);
    let id = id_of(&h, "block");

    // Cut the block in half (world z = 20, its own local z too): Keep
    // Bottom leaves a 40x40x20 box whose new top face (the cap, local
    // z = 20) is flat, bounded by the box's own four flat sides, and
    // faces up (visible from the default camera).
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] = 20.0;
    settle(&mut h);
    let names = cut_with_keep(&mut h, "Bottom");
    assert_eq!(names, ["block (bottom)"], "{names:?}");
    let kept = mesh_of(&h, "block (bottom)");

    // The cap's own faces: every face every one of whose corners sits
    // on the cut plane (local z = 20, the box's own new top).
    let cap_faces: Vec<u32> = kept
        .faces()
        .filter(|(_, t)| {
            t.iter()
                .all(|&v| (kept.corner_position(v)[2] - 20.0).abs() < 1e-6)
        })
        .map(|(f, _)| f.0)
        .collect();
    assert!(!cap_faces.is_empty());

    h.get_by_label("Hollow").click();
    settle(&mut h);
    wait_until(&mut h, "the box's preview", |a| {
        mf_desktop::hollow_tool::preview_render(a, id).is_some()
    });
    settle(&mut h);
    // The new top face (world z = 20, the cut plane), from above.
    let p = facing_point(&h, [20.0, 20.0, 20.0], 1.0);
    click_at(&mut h, p);
    settle(&mut h);

    // The whole cap is selected, nothing else.
    let region = h.state().hollow.picked_region.clone().unwrap();
    let mut picked: Vec<u32> = region.faces.iter().map(|f| f.0).collect();
    picked.sort_unstable();
    let mut expected = cap_faces.clone();
    expected.sort_unstable();
    assert_eq!(picked, expected, "the whole cap, up to its own boundary");

    // The selection tint and outline (the same mechanism and meaning as
    // any other face selection here): `app.selection` names the same
    // faces, on the box's own revision.
    let sel = h.state().selection.clone().unwrap();
    assert_eq!(sel.kind, mf_selection::SelectionKind::Faces);
    assert_eq!(sel.object, id);
    let mut sel_ids = sel.ids.clone();
    sel_ids.sort_unstable();
    assert_eq!(sel_ids, expected);
    wait_until(&mut h, "the cap's selection tint", |a| {
        cap_faces
            .iter()
            .all(|&f| a.read_face_state(f).unwrap().unwrap() & mf_render::FaceStates::SELECTED != 0)
    });

    // Named by the object and the side it faces (Split's own naming).
    h.get_by_label(&format!("Opens through: {}", region.name));
    assert!(
        region.name.starts_with("Cut face of block (bottom) ("),
        "{}",
        region.name
    );
}

/// The owner's case (2026-11-13 follow-up): a sphere cut near its
/// pole, Keep Both (so the cut records a joint — its own plane and
/// which piece is which side). Picking the large piece's cap must
/// match the joint's own recorded cap exactly, not a crease-grown
/// region (near the pole the surrounding surface curves at well under
/// 60 degrees from the cap, which crease growth alone would wrongly
/// cross); Hollow with "Open through this surface" on must then
/// succeed on it.
#[test]
fn hollow_open_through_the_cap_of_a_near_pole_split_uses_the_recorded_cap_exactly() {
    require_gpu!("hollow_open_through_the_cap_of_a_near_pole_split_uses_the_recorded_cap_exactly");
    let mut h = opened(vec![("ball", ball(20.0), at([0.0, 0.0, 20.0]))]);

    // Cut near the pole (world z = 38, local z = 18, 2 mm short of the
    // top): Keep Both records a joint between the two pieces.
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] = 38.0;
    settle(&mut h);
    let names = cut_with_keep(&mut h, "Both");
    assert_eq!(names, ["ball (top)", "ball (bottom)"], "{names:?}");
    assert_eq!(h.state().split_flow.joints.len(), 1, "a joint was recorded");

    // The large piece is "ball (bottom)" (below the cut, most of the
    // sphere); its own cap is the joint's side B (the cut's own report
    // pairs the created "(top)" piece as operand a, the target "(bottom)"
    // as b — checked, not assumed, below).
    let big_id = id_of(&h, "ball (bottom)");
    let joint = h.state().split_flow.joints[0].clone();
    let big_side = if joint.a == big_id {
        mf_operations::JointSide::A
    } else if joint.b == big_id {
        mf_operations::JointSide::B
    } else {
        panic!("the joint should name ball (bottom) as one of its pieces: {joint:?}");
    };
    let big_mesh = mesh_of(&h, "ball (bottom)");
    let big_transform = h
        .state()
        .session
        .document()
        .object(big_id)
        .unwrap()
        .transform;
    let bvh = mf_geometry::Bvh::build(&big_mesh, &mf_runtime::RunContext::none()).unwrap();
    let recorded: Vec<u32> = mf_operations::cap_faces(
        &big_mesh,
        Some(&bvh),
        &big_transform,
        &joint.frame,
        big_side,
        &mf_runtime::RunContext::none(),
    )
    .unwrap()
    .into_iter()
    .map(|(f, _)| f.0)
    .collect();
    assert!(!recorded.is_empty());

    // Pick the cap in the view: it must match the joint's own recorded
    // cap exactly.
    row(&h, "ball (bottom)").click();
    settle(&mut h);
    h.get_by_label("Hollow").click();
    settle(&mut h);
    wait_until(&mut h, "the big piece's preview", |a| {
        mf_desktop::hollow_tool::preview_render(a, big_id).is_some()
    });
    settle(&mut h);
    let p = facing_point(&h, [0.0, 0.0, 38.0], 1.0);
    click_at(&mut h, p);
    settle(&mut h);
    let region = h.state().hollow.picked_region.clone().unwrap();
    let mut picked: Vec<u32> = region.faces.iter().map(|f| f.0).collect();
    picked.sort_unstable();
    let mut expected = recorded.clone();
    expected.sort_unstable();
    assert_eq!(
        picked, expected,
        "the pick must be exactly the recorded cap, not crease growth"
    );

    h.get_by_label("Open through this surface").click();
    settle(&mut h);
    assert!(h.state().hollow.open_through);
    let history = history_len(&h);
    panel_button(&h, "Hollow").click();
    wait_until(&mut h, "the hollow", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(history_len(&h), history + 1, "one undo step");
    let shell = mesh_of(&h, "ball (bottom)");
    let topo = shell
        .topology_with(&mf_runtime::RunContext::none())
        .unwrap();
    assert_eq!(topo.boundary_edges, 0, "closed, with a rim, not an opening");
    assert!(
        topo.non_manifold_edges.is_empty() && topo.non_manifold_vertices == 0,
        "{topo:?}"
    );
    assert_eq!(
        topo.components.len(),
        1,
        "the rim joins the outer and inner walls into one surface"
    );
}

/// Owner 2026-11-12: with only "ball" selected (a plain Browser click,
/// not Shift), Split must cut only "ball", even though the plane also
/// crosses "cube", which was never selected. The panel says so by name
/// ("Cuts: ball" / "Not cut (not selected): cube"), and Apply leaves
/// "cube" bit-for-bit untouched with no refusal — the plane crossing an
/// object that isn't selected is not an error.
#[test]
fn split_targets_only_the_selected_object_and_names_the_rest() {
    require_gpu!("split_targets_only_the_selected_object_and_names_the_rest");
    let mut h = opened(vec![
        ("cube", mf_fixtures::cube(40.0), at([-20.0, -20.0, 0.0])),
        ("ball", ball(10.0), at([0.0, 0.0, 20.0])),
    ]);
    // A plain click: selects just "ball" (no Shift, no stale pick).
    row(&h, "ball").click();
    settle(&mut h);
    assert_eq!(h.state().active, Some(id_of(&h, "ball")));
    assert!(h.state().picked.len() <= 1, "{:?}", h.state().picked);
    let cube_before = h
        .state()
        .session
        .document()
        .object(id_of(&h, "cube"))
        .unwrap()
        .mesh()
        .unwrap()
        .clone();
    h.get_by_label("Split").click();
    settle(&mut h);
    assert_eq!(
        h.state().selected_objects(),
        vec![id_of(&h, "ball")],
        "Split must target only the selected object"
    );
    // The plane (through the ball's centre, z=20) also crosses the cube
    // (which spans z=0..40): a plane that only ever touched "ball" would
    // prove nothing about this bug.
    assert!(h.state().split.point().z > 0.0 && h.state().split.point().z < 40.0);
    h.get_by_label("Cuts: ball");
    h.get_by_label("Not cut (not selected): cube");
    let history = history_len(&h);
    h.get_by_label("Cut").click();
    wait_until(&mut h, "the cut", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let names = objects(&h);
    assert!(names.contains(&"ball (top)".to_string()), "{names:?}");
    assert!(names.contains(&"ball (bottom)".to_string()), "{names:?}");
    assert!(
        names.contains(&"cube".to_string()),
        "{names:?}: unchanged, so still named plainly \"cube\""
    );
    let cube_after = h
        .state()
        .session
        .document()
        .object(id_of(&h, "cube"))
        .unwrap()
        .mesh()
        .unwrap()
        .clone();
    assert!(
        Mesh::ptr_eq(&cube_before, &cube_after),
        "the cube must be bit-for-bit untouched"
    );
}

/// The demo scene's "scan" at Split's default plane (owner 2026-11-12:
/// `split_section_self_intersects`, "the cut grazes the surface here"):
/// the plane is placed horizontal through the selection's own bounding
/// box centre ([`crate::shape_tools::place_plane`]'s doc comment), which
/// for this damaged UV sphere lands exactly on its equator — a whole
/// *ring* of vertices on the plane, not just one. Cut must still
/// succeed, with both new parts present and no refusal.
#[test]
fn split_demo_scan_at_the_default_plane_is_never_refused() {
    require_gpu!("split_demo_scan_at_the_default_plane_is_never_refused");
    // The demo scene's own "scan" ([`mf_fixtures::demo_scene`]) has
    // deliberate holes (scan damage), which leaves it open — Split's Cut
    // button is disabled for an open target, so it can't reach this bug
    // at all. This is the same shape (a damaged-sphere-sized scan, with
    // its scan-bump cubes, at the same placement, rotated 25 degrees
    // about Z so the equator ring is off-axis in world space, exactly
    // as the real demo file is), but undamaged, so Split can actually
    // run: the plane Split places by default (the selection's own world
    // bounding-box centre, horizontal) still lands exactly on its
    // equator, a whole ring of vertices on the plane.
    let sphere = mf_fixtures::scaled(
        &mf_fixtures::damaged_sphere(&mf_fixtures::DamageSpec {
            rings: 32,
            segments: 64,
            holes: 0,
            islands: 0,
            unwelded: 0,
            seed: 3,
        }),
        20.0,
    );
    let mut parts = vec![sphere];
    for d in [[23.0, 0.0, 4.0], [-6.0, 22.0, -8.0], [4.0, -9.0, 21.0]] {
        parts.push(mf_fixtures::translated(&mf_fixtures::cube(2.0), d));
    }
    let scan = mf_fixtures::merge(&parts);
    let transform = mf_fixtures::rotation_translation(2, 25.0, [-32.0, 0.0, 20.0]);
    let mut h = opened(vec![("scan", scan, transform)]);
    row(&h, "scan").click();
    settle(&mut h);
    assert_eq!(h.state().active, Some(id_of(&h, "scan")));
    h.get_by_label("Split").click();
    settle(&mut h);
    assert_eq!(h.state().selected_objects(), vec![id_of(&h, "scan")]);
    let history = history_len(&h);
    h.get_by_label("Cut").click();
    wait_until(&mut h, "the cut", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let names = objects(&h);
    assert!(names.contains(&"scan (top)".to_string()), "{names:?}");
    assert!(names.contains(&"scan (bottom)".to_string()), "{names:?}");
}

/// Opens the owner's own demo file (`artifacts/phase1/demo/boolean-demo.3mf`,
/// copied byte for byte into `fixtures/owner/`) exactly as File > Open
/// reads it. Not a regenerated stand-in: a fresh `gen-fixture
/// boolean-demo` differs from it in the last bit of some coordinates,
/// which is exactly the level the owner's Split refusal lived at.
fn opened_owner_demo() -> Harness<'static, MeshFixerApp> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/owner/boolean-demo.3mf"
    );
    let model = mf_io::read_model(std::path::Path::new(path)).unwrap();
    let objects = model
        .objects
        .into_iter()
        .map(|o| (o.name, o.mesh, o.transform))
        .collect::<Vec<_>>();
    opened(
        objects
            .iter()
            .map(|(n, m, t)| (n.as_str(), m.clone(), *t))
            .collect(),
    )
}

/// Split's plane as the panel placed it: horizontal through `point`.
fn assert_default_horizontal_plane_at(h: &Harness<'_, MeshFixerApp>, point: [f64; 3]) {
    let p = h.state().split.point();
    let n = h.state().split.normal();
    assert_eq!([p.x, p.y, p.z], point, "Split's default plane point");
    assert_eq!([n.x, n.y, n.z], [0.0, 0.0, 1.0], "horizontal");
}

/// Owner 2026-11-13, step (A), clicked exactly as the owner did: open
/// the demo file, select 'scan' and 'part' together, open Split, leave
/// the default horizontal plane at (2.5, 0, 20) (the selection's own
/// bounding-box centre), Keep Top, Cut. Both objects cross the plane;
/// both must be cut, never refused.
#[test]
fn owner_split_scan_and_part_together_keep_top_is_never_refused() {
    require_gpu!("owner_split_scan_and_part_together_keep_top_is_never_refused");
    let mut h = opened_owner_demo();
    assert_eq!(objects(&h), ["scan", "part", "crossed scan", "probe"]);
    select_together(&mut h, &["scan", "part"]);
    h.get_by_label("Split").click();
    settle(&mut h);
    assert_eq!(
        h.state().selected_objects(),
        vec![id_of(&h, "scan"), id_of(&h, "part")]
    );
    assert_default_horizontal_plane_at(&h, [2.5, 0.0, 20.0]);
    assert!(h.state().split.cap, "Cap on by default");
    let names = cut_with_keep(&mut h, "Top");
    assert_eq!(
        names,
        ["scan (top)", "part (top)", "crossed scan", "probe"],
        "{names:?}"
    );
    for name in ["scan (top)", "part (top)"] {
        let kept = world_bounds(&h, id_of(&h, name));
        assert!(kept.min[2] >= 20.0 - 1e-9, "{name}: {kept:?}");
        let mesh = h
            .state()
            .session
            .document()
            .object(id_of(&h, name))
            .unwrap()
            .mesh()
            .unwrap()
            .clone();
        let (p, t) = mesh.to_arrays();
        mf_boolean::validate_solid(&p, &t, &mf_runtime::RunContext::none())
            .unwrap_or_else(|e| panic!("{name}: not a closed solid: {e}"));
    }
}

/// Owner 2026-11-13, step (B), clicked exactly as the owner did: open
/// the demo file, select 'scan' and 'part', Combine (leaving one object,
/// 'scan'), then Split it at the same default plane, horizontal at
/// (2.5, 0, 20), Keep Top, Cap on, Cut. Never refused.
#[test]
fn owner_combine_scan_and_part_then_split_keep_top_is_never_refused() {
    require_gpu!("owner_combine_scan_and_part_then_split_keep_top_is_never_refused");
    let mut h = opened_owner_demo();
    select_together(&mut h, &["scan", "part"]);
    h.get_by_label("Combine").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "combine");
    let history = history_len(&h);
    panel_button(&h, "Combine").click();
    wait_until(&mut h, "the combine", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(objects(&h), ["scan", "crossed scan", "probe"]);
    row(&h, "scan").click();
    settle(&mut h);
    h.get_by_label("Split").click();
    settle(&mut h);
    assert_eq!(h.state().selected_objects(), vec![id_of(&h, "scan")]);
    assert_default_horizontal_plane_at(&h, [2.5, 0.0, 20.0]);
    assert!(h.state().split.cap, "Cap on");
    let names = cut_with_keep(&mut h, "Top");
    assert_eq!(names, ["scan (top)", "crossed scan", "probe"], "{names:?}");
    let mesh = h
        .state()
        .session
        .document()
        .object(id_of(&h, "scan (top)"))
        .unwrap()
        .mesh()
        .unwrap()
        .clone();
    let (p, t) = mesh.to_arrays();
    mf_boolean::validate_solid(&p, &t, &mf_runtime::RunContext::none())
        .unwrap_or_else(|e| panic!("not a closed solid: {e}"));
    assert!(world_bounds(&h, id_of(&h, "scan (top)")).min[2] >= 20.0 - 1e-9);
}

/// Owner 2026-11-13, the full report: Split 'scan' and 'part' together at
/// the default plane with Keep Bottom (a hemisphere and a half cube sunk
/// into its side), Combine them, Make solid on the result, then Hollow
/// (2 mm wall). "it can't hollow it" — never refused.
#[test]
fn owner_hollow_the_combined_hemisphere_and_half_cube_is_never_refused() {
    require_gpu!("owner_hollow_the_combined_hemisphere_and_half_cube_is_never_refused");
    let mut h = opened_owner_demo();
    select_together(&mut h, &["scan", "part"]);
    h.get_by_label("Split").click();
    settle(&mut h);
    assert_default_horizontal_plane_at(&h, [2.5, 0.0, 20.0]);
    let names = cut_with_keep(&mut h, "Bottom");
    assert_eq!(
        names,
        ["scan (bottom)", "part (bottom)", "crossed scan", "probe"],
        "{names:?}"
    );

    select_together(&mut h, &["scan (bottom)", "part (bottom)"]);
    h.get_by_label("Combine").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "combine");
    let history = history_len(&h);
    panel_button(&h, "Combine").click();
    wait_until(&mut h, "the combine", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(
        objects(&h),
        ["scan (bottom)", "crossed scan", "probe"],
        "{:?}",
        objects(&h)
    );

    row(&h, "scan (bottom)").click();
    settle(&mut h);
    h.get_by_label("Make solid").click();
    settle(&mut h);
    let history = history_len(&h);
    h.get_by_label("Make it solid").click();
    wait_until(&mut h, "make solid", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);

    let solid_id = id_of(&h, "scan (bottom)");
    let mesh = h
        .state()
        .session
        .document()
        .object(solid_id)
        .unwrap()
        .mesh()
        .unwrap()
        .clone();
    let (p, t) = mesh.to_arrays();
    mf_boolean::validate_solid(&p, &t, &mf_runtime::RunContext::none())
        .unwrap_or_else(|e| panic!("Make solid's own output isn't a valid solid: {e}"));

    h.get_by_label("Hollow").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "hollow");
    wait_until(&mut h, "the hollow preview", |a| {
        mf_desktop::hollow_tool::preview_render(a, solid_id).is_some()
    });
    settle(&mut h);
    let history = history_len(&h);
    panel_button(&h, "Hollow").click();
    wait_until(&mut h, "the hollow", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(
        h.state().refusal.is_none(),
        "Hollow refused a Make-solid-produced closed solid: {:?}",
        h.state().refusal
    );
    assert_eq!(history_len(&h), history + 1, "one undo step");
}

/// Owner 2026-11-14, take 3, clicked exactly as the owner did: Split
/// 'scan' alone (Keep Bottom), Make solid on 'scan (bottom)', Split
/// 'part' the same way, Combine the two into 'part (bottom)', open
/// Hollow (2 mm wall), click the flat cut face (a disc with the cube's
/// tab joined on), turn on "Open through this surface", Hollow. It was
/// refused (`open_through_region_too_curved`: shrinking the picked
/// outline by the wall crossed itself at the tab's neck); the opening
/// now follows the cavity itself, so it is never refused here, and the
/// result is one closed, manifold, open shell with the rim at the cut.
#[test]
fn owner_hollow_open_through_the_cut_face_of_scan_and_part_split_separately() {
    require_gpu!("owner_hollow_open_through_the_cut_face_of_scan_and_part_split_separately");
    let mut h = opened_owner_demo();

    row(&h, "scan").click();
    settle(&mut h);
    h.get_by_label("Split").click();
    settle(&mut h);
    let cut_z = h.state().split.point().z;
    let names = cut_with_keep(&mut h, "Bottom");
    assert!(names.contains(&"scan (bottom)".to_string()), "{names:?}");

    row(&h, "scan (bottom)").click();
    settle(&mut h);
    h.get_by_label("Make solid").click();
    settle(&mut h);
    let history = history_len(&h);
    h.get_by_label("Make it solid").click();
    wait_until(&mut h, "make solid", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);

    row(&h, "part").click();
    settle(&mut h);
    h.get_by_label("Split").click();
    settle(&mut h);
    assert_eq!(h.state().split.point().z, cut_z, "the same cut height");
    let names = cut_with_keep(&mut h, "Bottom");
    assert!(names.contains(&"part (bottom)".to_string()), "{names:?}");

    select_together(&mut h, &["part (bottom)", "scan (bottom)"]);
    h.get_by_label("Combine").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "combine");
    let history = history_len(&h);
    panel_button(&h, "Combine").click();
    wait_until(&mut h, "the combine", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let names = objects(&h);
    assert!(
        names.contains(&"part (bottom)".to_string())
            && !names.contains(&"scan (bottom)".to_string()),
        "{names:?}"
    );

    let id = id_of(&h, "part (bottom)");
    row(&h, "part (bottom)").click();
    settle(&mut h);
    h.get_by_label("Hollow").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "hollow");
    wait_until(&mut h, "the hollow preview", |a| {
        mf_desktop::hollow_tool::preview_render(a, id).is_some()
    });
    settle(&mut h);
    // The disc's own middle, on the cut face.
    let bb = world_bounds(&h, id);
    let disc = [
        (bb.min[0] + bb.max[0]) / 2.0,
        (bb.min[1] + bb.max[1]) / 2.0,
        cut_z,
    ];
    let p = h.state().screen_point(disc).unwrap();
    click_at(&mut h, p);
    settle(&mut h);
    let picked = h
        .state()
        .hollow
        .picked_region
        .as_ref()
        .map(|r| r.faces.len())
        .unwrap_or(0);
    assert!(
        picked > 2,
        "the click should pick the cut face: {picked} faces"
    );
    h.get_by_label("Open through this surface").click();
    settle(&mut h);
    assert!(h.state().hollow.open_through, "the checkbox should be on");

    let history = history_len(&h);
    panel_button(&h, "Hollow").click();
    wait_until(&mut h, "the hollow", |a| {
        a.busy.is_none()
            && (a.session.history_labels().len() > history
                || a.refusal.is_some()
                || a.error.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert!(h.state().error.is_none(), "{:?}", h.state().error);
    assert_eq!(history_len(&h), history + 1, "one undo step");
    let shell = mesh_of(&h, "part (bottom)");
    let topo = shell
        .topology_with(&mf_runtime::RunContext::none())
        .unwrap();
    assert_eq!(topo.boundary_edges, 0, "closed");
    assert!(
        topo.non_manifold_edges.is_empty() && topo.non_manifold_vertices == 0,
        "{topo:?}"
    );
    assert_eq!(topo.components.len(), 1, "one open shell");
    let top = world_bounds(&h, id).max[2];
    assert!((top - cut_z).abs() < 1e-9, "rim at {top}, cut at {cut_z}");
}

// ---------------------------------------------------------------------
// Split: cross-section picking and the bounded cut plane (Phase 1b).
// ---------------------------------------------------------------------

/// A two-lobed object with only one region ticked: the other lobe stays
/// one piece, never split, in whichever kept part it ends up in.
#[test]
fn split_one_region_ticked_leaves_the_other_lobe_whole() {
    require_gpu!("split_one_region_ticked_leaves_the_other_lobe_whole");
    let mut h = opened(vec![("lobes", two_lobed(), at([0.0, 0.0, 20.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    // Horizontal, through both lobes' middles (off their shared pole
    // seam by the same 1.3mm offset the other Split tests use).
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    let regions = mf_desktop::split_flow::region_preview(h.state());
    assert_eq!(
        regions.len(),
        2,
        "two disjoint lobes, two regions: {regions:?}"
    );
    // Untick the first region (as the panel's own checkbox would).
    let id = id_of(&h, "lobes");
    let untick = regions[0].seed_face;
    h.state_mut()
        .split
        .unticked_regions
        .entry(id)
        .or_default()
        .insert(untick);
    settle(&mut h);
    let names = cut_with_keep(&mut h, "Both");
    assert_eq!(names, ["lobes (top)", "lobes (bottom)"], "{names:?}");
    let sphere_faces = mf_fixtures::uv_sphere(32, 48).face_count();
    let top = h
        .state()
        .session
        .document()
        .object(id_of(&h, "lobes (top)"))
        .unwrap()
        .mesh()
        .unwrap()
        .face_count();
    let bottom = h
        .state()
        .session
        .document()
        .object(id_of(&h, "lobes (bottom)"))
        .unwrap()
        .mesh()
        .unwrap()
        .face_count();
    // The untouched lobe's whole faces are present, on top of the
    // ticked lobe's own cut faces, in one of the two kept parts.
    assert!(
        top > sphere_faces || bottom > sphere_faces,
        "top={top} bottom={bottom} sphere={sphere_faces}"
    );
}

/// A bounded disc placed over one lobe only cuts that lobe.
#[test]
fn split_bounded_disc_over_one_region_only() {
    require_gpu!("split_bounded_disc_over_one_region_only");
    let mut h = opened(vec![("lobes", two_lobed(), at([0.0, 0.0, 20.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    // Centre the plane's point over the lobe at world x=-40 (object
    // space, since the object's own transform is translation-only
    // here): the disc (diameter 40mm, radius 20) covers that lobe's
    // section (radius ~20 at most) but not the other lobe, 80mm away.
    h.state_mut().split.plane[9] = -40.0;
    h.state_mut().split.bound = true;
    h.state_mut().split.bound_shape = "disc".into();
    h.state_mut().split.bound_width = Some(mf_units::Length::new(40.0, Unit::Millimeter));
    settle(&mut h);
    let names = cut_with_keep(&mut h, "Both");
    assert_eq!(names, ["lobes (top)", "lobes (bottom)"], "{names:?}");
    let sphere_faces = mf_fixtures::uv_sphere(32, 48).face_count();
    let top = h
        .state()
        .session
        .document()
        .object(id_of(&h, "lobes (top)"))
        .unwrap()
        .mesh()
        .unwrap()
        .face_count();
    let bottom = h
        .state()
        .session
        .document()
        .object(id_of(&h, "lobes (bottom)"))
        .unwrap()
        .mesh()
        .unwrap()
        .face_count();
    assert!(
        top > sphere_faces || bottom > sphere_faces,
        "top={top} bottom={bottom} sphere={sphere_faces}"
    );
}

/// A bound that crosses a region's edge is refused by name, with the
/// owner's exact next action.
#[test]
fn split_bound_crossing_a_region_is_refused() {
    require_gpu!("split_bound_crossing_a_region_is_refused");
    let mut h = opened(vec![("lobes", two_lobed(), at([0.0, 0.0, 20.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    // The lobe at x=-40's section (at z=1.3 from its own centre) has
    // radius sqrt(20^2 - 1.3^2) =~ 19.96. A disc of radius 10 centred
    // at x=-25 (15mm from that lobe's centre) straddles that circle —
    // 15 is between |19.96-10|=9.96 and 19.96+10=29.96, so the two
    // circles genuinely cross, not one containing or missing the
    // other (a disc concentric with the lobe's own centre, by
    // contrast, is always cleanly inside or outside it, never a
    // straddle).
    h.state_mut().split.plane[9] = -25.0;
    h.state_mut().split.bound = true;
    h.state_mut().split.bound_shape = "disc".into();
    h.state_mut().split.bound_width = Some(mf_units::Length::new(20.0, Unit::Millimeter));
    settle(&mut h);
    let history = history_len(&h);
    scroll_and_click(&mut h, "Cut");
    wait_until(&mut h, "the refusal", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    let refusal = h.state().refusal.clone().expect("a refusal");
    assert_eq!(refusal.code, "split_region_crosses_bound");
    assert!(
        refusal
            .message
            .contains("Make the cut area larger or move it"),
        "{}",
        refusal.message
    );
    assert_eq!(history_len(&h), history, "no cut was made");
}

// ---------------------------------------------------------------------
// Combine: solid + solid + hole, with Undo.
// ---------------------------------------------------------------------

fn press_h(h: &mut Harness<'_, MeshFixerApp>) {
    h.key_press(egui::Key::H);
    settle(h);
}

fn row<'a>(h: &'a Harness<'_, MeshFixerApp>, name: &'a str) -> egui_kittest::Node<'a> {
    h.get_by_role_and_label(egui::accesskit::Role::Button, name)
}

/// Click `names`' Browser rows: the first plainly, the rest with Shift
/// (selected together).
fn select_together(h: &mut Harness<'_, MeshFixerApp>, names: &[&str]) {
    for (k, name) in names.iter().enumerate() {
        if k == 0 {
            row(h, name).click();
        } else {
            row(h, name).click_modifiers(egui::Modifiers::SHIFT);
        }
        settle(h);
    }
}

/// The widest node labelled `label`: the panel's own button where the
/// rail has an icon of the same name ("Combine").
fn panel_button<'a>(h: &'a Harness<'_, MeshFixerApp>, label: &'a str) -> egui_kittest::Node<'a> {
    h.query_all_by_label(label)
        .max_by(|a, b| a.rect().width().total_cmp(&b.rect().width()))
        .unwrap_or_else(|| panic!("no {label:?}"))
}

/// A block, a post on it (both solids) and a peg through its side (the
/// hole).
fn block_post_peg() -> Vec<(&'static str, Mesh, [f64; 12])> {
    vec![
        ("block", mf_fixtures::cube(20.0), at([0.0; 3])),
        ("post", mf_fixtures::cube(8.0), at([6.0, 6.0, 16.0])),
        ("peg", ball(10.0), at([20.0, 10.0, 10.0])),
    ]
}

/// Combine of solid + solid + hole: the post joins the block, the peg
/// cuts it and is used up, all in one undo step; Undo restores every
/// object and the hole role.
#[test]
fn combine_solid_solid_hole_then_undo() {
    require_gpu!("combine_solid_solid_hole_then_undo");
    let mut h = opened(block_post_peg());
    row(&h, "peg").click();
    settle(&mut h);
    press_h(&mut h); // peg becomes a Hole
    assert!(h.state().is_hole(id_of(&h, "peg")));
    select_together(&mut h, &["block", "post", "peg (hole)"]);
    h.get_by_label("Combine").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "combine");
    h.get_by_label("block · Solid");
    h.get_by_label("post · Solid");
    h.get_by_label("peg · Hole");

    let before_block = world_bounds(&h, id_of(&h, "block"));
    let history = history_len(&h);
    panel_button(&h, "Combine").click();
    wait_until(&mut h, "the combine", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(
        objects(&h),
        ["block"],
        "the post joined, the peg cut and used up"
    );
    assert_eq!(history_len(&h), history + 1, "one undo step");
    let block_id = id_of(&h, "block");
    let after_mesh = h
        .state()
        .session
        .document()
        .object(block_id)
        .unwrap()
        .mesh()
        .unwrap()
        .clone();
    assert!(
        after_mesh.triangle_count() > mf_fixtures::cube(20.0).triangle_count(),
        "the join and the cut both left their mark on the mesh"
    );
    // The combined block's bounds grew to include the post (which stuck
    // out above it), but the peg didn't extend its footprint past the
    // original block (it only cut a hole through the side).
    let combined = world_bounds(&h, block_id);
    assert!(combined.max[2] > before_block.max[2] + 1.0, "{combined:?}");

    h.get_by_label("Undo").click();
    wait_until(&mut h, "the undo", |a| {
        a.busy.is_none() && a.session.document().objects().len() == 3
    });
    settle(&mut h);
    assert_eq!(objects(&h), ["block", "post", "peg"], "{:?}", objects(&h));
    assert!(
        h.state().is_hole(id_of(&h, "peg")),
        "the role came back too"
    );
}

// ---------------------------------------------------------------------
// Delete a split part: Backspace, and the trash button.
// ---------------------------------------------------------------------

/// Splits a ball in two and returns the harness with the Browser open
/// (Split's Escape clears its own selection, which reopens the
/// Browser).
fn ball_split_in_two() -> Harness<'static, MeshFixerApp> {
    let mut h = opened(vec![("ball", ball(30.0), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    let history = history_len(&h);
    h.get_by_label("Cut").click();
    wait_until(&mut h, "the split", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    h
}

/// Backspace deletes the picked part (the key is bound the same as
/// Delete): only the other part remains, and it's undoable.
#[test]
fn delete_a_split_part_with_backspace() {
    require_gpu!("delete_a_split_part_with_backspace");
    let mut h = ball_split_in_two();
    row(&h, "ball (bottom)").click();
    settle(&mut h);
    assert_eq!(h.state().active, Some(id_of(&h, "ball (bottom)")));
    h.key_press(egui::Key::Backspace);
    wait_until(&mut h, "the delete", |a| {
        a.busy.is_none() && a.session.document().objects().len() == 1
    });
    settle(&mut h);
    assert_eq!(objects(&h), ["ball (top)"]);

    h.get_by_label("Undo").click();
    wait_until(&mut h, "the undo", |a| {
        a.busy.is_none() && a.session.document().objects().len() == 2
    });
    settle(&mut h);
    assert_eq!(objects(&h), ["ball (top)", "ball (bottom)"]);
}

/// The Browser row's own trash button (shown on hover) deletes that
/// row's object, regardless of which object is active.
#[test]
fn delete_a_split_part_with_the_trash_button() {
    require_gpu!("delete_a_split_part_with_the_trash_button");
    let mut h = ball_split_in_two();
    assert_eq!(objects(&h), ["ball (top)", "ball (bottom)"]);
    let bottom_row = row(&h, "ball (bottom)");
    let pos = bottom_row.rect().center();
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    settle(&mut h);
    h.get_by_label("Delete ball (bottom)").click();
    wait_until(&mut h, "the delete", |a| {
        a.busy.is_none() && a.session.document().objects().len() == 1
    });
    settle(&mut h);
    assert_eq!(objects(&h), ["ball (top)"]);

    h.get_by_label("Undo").click();
    wait_until(&mut h, "the undo", |a| {
        a.busy.is_none() && a.session.document().objects().len() == 2
    });
    settle(&mut h);
    assert_eq!(objects(&h), ["ball (top)", "ball (bottom)"]);
}

// ---------------------------------------------------------------------
// Connect: place 2 dowels, Apply, check the holes and the dowel
// objects.
// ---------------------------------------------------------------------

fn cap_point(h: &Harness<'_, MeshFixerApp>, uv: [f64; 2]) -> egui::Pos2 {
    let a = h.state();
    let j = a.split_flow.open_joint().unwrap();
    let lift = j.frame.cap_point(j.solid()) - j.frame.point;
    a.screen_point((j.frame.at(uv) + lift).to_array()).unwrap()
}

fn placements(h: &Harness<'_, MeshFixerApp>) -> Vec<mf_operations::Placement> {
    h.state()
        .split_flow
        .open_joint()
        .map(|j| j.placements.clone())
        .unwrap_or_default()
}

/// How much of the window's bottom edge the status bar covers (its own
/// text row): a node whose rect's bottom falls within this band is
/// visually hidden behind it even though it's within the window's own
/// pixel bounds, so [`scroll_and_click`] keeps scrolling past it rather
/// than treating it as "on screen" and clicking through the status bar.
const STATUS_BAR_HEIGHT: f32 = 36.0;

/// Scrolls the tool panel until `label` is inside the window (clear of
/// the status bar at the bottom), then clicks it (Connect's panel is
/// taller than the window).
fn scroll_and_click(h: &mut Harness<'_, MeshFixerApp>, label: &str) {
    let over = egui::pos2(200.0, WINDOW.1 * 0.5);
    h.event(egui::Event::PointerMoved(over));
    h.step();
    let mut tries = 0;
    loop {
        let rect = h.get_by_label(label).rect();
        if rect.bottom() <= WINDOW.1 - STATUS_BAR_HEIGHT && rect.top() >= 40.0 {
            break;
        }
        tries += 1;
        assert!(tries < 400, "scrolled to {label:?}");
        let down = rect.bottom() > WINDOW.1 - STATUS_BAR_HEIGHT;
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, if down { -1.0 } else { 1.0 }),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(2);
    }
    h.run_steps(10);
    h.get_by_label(label).click();
}

/// Place 2 dowels on a cut joint, Apply, and check both cut faces grew
/// extra faces (the holes) and the dowel objects landed on the plate.
#[test]
fn connect_places_two_dowels_and_applies() {
    require_gpu!("connect_places_two_dowels_and_applies");
    let mut h = opened(vec![("ball", ball(30.0), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    let history = history_len(&h);
    h.get_by_label("Cut").click();
    wait_until(&mut h, "the cut", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let (top_id, bottom_id) = (id_of(&h, "ball (top)"), id_of(&h, "ball (bottom)"));
    let faces_before = |h: &Harness<'_, MeshFixerApp>, id: mf_command::ObjectId| {
        h.state()
            .session
            .document()
            .object(id)
            .unwrap()
            .mesh()
            .unwrap()
            .face_count()
    };
    let (top_faces_before, bottom_faces_before) =
        (faces_before(&h, top_id), faces_before(&h, bottom_id));

    h.get_by_label("Connect").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);

    // Place 2 dowels, well apart on the cut face.
    let p1 = cap_point(&h, [-10.0, 0.0]);
    click_at(&mut h, p1);
    settle(&mut h);
    let p2 = cap_point(&h, [10.0, 0.0]);
    click_at(&mut h, p2);
    settle(&mut h);
    let ps = placements(&h);
    assert_eq!(ps.len(), 2, "{ps:?}");
    assert!(
        ps.iter().all(|p| p.connector == "joint.dowel"),
        "dowel is the default connector: {ps:?}"
    );

    let history = history_len(&h);
    scroll_and_click(&mut h, "Add connectors");
    wait_until(&mut h, "the connectors", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(history_len(&h), history + 1, "one undo step");

    // The holes: both cut faces gained faces (the subtracted holes).
    let (top_faces_after, bottom_faces_after) =
        (faces_before(&h, top_id), faces_before(&h, bottom_id));
    assert!(
        top_faces_after > top_faces_before,
        "top gained holes: {top_faces_before} -> {top_faces_after}"
    );
    assert!(
        bottom_faces_after > bottom_faces_before,
        "bottom gained holes: {bottom_faces_before} -> {bottom_faces_after}"
    );

    // The dowel objects: 2 loose dowels, standing on the plate.
    let names = objects(&h);
    let dowels: Vec<&String> = names.iter().filter(|n| n.contains("dowel")).collect();
    assert_eq!(dowels.len(), 2, "{names:?}");
    for name in dowels {
        let id = id_of(&h, name);
        let bb = world_bounds(&h, id);
        assert!(
            bb.min[2].abs() < 1e-6,
            "{name} should stand on the plate: {bb:?}"
        );
    }
}

// ---------------------------------------------------------------------
// Sculpt: one inflate stroke; "stay on surface" leaves the cap
// untouched.
// ---------------------------------------------------------------------

/// A world point well inside the camera-facing side of the sphere
/// centred at `center` with `radius`.
fn facing_point(h: &Harness<'_, MeshFixerApp>, center: [f64; 3], radius: f64) -> egui::Pos2 {
    let cam = h.state().camera.unwrap();
    let to_eye = (cam.eye() - DVec3::from(center)).normalize();
    let world = DVec3::from(center) + to_eye * (radius * 0.8);
    h.state().screen_point(world.to_array()).unwrap()
}

/// The cap's faces (coplanar with the cut, at world z = `cap_z`, within
/// 1e-6): their corner positions, so a before/after comparison can
/// check they never moved.
fn cap_corner_positions(
    h: &Harness<'_, MeshFixerApp>,
    id: mf_command::ObjectId,
    cap_z: f64,
) -> Vec<[f64; 3]> {
    let doc = h.state().session.document();
    let o = doc.object(id).unwrap();
    let mesh = o.mesh().unwrap();
    let t = DVec3::new(o.transform[9], o.transform[10], o.transform[11]);
    let mut out = Vec::new();
    for (_, tri) in mesh.faces() {
        let corners: Vec<[f64; 3]> = tri
            .iter()
            .map(|&v| {
                let p = mesh.corner_position(v);
                (DVec3::from(p) + t).to_array()
            })
            .collect();
        if corners.iter().all(|c| (c[2] - cap_z).abs() < 1e-6) {
            out.extend(corners);
        }
    }
    assert!(!out.is_empty(), "the cap must have at least one face");
    out
}

/// One inflate stroke near a split's cap, with "stay on surface" on
/// (the default): the curved surface changes, but the flat cap (a
/// separate face group across the sharp crease where it meets the
/// curved surface) never moves.
#[test]
fn sculpt_inflate_stroke_leaves_the_cap_untouched_with_stay_on_surface() {
    require_gpu!("sculpt_inflate_stroke_leaves_the_cap_untouched_with_stay_on_surface");
    let mut h = opened(vec![("ball", ball(20.0), at([0.0, 0.0, 20.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    // A plane close to one side, so the cap is a definite, small patch
    // and the curved surface right next to it is easy to aim at.
    h.state_mut().split.plane[11] = 32.0;
    settle(&mut h);
    let history = history_len(&h);
    h.get_by_label("Cut").click();
    wait_until(&mut h, "the cut", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    // Keep the top part only (closed; one object, one cap at z=32).
    let names = objects(&h);
    assert!(names.contains(&"ball (top)".to_owned()), "{names:?}");
    let top_id = id_of(&h, "ball (top)");

    let cap_before = cap_corner_positions(&h, top_id, 32.0);

    h.get_by_label("Split").click();
    settle(&mut h);
    h.get_by_label("Sculpt").click();
    settle(&mut h);
    assert!(h.state().sculpt.stay_on_surface, "on by default");
    h.state_mut().sculpt.strength = 1.0;

    // A dab well away from the cap, on the curved surface (the ball's
    // equator, facing the camera).
    let dab = facing_point(&h, [0.0, 0.0, 20.0], 20.0);
    let dab_world = {
        let cam = h.state().camera.unwrap();
        let center = DVec3::new(0.0, 0.0, 20.0);
        let to_eye = (cam.eye() - center).normalize();
        (center + to_eye * (20.0 * 0.8)).to_array()
    };
    h.state_mut().sculpt.radius_pt =
        mf_desktop::sculpt_tool::radius_pt_for_world(h.state(), dab_world, 6.0).unwrap();
    let before_mesh = h.state().active_mesh().unwrap().clone();
    h.event(egui::Event::PointerMoved(dab));
    h.event(egui::Event::PointerButton {
        pos: dab,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    let to = dab + egui::vec2(20.0, 0.0);
    for i in 1..=6 {
        let p = dab + (to - dab) * (i as f32 / 6.0);
        h.event(egui::Event::PointerMoved(p));
        h.step();
    }
    wait_until(&mut h, "a dab recorded", |a| {
        a.sculpt_stroke.as_ref().is_some_and(|s| s.dab_count() > 0)
    });
    h.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    wait_until(&mut h, "the stroke to commit", |a| {
        a.sculpt_stroke.is_none() && a.busy.is_none()
    });
    settle(&mut h);

    let after_mesh = h.state().active_mesh().unwrap().clone();
    assert!(
        !Mesh::ptr_eq(&before_mesh, &after_mesh),
        "the stroke changed the mesh"
    );
    let cap_after = cap_corner_positions(&h, top_id, 32.0);
    assert_eq!(
        cap_before, cap_after,
        "\"stay on this surface\" must never move the cap's own faces"
    );
}

// ---------------------------------------------------------------------
// Sculpt: the brush radius is screen-relative (owner bug 2026-11-14):
// the default radius must stay a small local patch regardless of the
// document's units, on a metre-scale scan and on a millimetre-scale
// primitive. Before the fix, the panel's "5" was read as 5 *document
// units*, which is 5 m on the Stanford bunny (imported in metres) —
// enough to move all 34,978 of its vertices in one stroke.
// ---------------------------------------------------------------------

/// Half of `bb`'s shortest side: a radius [`facing_point`] can use that
/// always lands inside the box along every axis, so the sampled dab
/// stays on the mesh's own surface rather than drifting past its
/// silhouette (unlike `bb.diagonal() * 0.5`, which overshoots on
/// anything that isn't a cube).
fn inscribed_radius(bb: &Aabb) -> f64 {
    (0..3)
        .map(|k| (bb.max[k] - bb.min[k]) * 0.5)
        .fold(f64::MAX, f64::min)
}

/// How many of `before`'s live vertices moved (by more than `eps`) in
/// `after`, as a fraction of `before`'s vertex count. Compared by
/// `VertexId` (stable across one stroke, which only moves vertices,
/// never renumbers them unless dynamic detail is on — off here).
fn fraction_of_vertices_moved(before: &Mesh, after: &Mesh, eps: f64) -> f64 {
    let moved = before
        .vertices()
        .filter(|&(id, p)| match after.position(id) {
            Some(q) => {
                let d2 = (0..3).map(|k| (p[k] - q[k]).powi(2)).sum::<f64>();
                d2.sqrt() > eps
            }
            // Gone in `after`: dynamic detail would explain this, but it's
            // off by default, so this never happens in these tests.
            None => true,
        })
        .count();
    moved as f64 / before.vertex_count().max(1) as f64
}

/// One Inflate stroke at the panel's default settings, dragged a short
/// distance on the active object's camera-facing surface near
/// `center`/`radius` (world space): the mesh before and after.
fn one_default_inflate_stroke(
    h: &mut Harness<'_, MeshFixerApp>,
    center: [f64; 3],
    radius: f64,
) -> (Mesh, Mesh) {
    h.get_by_label("Sculpt").click();
    settle(h);
    assert_eq!(
        h.state().sculpt.brush,
        mf_operations::sculpt::BrushKind::Inflate,
        "Inflate is the default brush"
    );
    let before_mesh = h.state().active_mesh().unwrap().clone();
    let dab = facing_point(h, center, radius);
    h.event(egui::Event::PointerMoved(dab));
    h.event(egui::Event::PointerButton {
        pos: dab,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    let to = dab + egui::vec2(15.0, 0.0);
    for i in 1..=6 {
        let p = dab + (to - dab) * (i as f32 / 6.0);
        h.event(egui::Event::PointerMoved(p));
        h.step();
    }
    wait_until(h, "a dab recorded", |a| {
        a.sculpt_stroke.as_ref().is_some_and(|s| s.dab_count() > 0)
    });
    h.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    wait_until(h, "the stroke to commit", |a| {
        a.sculpt_stroke.is_none() && a.busy.is_none()
    });
    settle(h);
    let after_mesh = h.state().active_mesh().unwrap().clone();
    (before_mesh, after_mesh)
}

/// The owner's exact repro: open the Stanford bunny (imported in
/// metres, ~0.156 m across), choose Sculpt, Inflate (the default
/// brush), and drag one stroke. At the panel's default radius, the
/// stroke must move only a local patch — under 5% of the bunny's
/// 34,978 vertices — never the whole model.
#[test]
fn sculpt_inflate_stroke_moves_only_a_local_patch_on_the_bunny_in_metres() {
    require_gpu!("sculpt_inflate_stroke_moves_only_a_local_patch_on_the_bunny_in_metres");
    let Some(path) = mf_fixtures::real::path(
        "stanford-bunny.obj",
        "sculpt_inflate_stroke_moves_only_a_local_patch_on_the_bunny_in_metres",
    ) else {
        return;
    };
    let model = mf_io::read_model(&path).unwrap();
    let bunny = model.objects.into_iter().next().unwrap().mesh;
    let bb = bunny.bounding_box().unwrap();
    assert!(
        bb.diagonal() < 1.0,
        "the raw bunny should be metre-scale, got diagonal {}",
        bb.diagonal()
    );
    let vertex_count = bunny.vertex_count();
    let mut h = opened_with_units(vec![("bunny", bunny, at([0.0, 0.0, 0.0]))], Unit::Meter);
    assert_eq!(h.state().session.document().units(), Unit::Meter);

    let (before_mesh, after_mesh) =
        one_default_inflate_stroke(&mut h, bb.center(), inscribed_radius(&bb));
    assert!(
        !Mesh::ptr_eq(&before_mesh, &after_mesh),
        "the stroke must change the mesh"
    );
    let fraction = fraction_of_vertices_moved(&before_mesh, &after_mesh, 1e-9);
    assert!(
        fraction < 0.05,
        "one Inflate stroke at the default radius moved {:.1}% of {vertex_count} vertices \
         (expected under 5%, a local patch): the radius must be screen-relative, not \
         5 document units (5 m on this document)",
        fraction * 100.0,
    );
}

/// The same check on a millimetre-scale model (a ~50 mm sphere): the
/// default radius must still read as a small local patch there too, so
/// the fix is screen-relative rather than merely "assume metres".
#[test]
fn sculpt_inflate_stroke_moves_only_a_local_patch_on_a_small_mm_model() {
    require_gpu!("sculpt_inflate_stroke_moves_only_a_local_patch_on_a_small_mm_model");
    // Fine enough that even a few-millimetre brush radius clearly
    // displaces some vertices (unlike `ball`'s coarse 32x48 sphere,
    // where a handful of millimetres can fall between vertices and
    // move none at all — not what this test is probing).
    let sphere = mf_fixtures::scaled(&mf_fixtures::uv_sphere(96, 144), 25.0); // ~50 mm across
    let vertex_count = sphere.vertex_count();
    let bb = sphere.bounding_box().unwrap();
    let mut h = opened(vec![("ball", sphere, at([0.0, 0.0, 0.0]))]);

    let (before_mesh, after_mesh) =
        one_default_inflate_stroke(&mut h, bb.center(), inscribed_radius(&bb));
    assert!(
        !Mesh::ptr_eq(&before_mesh, &after_mesh),
        "the stroke must change the mesh"
    );
    let fraction = fraction_of_vertices_moved(&before_mesh, &after_mesh, 1e-9);
    assert!(
        fraction < 0.20,
        "one Inflate stroke at the default radius moved {:.1}% of {vertex_count} vertices \
         on a 50 mm model (expected under 20%)",
        fraction * 100.0,
    );
}

// ---------------------------------------------------------------------
// Transform: move, then Undo.
// ---------------------------------------------------------------------

/// Opens Transform with T and waits until its gizmo is drawn.
fn open_transform(h: &mut Harness<'_, MeshFixerApp>) {
    h.key_press(egui::Key::T);
    wait_until(h, "the gizmo", |a| a.transform_gizmo().is_some());
}

fn transform_of(h: &Harness<'_, MeshFixerApp>, id: mf_command::ObjectId) -> [f64; 12] {
    h.state().session.document().object(id).unwrap().transform
}

fn x_arrow(h: &Harness<'_, MeshFixerApp>) -> (egui::Pos2, egui::Vec2) {
    let app = h.state();
    let g = app.transform_gizmo().unwrap();
    let cam = app.camera.unwrap();
    let rect = app.viewport.last_rect.unwrap();
    let view = (rect.width() as f64, rect.height() as f64);
    let shape = g
        .handles(&cam, view)
        .into_iter()
        .find(|s| s.handle == mf_render::Handle::Axis(0))
        .expect("the X arrow is drawn");
    let mf_render::HandleGeometry::Arrow {
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

/// Dragging the gizmo's X arrow moves the object (one undo step), and
/// Undo puts it back exactly.
#[test]
fn transform_move_then_undo() {
    require_gpu!("transform_move_then_undo");
    let mut h = opened(vec![("cube", mf_fixtures::cube(10.0), at([0.0; 3]))]);
    let id = id_of(&h, "cube");
    open_transform(&mut h);
    settle(&mut h);
    let before = transform_of(&h, id);
    let revision = h.state().session.current();
    let history = history_len(&h);

    let (p, dir_x) = x_arrow(&h);
    let to = p + dir_x * 100.0;
    drag(&mut h, p, to, 10);
    wait_until(&mut h, "the move commits", |a| {
        a.busy.is_none() && a.session.history_labels().len() == history + 1
    });
    settle(&mut h);

    let after = transform_of(&h, id);
    assert!(after[9] > before[9] + 1.0, "{after:?}");
    assert_eq!(after[..9], before[..9], "only the translation changes");

    // `history_labels` is the *whole* linear view (undo and redo both
    // stay in it); the current position is `session.current()` /
    // `history_cursor()`, not its length, which Undo never shrinks.
    h.get_by_label("Undo").click();
    wait_until(&mut h, "the undo", |a| {
        a.busy.is_none() && a.session.current() == revision
    });
    settle(&mut h);
    assert_eq!(
        transform_of(&h, id),
        before,
        "one undo puts it back exactly"
    );
}

// ---------------------------------------------------------------------
// Transform: Scale mode (fix round 2026-11-12: dragging a scale handle
// used to move the object instead of scaling it — `Gizmo::begin`'s
// `ScaleAxis` anchor was the same shape as a move's, and `Drag::update`
// couldn't tell them apart).
// ---------------------------------------------------------------------

/// Switches the open Transform tool to Scale mode (R) and waits for the
/// gizmo to redraw in it.
fn open_scale_mode(h: &mut Harness<'_, MeshFixerApp>) {
    h.key_press(egui::Key::R);
    wait_until(h, "scale mode", |a| {
        a.transform_gizmo()
            .is_some_and(|g| g.mode == mf_render::GizmoMode::Scale)
    });
    h.run_steps(2);
}

/// A point to grab (and the direction to drag it outward) on `handle`,
/// read from the gizmo's own drawn geometry, the same way [`x_arrow`]
/// does for a move handle.
fn handle_point(
    h: &Harness<'_, MeshFixerApp>,
    handle: mf_render::Handle,
) -> (egui::Pos2, egui::Vec2) {
    let app = h.state();
    let g = app.transform_gizmo().unwrap();
    let cam = app.camera.unwrap();
    let rect = app.viewport.last_rect.unwrap();
    let view = (rect.width() as f64, rect.height() as f64);
    let shape = g
        .handles(&cam, view)
        .into_iter()
        .find(|s| s.handle == handle)
        .unwrap_or_else(|| panic!("{handle:?} is drawn"));
    let (at, dir) = match shape.geometry {
        mf_render::HandleGeometry::Arrow {
            shaft_start, tip, ..
        } => {
            let mid = ((shaft_start.0 + tip.0) / 2.0, (shaft_start.1 + tip.1) / 2.0);
            let d = egui::vec2(
                (tip.0 - shaft_start.0) as f32,
                (tip.1 - shaft_start.1) as f32,
            )
            .normalized();
            (mid, d)
        }
        // A point handle (`ScaleUniform`/`Centre`) has no arrow
        // direction of its own: start the drag at its edge, straight
        // down from its centre, same as a real grab (starting exactly
        // on the centre pixel would make the drag's own start distance
        // from the pivot ~0, blowing up the scale ratio on the very
        // first step).
        mf_render::HandleGeometry::Point { center, radius_px } => {
            // Comfortably inside the disc's own hit tolerance (not
            // exactly on its edge, which risks landing just outside by
            // float error and hitting nothing), but away from dead
            // centre, so the drag's own start distance from the pivot
            // isn't ~0 (that blows up the scale ratio on the first step).
            let near_edge = (center.0, center.1 + radius_px * 0.5);
            (near_edge, egui::vec2(0.0, 1.0))
        }
        other => panic!("{handle:?}'s geometry is {other:?}, not a point or an arrow"),
    };
    (rect.left_top() + egui::vec2(at.0 as f32, at.1 as f32), dir)
}

/// Drags `handle` outward by `px` screen points and waits for the commit.
fn drag_scale_handle(
    h: &mut Harness<'_, MeshFixerApp>,
    handle: mf_render::Handle,
    px: f32,
    history: usize,
) {
    let (p, dir) = handle_point(h, handle);
    let to = p + dir * px;
    drag(h, p, to, 10);
    wait_until(h, "the scale commits", |a| {
        a.busy.is_none() && a.session.history_labels().len() == history + 1
    });
    settle(h);
}

/// Dragging each `ScaleAxis` handle outward scales only that axis,
/// leaves the other two at 1.0, keeps the object's translation fixed
/// (a ball is centred on its own local origin, unlike
/// `mf_fixtures::cube`, whose local origin is a corner: placed at the
/// world origin, its bounding-box centre — the gizmo's pivot — coincides
/// exactly with the transform's translation column), and one undo
/// restores it exactly.
#[test]
fn transform_scale_axis_handles_scale_one_axis_and_undo_restores_it() {
    require_gpu!("transform_scale_axis_handles_scale_one_axis_and_undo_restores_it");
    for (i, axis_name) in [(0u8, "X"), (1, "Y"), (2, "Z")] {
        let mut h = opened(vec![("ball", ball(5.0), at([0.0; 3]))]);
        let id = id_of(&h, "ball");
        open_transform(&mut h);
        settle(&mut h);
        open_scale_mode(&mut h);
        let before = transform_of(&h, id);
        let revision = h.state().session.current();
        let history = history_len(&h);

        drag_scale_handle(&mut h, mf_render::Handle::ScaleAxis(i), 60.0, history);

        let after = transform_of(&h, id);
        let decomp = mf_render::decompose(&after);
        let scale = match decomp {
            mf_render::Decomposed::Trs { scale, .. } => scale,
            mf_render::Decomposed::Sheared => panic!("{axis_name}: expected Trs, got Sheared"),
        };
        let axes = [scale.x, scale.y, scale.z];
        assert!(
            axes[i as usize] > 1.0 + 1e-6,
            "{axis_name}: dragging its handle outward should scale it up: {scale:?}"
        );
        for (j, &s) in axes.iter().enumerate() {
            if j != i as usize {
                assert!(
                    (s - 1.0).abs() < 1e-6,
                    "{axis_name}: axis {j} should stay 1.0, not scale: {scale:?}"
                );
            }
        }
        // The cube's own bounding-box centre is placed exactly at the
        // world origin (the transform's translation column), so the
        // gizmo's pivot coincides with it: a scale about the pivot
        // leaves the transform's translation column untouched — unlike
        // the bug, which moved it along the dragged axis.
        assert_eq!(
            after[9..12],
            before[9..12],
            "{axis_name}: the pivot (the object's own centre here) must stay fixed, \
             translation must not change: before {before:?}, after {after:?}"
        );

        h.get_by_label("Undo").click();
        wait_until(&mut h, "the undo", |a| {
            a.busy.is_none() && a.session.current() == revision
        });
        settle(&mut h);
        assert_eq!(
            transform_of(&h, id),
            before,
            "{axis_name}: one undo puts it back exactly"
        );
    }
}

/// Dragging the uniform-scale centre handle scales all three axes
/// equally, keeps the pivot (and so the translation, for a centred
/// ball) fixed, and one undo restores it exactly.
#[test]
fn transform_uniform_scale_handle_scales_all_axes_and_undo_restores_it() {
    require_gpu!("transform_uniform_scale_handle_scales_all_axes_and_undo_restores_it");
    let mut h = opened(vec![("ball", ball(5.0), at([0.0; 3]))]);
    let id = id_of(&h, "ball");
    open_transform(&mut h);
    settle(&mut h);
    open_scale_mode(&mut h);
    let before = transform_of(&h, id);
    let revision = h.state().session.current();
    let history = history_len(&h);

    drag_scale_handle(&mut h, mf_render::Handle::ScaleUniform, 60.0, history);

    let after = transform_of(&h, id);
    let decomp = mf_render::decompose(&after);
    let scale = match decomp {
        mf_render::Decomposed::Trs { scale, .. } => scale,
        mf_render::Decomposed::Sheared => panic!("expected Trs, got Sheared"),
    };
    assert!(scale.x > 1.0 + 1e-6, "{scale:?}");
    assert!((scale.x - scale.y).abs() < 1e-6, "{scale:?}");
    assert!((scale.y - scale.z).abs() < 1e-6, "{scale:?}");
    assert_eq!(
        after[9..12],
        before[9..12],
        "the pivot must stay fixed: before {before:?}, after {after:?}"
    );

    h.get_by_label("Undo").click();
    wait_until(&mut h, "the undo", |a| {
        a.busy.is_none() && a.session.current() == revision
    });
    settle(&mut h);
    assert_eq!(
        transform_of(&h, id),
        before,
        "one undo puts it back exactly"
    );
}

// ---------------------------------------------------------------------
// Parametric shape: edit a param, then Undo.
// ---------------------------------------------------------------------

/// Places a parametric Box next to a scan, through the real Parts
/// panel.
fn place_box(h: &mut Harness<'_, MeshFixerApp>) -> mf_command::ObjectId {
    h.get_by_label("Parts").click();
    settle(h);
    let history = history_len(h);
    h.get_by_label("Box").click();
    wait_until(h, "the shape placed", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(h);
    h.state().active.unwrap()
}

fn shape_width(h: &Harness<'_, MeshFixerApp>, id: mf_command::ObjectId) -> f64 {
    let bb = h
        .state()
        .session
        .document()
        .object(id)
        .unwrap()
        .mesh()
        .unwrap()
        .bounding_box()
        .unwrap();
    bb.max[0] - bb.min[0]
}

/// Editing a placed shape's Width param regenerates its mesh as one
/// undo step; Undo restores the original width exactly.
#[test]
fn parametric_shape_edit_param_then_undo() {
    require_gpu!("parametric_shape_edit_param_then_undo");
    let mut h = opened(vec![("scan", ball(10.0), at([0.0, 0.0, 10.0]))]);
    let id = place_box(&mut h);
    let before_width = shape_width(&h, id);
    assert!((before_width - 10.0).abs() < 1e-9, "{before_width}");

    h.get_by_label("Select").click();
    settle(&mut h);
    h.state_mut().set_active(id);
    settle(&mut h);
    h.get_by_label("Width");

    let history = history_len(&h);
    mf_desktop::shape_panel::set_param(h.state_mut(), "width", mf_spec::ParamValue::Float(25.0));
    wait_until(&mut h, "the width change", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    assert_eq!(history_len(&h), history + 1, "one undo step");
    assert!((shape_width(&h, id) - 25.0).abs() < 1e-9, "regenerated");

    h.state_mut().undo();
    settle(&mut h);
    assert!(
        (shape_width(&h, id) - before_width).abs() < 1e-9,
        "undo restored the original width"
    );
}

// ---------------------------------------------------------------------
// Dovetail: place from Connect's shortcut, rotate 90° with Transform,
// apply (Attach), and check the tail exits the piece boundary and the
// groove matches within the clearance.
// ---------------------------------------------------------------------

/// The dovetail tail object in the document (its name ends "(tail)",
/// `connector_tool::group_members`'s naming).
fn tail_id(h: &Harness<'_, MeshFixerApp>) -> mf_command::ObjectId {
    id_of(h, "Dovetail (tail)")
}

fn groove_id(h: &Harness<'_, MeshFixerApp>) -> mf_command::ObjectId {
    id_of(h, "Dovetail (groove)")
}

/// A 90°-about-Z rotation of `t`'s linear part, same translation.
fn rotated_90_z(t: &[f64; 12]) -> [f64; 12] {
    let rot = glam::DMat3::from_axis_angle(glam::DVec3::Z, std::f64::consts::FRAC_PI_2);
    let before = mf_desktop::shape_panel::rotation(t);
    let after = rot * before;
    [
        after.x_axis.x,
        after.x_axis.y,
        after.x_axis.z,
        after.y_axis.x,
        after.y_axis.y,
        after.y_axis.z,
        after.z_axis.x,
        after.z_axis.y,
        after.z_axis.z,
        t[9],
        t[10],
        t[11],
    ]
}

/// Places a dovetail on the open joint through Connect's shortcut
/// button, waits for it to commit, and returns the tail's object id.
fn place_dovetail_via_connect(h: &mut Harness<'_, MeshFixerApp>) -> mf_command::ObjectId {
    let history = history_len(h);
    scroll_and_click(h, "Dovetail");
    wait_until(h, "the dovetail placed", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    tail_id(h)
}

/// The exact minimum distance (mm) between `a` and `b`'s world-placed
/// meshes (`mf_geometry::minimum_distance`, the same BVH measure every
/// connector's own fit check uses).
fn world_min_distance(
    h: &Harness<'_, MeshFixerApp>,
    a: mf_command::ObjectId,
    b: mf_command::ObjectId,
) -> f64 {
    let doc = h.state().session.document();
    let oa = doc.object(a).unwrap();
    let ob = doc.object(b).unwrap();
    let mesh_a = oa.mesh().unwrap().clone();
    let mesh_b = ob.mesh().unwrap().clone();
    let mat = |t: &[f64; 12]| {
        glam::DMat4::from_cols_array(&[
            t[0], t[1], t[2], 0.0, t[3], t[4], t[5], 0.0, t[6], t[7], t[8], 0.0, t[9], t[10],
            t[11], 1.0,
        ])
    };
    // `a`'s frame: mesh_b is placed by b_to_a = a_world^-1 * b_world.
    let b_to_a = mat(&oa.transform).inverse() * mat(&ob.transform);
    let ctx = mf_runtime::RunContext::none();
    let bvh = mf_geometry::Bvh::build(&mesh_a, &ctx).unwrap();
    mf_geometry::minimum_distance(&mesh_a, &bvh, &mesh_b, &b_to_a, &ctx)
        .unwrap()
        .expect("both meshes have faces")
}

/// Connect's Dovetail shortcut drops a linked tail/groove pair on the
/// open joint; rotating the whole group 90° with Transform
/// (`object.move_group`) still leaves the tail exiting the piece it's
/// sunk into (`dovetail_tool::ends_inside`) and the groove matching the
/// tail within the fit's clearance (`minimum_distance`); applying it
/// (Attach) folds both into their pieces, one undo step.
#[test]
fn dovetail_placed_rotated_and_applied_exits_the_piece_and_fits_the_groove() {
    require_gpu!("dovetail_placed_rotated_and_applied_exits_the_piece_and_fits_the_groove");
    // A flat-capped block rather than the ball Connect's dowel test
    // uses: a sliding dovetail needs real flat room either side of the
    // cut to extend through without self-intersecting against
    // curvature. Long and narrow (100 x 30 in x/y) so its cut cap's
    // nearest edge, from the centre, is unambiguous and axis-aligned
    // (along y, 15 mm away): Connect's Dovetail shortcut aims straight
    // at it rather than cutting a square cap's corner.
    let block = {
        let (p, t) = mf_fixtures::cube(1.0).to_arrays();
        let p = p
            .into_iter()
            .map(|[x, y, z]| [x * 100.0, y * 30.0, z * 60.0])
            .collect();
        Mesh::new(p, t).unwrap()
    };
    let mut h = opened(vec![("block", block, at([-50.0, -15.0, 0.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    let history = history_len(&h);
    h.get_by_label("Cut").click();
    wait_until(&mut h, "the cut", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);

    h.get_by_label("Connect").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);

    let tail = place_dovetail_via_connect(&mut h);
    let groove = groove_id(&h);

    // Rotate the linked pair 90° about Z with Transform
    // (`commit_transform`, the same path the gizmo's ring drag commits
    // through): both the tail and the groove move together, one
    // `object.move_group` step, possibly followed by a second,
    // `extend_through_edge`'s own re-evaluation (`shape_panel::
    // after_commit` -> `dovetail_tool::reextend`), the same "one more
    // step" pattern a hole profile's re-orientation already uses.
    h.state_mut().set_active(tail);
    settle(&mut h);
    let history = history_len(&h);
    let before_tail = h.state().session.document().object(tail).unwrap().transform;
    let before_groove = h
        .state()
        .session
        .document()
        .object(groove)
        .unwrap()
        .transform;
    let turned = rotated_90_z(&before_tail);
    h.state_mut()
        .commit_transform(tail, turned, "test rotate".into());
    wait_until(&mut h, "the rotate", |a| a.busy.is_none());
    settle(&mut h);
    assert!(
        history_len(&h) == history + 1 || history_len(&h) == history + 2,
        "the move, and possibly extend-through-edge's own re-evaluation: {} -> {}",
        history,
        history_len(&h)
    );
    let after_tail = h.state().session.document().object(tail).unwrap().transform;
    let after_groove = h
        .state()
        .session
        .document()
        .object(groove)
        .unwrap()
        .transform;
    assert_ne!(after_tail, before_tail, "the tail turned");
    assert_ne!(
        after_groove, before_groove,
        "the groove turned with its group"
    );

    // The tail exits the piece it's attached to (extend-through-edge
    // re-evaluated after the rotate, `shape_panel::after_commit` ->
    // `dovetail_tool::reextend`): not "ends inside", the live warning a
    // stopped or short dovetail would get.
    assert_ne!(
        mf_desktop::dovetail_tool::ends_inside(h.state(), tail),
        Some(true),
        "the tail should exit the piece after rotating"
    );

    // The groove matches the tail within the fit's clearance: the exact
    // minimum distance between their world-placed meshes is positive
    // (never fused) and no more than the recorded clearance (the tail's
    // own recipe, `fit_clearance + machine_tolerance`, exactly what the
    // generator narrowed the tail by) plus a small margin for the
    // flank's own width-change with depth.
    let gap = world_min_distance(&h, tail, groove);
    let record = mf_desktop::shape_panel::recipe(h.state(), tail).unwrap();
    let clearance = record.params.get_float("fit_clearance").unwrap()
        + record.params.get_float("machine_tolerance").unwrap();
    assert!(
        gap >= 0.0 && gap <= clearance.max(0.0) + 0.5,
        "gap {gap} should be within the clearance {clearance}"
    );

    // Apply: Attach joins the tail to its piece and cuts the groove
    // from the other, one undo step, through the shape panel's own
    // "Attach to … and …" button (`shape_panel::attach_sides`).
    h.get_by_label("Select").click();
    settle(&mut h);
    h.state_mut().set_active(tail);
    settle(&mut h);
    let history = history_len(&h);
    h.get_by_label_contains("Attach to").click();
    wait_until(&mut h, "attach", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(history_len(&h), history + 1, "one undo step");

    // The tail and the groove objects are gone, folded into their
    // pieces (`object.attach`, same as every other connector group).
    assert!(
        h.state().session.document().object(tail).is_none(),
        "the tail joined its piece"
    );
    assert!(
        h.state().session.document().object(groove).is_none(),
        "the groove was cut from its piece"
    );
    let names = objects(&h);
    assert_eq!(names.len(), 2, "{names:?}");
}

/// Parts' "Combine with <target>" (owner 2026-11-12, replacing Merge):
/// a placed Box sunk into the ball it was dropped on is really unioned
/// into it — one object left, whose bounds take in the box, with more
/// faces than the ball alone — as one undo step; Undo restores both.
#[test]
fn parts_combine_with_target_unions_the_shapes_then_undo() {
    require_gpu!("parts_combine_with_target_unions_the_shapes_then_undo");
    let mut h = opened(vec![("scan", ball(10.0), at([0.0, 0.0, 10.0]))]);
    let scan = id_of(&h, "scan");
    let ball_faces = h
        .state()
        .session
        .document()
        .object(scan)
        .unwrap()
        .mesh()
        .unwrap()
        .face_count();
    let part = place_box(&mut h);
    // Sink it a few mm into the ball, as a person would in Transform, so
    // the union has a real overlap.
    let placed = history_len(&h);
    let mut t = h.state().session.document().object(part).unwrap().transform;
    t[11] -= 4.0;
    h.state_mut().commit_transform(part, t, "sink".into());
    wait_until(&mut h, "the sink-in", |a| {
        a.busy.is_none() && a.session.history_labels().len() > placed
    });
    settle(&mut h);
    let box_bounds = world_bounds(&h, part);
    let ball_bounds = world_bounds(&h, scan);
    assert!(
        box_bounds.max[2] > ball_bounds.max[2] + 1.0,
        "the box sticks out"
    );
    let before = objects(&h);
    assert_eq!(before.len(), 2, "{before:?}");

    h.get_by_label("Parts").click();
    settle(&mut h);
    scroll_and_click(&mut h, "Combine with scan");
    settle(&mut h);
    let history = history_len(&h);
    scroll_and_click(&mut h, "Join");
    wait_until(&mut h, "the combine", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(history_len(&h), history + 1, "one undo step");
    assert_eq!(
        objects(&h),
        vec!["scan".to_owned()],
        "the box is combined away"
    );
    let combined = world_bounds(&h, scan);
    assert!(
        (combined.max[2] - box_bounds.max[2]).abs() < 1e-6,
        "the union takes in the box: {combined:?} vs {box_bounds:?}"
    );
    let o = h.state().session.document().object(scan).unwrap();
    let mesh = o.mesh().unwrap().clone();
    let shift = DVec3::new(o.transform[9], o.transform[10], o.transform[11]);
    // The box's own top corner is now a corner of the one mesh: its
    // faces really joined, not just its bounds.
    let corner = DVec3::from(box_bounds.max);
    assert!(
        mesh.vertices()
            .any(|(_, p)| (DVec3::from(p) + shift - corner).length() < 1e-6),
        "the box's corner is part of the union"
    );
    assert_ne!(mesh.face_count(), ball_faces, "the ball's surface changed");
    assert_eq!(mesh.topology().boundary_edges, 0, "still closed");

    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(objects(&h), before, "Undo brings the box back");
    assert!((world_bounds(&h, scan).max[2] - ball_bounds.max[2]).abs() < 1e-9);
}

// Project organisation: rename, groups, history labels, checkpoints.
// ---------------------------------------------------------------------

/// Renaming an object, then splitting it, derives both pieces' names
/// from the *new* name: "scan" renamed to "widget" and split gives
/// "widget (top)" / "widget (bottom)", not "scan (top)" / "scan
/// (bottom)" (Task "Project organisation" step 1 and its test list).
#[test]
fn rename_then_split_gives_derived_names() {
    require_gpu!("rename_then_split_gives_derived_names");
    let mut h = opened(vec![("scan", ball(30.0), at([0.0, 0.0, 30.0]))]);
    let id = id_of(&h, "scan");
    let history = history_len(&h);
    h.state_mut().rename_object(id, "widget".into());
    wait_until(&mut h, "the rename", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    assert_eq!(objects(&h), ["widget"], "{:?}", objects(&h));

    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    let names = cut_with_keep(&mut h, "Both");
    assert_eq!(
        names,
        ["widget (top)", "widget (bottom)"],
        "derived from the renamed object: {names:?}"
    );
}

/// Double-clicking an object's Browser row frames it (M4 Task 8a), and
/// F2 renames the active object in place (Task "Project organisation"
/// step 1): a regression test for commit 14dc859, which briefly made an
/// object row's double-click rename it instead of framing it.
#[test]
fn double_click_frames_an_object_row_and_f2_renames_it() {
    require_gpu!("double_click_frames_an_object_row_and_f2_renames_it");
    let mut h = opened(vec![("scan", ball(30.0), at([0.0, 0.0, 30.0]))]);
    let id = id_of(&h, "scan");
    let before = h.state().camera.unwrap();

    // A single clean object leaves the right dock (the Browser) closed
    // by default.
    if h.query_by_label("Show Browser").is_some() {
        h.get_by_label("Show Browser").click();
        settle(&mut h);
    }

    let pos = row(&h, "scan").rect().center();
    click_at(&mut h, pos);
    click_at(&mut h, pos);
    let waiting = Instant::now();
    while h.state().framing_pending() {
        assert!(waiting.elapsed() < Duration::from_secs(30), "timed out");
        h.step();
    }
    let mut steps = 0;
    while h.state().camera_animating() {
        steps += 1;
        assert!(steps < 600, "the camera move never ended");
        h.step();
    }
    settle(&mut h);
    assert_ne!(
        h.state().camera.unwrap(),
        before,
        "double-click frames the object, it doesn't rename it"
    );
    assert!(h.state().renaming.is_none(), "no rename started");
    assert_eq!(
        h.state().session.document().object(id).unwrap().name,
        "scan",
        "unchanged by the double-click"
    );

    h.state_mut().active = Some(id);
    // Hide the Browser so only the selection panel's name field watches
    // `app.renaming` for this object (both would otherwise fight for
    // keyboard focus on the same text, as they did before this test
    // closed the Browser).
    if h.query_by_label("Hide Browser").is_some() {
        h.get_by_label("Hide Browser").click();
        settle(&mut h);
    }
    let history = history_len(&h);
    h.key_press(egui::Key::F2);
    settle(&mut h);
    assert_eq!(
        h.state().renaming,
        Some((id, "scan".to_owned())),
        "F2 starts an inline rename of the active object"
    );
    h.state_mut().rename_object(id, "widget".into());
    wait_until(&mut h, "the rename", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    assert_eq!(objects(&h), ["widget"], "F2's rename committed");
}

/// Grouping two objects (Cmd+G), then hiding and locking the group:
/// every member's hidden/locked flag follows, it's undoable, and a
/// locked object refuses to rename (Task "Project organisation" step 2
/// and its test list).
#[test]
fn group_hide_and_lock() {
    require_gpu!("group_hide_and_lock");
    let mut h = opened(vec![
        ("block", mf_fixtures::cube(20.0), at([0.0; 3])),
        ("post", mf_fixtures::cube(8.0), at([40.0, 0.0, 0.0])),
    ]);
    let (block, post) = (id_of(&h, "block"), id_of(&h, "post"));
    select_together(&mut h, &["block", "post"]);
    assert_eq!(
        h.state().picked.len(),
        2,
        "both selected together: {:?}",
        h.state().picked
    );

    h.state_mut().group_selection();
    settle(&mut h);
    let org = h.state().session.document().organisation().clone();
    assert_eq!(org.root_order().len(), 1, "one group at the root");
    let mf_command::Entry::Group(gid) = org.root_order()[0] else {
        panic!("expected a group at the root: {:?}", org.root_order())
    };
    let mut members = org.members(gid);
    members.sort_by_key(|id| id.0);
    let mut expected = [block, post];
    expected.sort_by_key(|id| id.0);
    assert_eq!(members, expected, "the group holds both objects");

    // Hiding the group hides every member (document state, undoable).
    assert!(!h.state().is_hidden(block) && !h.state().is_hidden(post));
    let history = history_len(&h);
    h.state_mut().set_hidden(&[block, post], true);
    settle(&mut h);
    assert!(h.state().is_hidden(block) && h.state().is_hidden(post));
    assert_eq!(history_len(&h), history + 1, "hide is one undo step");
    assert!(
        !h.state()
            .object_views()
            .iter()
            .any(|v| v.id == block || v.id == post),
        "hidden objects don't render"
    );
    h.state_mut().undo();
    settle(&mut h);
    assert!(
        !h.state().is_hidden(block) && !h.state().is_hidden(post),
        "undo restores visibility"
    );

    // Locking blocks rename and delete.
    h.state_mut().set_locked(&[block], true);
    settle(&mut h);
    assert!(h.state().is_locked(block));
    let before_name = h
        .state()
        .session
        .document()
        .object(block)
        .unwrap()
        .name
        .clone();
    h.state_mut().rename_object(block, "renamed".into());
    settle(&mut h);
    assert_eq!(
        h.state().session.document().object(block).unwrap().name,
        before_name,
        "a locked object refuses to rename"
    );
    let objects_before = h.state().session.document().objects().len();
    h.state_mut().delete_object(block);
    settle(&mut h);
    assert_eq!(
        h.state().session.document().objects().len(),
        objects_before,
        "a locked object refuses to delete"
    );
}

/// Every History row's plain-language label names the object, never a
/// raw matrix, op id or parameter list: audited across split, combine,
/// transform and rename (Task "Project organisation" step 3's test
/// list; `history_labels.rs` covers make_solid and place_connectors
/// too).
#[test]
fn history_labels_contain_no_raw_parameters() {
    require_gpu!("history_labels_contain_no_raw_parameters");
    let mut h = opened(vec![("scan", ball(30.0), at([0.0, 0.0, 30.0]))]);
    let id = id_of(&h, "scan");

    h.state_mut().rename_object(id, "widget".into());
    wait_until(&mut h, "the rename", |a| a.busy.is_none());
    settle(&mut h);

    h.state_mut()
        .commit_transform(id, at([12.0, 0.0, 30.0]), "moved".into());
    wait_until(&mut h, "the move", |a| a.busy.is_none());
    settle(&mut h);

    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    cut_with_keep(&mut h, "Both");

    let never_raw = |label: &str| {
        assert!(!label.contains("object.rename"), "{label:?}");
        assert!(!label.contains("object.set_transform"), "{label:?}");
        assert!(!label.contains("mesh.split_plane"), "{label:?}");
        assert!(!label.contains("transform:"), "{label:?}");
        assert!(
            !label.contains('['),
            "a raw matrix would show as [...]: {label:?}"
        );
        // A raw f64 array prints with a decimal point; "12 mm" and
        // "2 pieces" are fine, but "1.0, 0.0, 0.0, 0.0, 1.0" (the
        // identity transform's own text) must never appear.
        assert!(
            !label.contains("1, 0, 0, 0, 1, 0, 0, 0, 1"),
            "a raw transform's numbers leaked into the label: {label:?}"
        );
    };
    let labels: Vec<String> = h
        .state()
        .breadcrumb()
        .into_iter()
        .map(|(_, label, _, _)| label)
        .collect();
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("Renamed scan to widget")),
        "{labels:?}"
    );
    assert!(
        labels.iter().any(|l| l.starts_with("Moved widget by")),
        "{labels:?}"
    );
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with("Split widget into 2 piece")),
        "{labels:?}"
    );
    for label in &labels {
        never_raw(label);
    }
}

/// A named checkpoint shows as "[name]" in the History panel and
/// clicking it checks out that revision, same as any other row (Task
/// "Project organisation" step 3's test list).
#[test]
fn a_named_checkpoint_can_be_checked_out() {
    require_gpu!("a_named_checkpoint_can_be_checked_out");
    let mut h = opened(vec![("scan", ball(30.0), at([0.0, 0.0, 30.0]))]);
    let id = id_of(&h, "scan");

    h.state_mut().rename_object(id, "before".into());
    wait_until(&mut h, "the first rename", |a| a.busy.is_none());
    settle(&mut h);
    let checkpoint = h.state().session.current();

    h.state_mut().rename_object(id, "after".into());
    wait_until(&mut h, "the second rename", |a| a.busy.is_none());
    settle(&mut h);
    assert_eq!(
        h.state().session.document().object(id).unwrap().name,
        "after"
    );

    h.state_mut()
        .name_checkpoint(checkpoint, "Before the second rename".into());
    settle(&mut h);
    assert_eq!(
        h.state().session.checkpoint_name(checkpoint),
        Some("Before the second rename")
    );

    // A single clean object leaves the right dock closed by default:
    // open it, then switch to the History tab.
    if h.query_by_label("Show Browser").is_some() {
        h.get_by_label("Show Browser").click();
        settle(&mut h);
    }
    h.state_mut().right_tab = "history";
    settle(&mut h);
    let row = h.get_by_label("[Before the second rename]");
    row.click();
    settle(&mut h);
    assert_eq!(h.state().session.current(), checkpoint, "checked out");
    assert_eq!(
        h.state().session.document().object(id).unwrap().name,
        "before",
        "the document is back at the named point"
    );
}

// ---------------------------------------------------------------------
// Align: min/centre/max per axis, an anchor that stays fixed,
// distributing with equal gaps, and Undo.
// ---------------------------------------------------------------------

/// With `MF_ARTIFACTS` set, saves a frame to
/// `<dir>/design/align-<scene>-<w>x<h>.png`.
fn capture(h: &mut Harness<'_, MeshFixerApp>, scene: &str) {
    let Some(dir) = std::env::var_os("MF_ARTIFACTS") else {
        return;
    };
    let dir = PathBuf::from(dir).join("design");
    std::fs::create_dir_all(&dir).unwrap();
    let size = h.ctx.content_rect().size();
    let name = format!("align-{scene}-{}x{}.png", size.x as u32, size.y as u32);
    h.render().unwrap().save(dir.join(name)).unwrap();
}

/// Opens Align (L) with `names` selected together, waiting for its 9
/// dot handles to be drawn.
fn open_align(h: &mut Harness<'_, MeshFixerApp>, names: &[&str]) {
    select_together(h, names);
    h.key_press(egui::Key::L);
    wait_until(h, "align's dots", |a| {
        a.active_tool == mf_desktop::align_tool::ID && !mf_desktop::align_tool::dots(a).is_empty()
    });
    settle(h);
}

/// `dot`'s window position, as the renderer places it (the same
/// projection [`mf_desktop::align_tool::view_overlay`] draws with).
fn dot_point(h: &Harness<'_, MeshFixerApp>, dot: mf_desktop::align_tool::Dot) -> egui::Pos2 {
    let app = h.state();
    let cam = app.camera.unwrap();
    let rect = app.viewport.last_rect.unwrap();
    let view = (rect.width() as f64, rect.height() as f64);
    let (x, y) = mf_render::screen_position(&cam, cam.target, view, dot.world.to_array()).unwrap();
    rect.left_top() + egui::vec2(x as f32, y as f32)
}

/// The dot for `axis`/`mode` among Align's current 9.
fn find_dot(
    h: &Harness<'_, MeshFixerApp>,
    axis: usize,
    mode: mf_desktop::align_tool::Mode,
) -> mf_desktop::align_tool::Dot {
    mf_desktop::align_tool::dots(h.state())
        .into_iter()
        .find(|d| d.axis == axis && d.mode == mode)
        .unwrap_or_else(|| panic!("no dot for axis {axis} mode {mode:?}"))
}

/// Clicks the dot for `axis`/`mode` and waits for the command to commit.
fn click_dot(h: &mut Harness<'_, MeshFixerApp>, axis: usize, mode: mf_desktop::align_tool::Mode) {
    let history = history_len(h);
    let p = dot_point(h, find_dot(h, axis, mode));
    click_at(h, p);
    wait_until(h, "the align commits", |a| {
        a.busy.is_none() && a.session.history_labels().len() == history + 1
    });
    settle(h);
}

/// Three balls (10 mm diameter) placed apart on one axis.
fn three_balls_on(axis: usize) -> Vec<(&'static str, Mesh, [f64; 12])> {
    let p = |v: f64| {
        let mut t = [0.0; 3];
        t[axis] = v;
        t
    };
    vec![
        ("a", ball(5.0), at(p(0.0))),
        ("b", ball(5.0), at(p(30.0))),
        ("c", ball(5.0), at(p(100.0))),
    ]
}

/// Aligning to the min, centre and max edges on each axis: every
/// selected object's bounding box lands exactly on the overall bounds'
/// edge or centre on that axis, and the other two axes don't move.
#[test]
fn align_min_centre_max_on_each_axis() {
    use mf_desktop::align_tool::Mode;
    require_gpu!("align_min_centre_max_on_each_axis");
    for axis in 0..3 {
        for (mode, scene) in [
            (Mode::Min, "min"),
            (Mode::Centre, "centre"),
            (Mode::Max, "max"),
        ] {
            let mut h = opened(three_balls_on(axis));
            let (ida, idb, idc) = (id_of(&h, "a"), id_of(&h, "b"), id_of(&h, "c"));
            open_align(&mut h, &["a", "b", "c"]);
            capture(&mut h, &format!("{scene}-axis{axis}-before"));

            click_dot(&mut h, axis, mode);
            capture(&mut h, &format!("{scene}-axis{axis}-after"));

            let (ba, bb, bc) = (
                world_bounds(&h, ida),
                world_bounds(&h, idb),
                world_bounds(&h, idc),
            );
            let value = |b: &Aabb| match mode {
                Mode::Min => b.min[axis],
                Mode::Max => b.max[axis],
                _ => (b.min[axis] + b.max[axis]) / 2.0,
            };
            let (va, vb, vc) = (value(&ba), value(&bb), value(&bc));
            assert!(
                (va - vb).abs() < 1e-6 && (vb - vc).abs() < 1e-6,
                "axis {axis} {scene}: all three should share the same {scene}: {va} {vb} {vc}"
            );
            // The other two axes are untouched.
            for other in (0..3).filter(|&o| o != axis) {
                assert_eq!(
                    ba.min[other],
                    0.0 - 5.0,
                    "{scene}: axis {other} untouched on a"
                );
                assert_eq!(
                    bb.min[other],
                    0.0 - 5.0,
                    "{scene}: axis {other} untouched on b"
                );
                assert_eq!(
                    bc.min[other],
                    0.0 - 5.0,
                    "{scene}: axis {other} untouched on c"
                );
            }
        }
    }
}

/// Clicking a selected object in the view sets it as the anchor: its
/// own bounding box never moves, and the others align to it.
#[test]
fn align_anchor_stays_fixed() {
    require_gpu!("align_anchor_stays_fixed");
    let mut h = opened(three_balls_on(0));
    let (ida, idb, idc) = (id_of(&h, "a"), id_of(&h, "b"), id_of(&h, "c"));
    open_align(&mut h, &["a", "b", "c"]);
    wait_until(&mut h, "picking is ready", |a| a.picking_ready());
    settle(&mut h);

    // Click on c's own surface (its centre, which is on its silhouette
    // from this camera) to anchor it. Retried a few times (each a fresh
    // press-and-release): the anchor pick is a synchronous ray cast
    // against the object's own BVH (`align_tool::pick_selected_object`,
    // the same technique Split's "click a side" uses), so
    // `picking_ready()` being true is as settled as it gets, but a
    // single press can still land a frame before the BVH it reads is
    // swapped in on a freshly warmed-up test binary.
    let anchor_set =
        |a: &MeshFixerApp| a.tool_state::<mf_desktop::align_tool::AlignState>().anchor == Some(idc);
    for _ in 0..5 {
        if anchor_set(h.state()) {
            break;
        }
        let p = {
            let app = h.state();
            let cam = app.camera.unwrap();
            let rect = app.viewport.last_rect.unwrap();
            let view = (rect.width() as f64, rect.height() as f64);
            let world = world_bounds(&h, idc).center();
            let (x, y) = mf_render::screen_position(&cam, cam.target, view, world).unwrap();
            rect.left_top() + egui::vec2(x as f32, y as f32)
        };
        click_at(&mut h, p);
        settle(&mut h);
    }
    wait_until(&mut h, "c becomes the anchor", |a| {
        a.tool_state::<mf_desktop::align_tool::AlignState>().anchor == Some(idc)
    });
    settle(&mut h);
    capture(&mut h, "anchor-set");

    let before_c = world_bounds(&h, idc);
    click_dot(&mut h, 0, mf_desktop::align_tool::Mode::Centre);
    capture(&mut h, "anchor-after");

    let after_c = world_bounds(&h, idc);
    assert_eq!(before_c, after_c, "the anchor itself never moves");
    let (ba, bb) = (world_bounds(&h, ida), world_bounds(&h, idb));
    let centre = |b: &Aabb| (b.min[0] + b.max[0]) / 2.0;
    let anchor_centre = centre(&after_c);
    assert!(
        (centre(&ba) - anchor_centre).abs() < 1e-6,
        "a centres on the anchor"
    );
    assert!(
        (centre(&bb) - anchor_centre).abs() < 1e-6,
        "b centres on the anchor"
    );
}

/// Distribute, equal gaps on X: the two outermost objects (by position)
/// stay put, and the middle one lands with equal gaps on either side.
#[test]
fn align_distribute_equal_gaps() {
    require_gpu!("align_distribute_equal_gaps");
    // a and c at the extremes; b not yet evenly spaced between them.
    let mut h = opened(vec![
        ("a", ball(5.0), at([0.0, 0.0, 0.0])),
        ("b", ball(5.0), at([17.0, 0.0, 0.0])),
        ("c", ball(5.0), at([100.0, 0.0, 0.0])),
    ]);
    let (ida, idb, idc) = (id_of(&h, "a"), id_of(&h, "b"), id_of(&h, "c"));
    open_align(&mut h, &["a", "b", "c"]);
    let (before_a, before_c) = (world_bounds(&h, ida), world_bounds(&h, idc));

    let history = history_len(&h);
    h.get_by_label("Distribute X: Equal gaps").click();
    wait_until(&mut h, "distribute commits", |a| {
        a.busy.is_none() && a.session.history_labels().len() == history + 1
    });
    settle(&mut h);
    capture(&mut h, "distribute-gaps");

    assert_eq!(
        world_bounds(&h, ida),
        before_a,
        "a (an extreme) doesn't move"
    );
    assert_eq!(
        world_bounds(&h, idc),
        before_c,
        "c (an extreme) doesn't move"
    );
    let bb = world_bounds(&h, idb);
    // a: [-5,5], c: [95,105]; equal gaps split (95-5-10)/2 = 40 each
    // side, so b's box should be [45, 55].
    assert!((bb.min[0] - 45.0).abs() < 1e-6, "{bb:?}");
    assert!((bb.max[0] - 55.0).abs() < 1e-6, "{bb:?}");
}

/// Aligning is one undo step; Undo puts every moved object back exactly.
#[test]
fn align_then_undo_restores_every_object() {
    require_gpu!("align_then_undo_restores_every_object");
    let mut h = opened(three_balls_on(0));
    let (ida, idb, idc) = (id_of(&h, "a"), id_of(&h, "b"), id_of(&h, "c"));
    open_align(&mut h, &["a", "b", "c"]);

    let before = (
        h.state().session.document().object(ida).unwrap().transform,
        h.state().session.document().object(idb).unwrap().transform,
        h.state().session.document().object(idc).unwrap().transform,
    );
    let revision = h.state().session.current();

    click_dot(&mut h, 0, mf_desktop::align_tool::Mode::Centre);
    assert_ne!(
        h.state().session.document().object(idb).unwrap().transform,
        before.1,
        "the align actually moved something"
    );
    capture(&mut h, "undo-before-undo");

    h.get_by_label("Undo").click();
    wait_until(&mut h, "the undo", |a| {
        a.busy.is_none() && a.session.current() == revision
    });
    settle(&mut h);
    capture(&mut h, "undo-after-undo");

    assert_eq!(
        h.state().session.document().object(ida).unwrap().transform,
        before.0
    );
    assert_eq!(
        h.state().session.document().object(idb).unwrap().transform,
        before.1
    );
    assert_eq!(
        h.state().session.document().object(idc).unwrap().transform,
        before.2,
        "one undo restores every moved object exactly"
    );
}

/// Fix round 2026-11-13 (owner: the 9 dots "float with nothing tying
/// them to an axis" — Tinkercad ties each axis's dots to a rail on the
/// box's own edge). Every dot [`mf_desktop::align_tool::dots`] returns
/// must lie exactly on its own axis's rail segment
/// ([`mf_desktop::align_tool::rails`]): on the rail's line (the two
/// cross-axis coordinates match the rail's own, not just "somewhere near
/// it") and between its two ends (`Mode::Min` at one end, `Mode::Max` at
/// the other, `Mode::Centre` at the midpoint) — draw and hit share the
/// same `dots()`, so this also covers hit-test parity. Checked from
/// three different camera orbits (the default, and two more that each
/// put one of the brief's three default rail edges on the box's hidden
/// side) so the "pick the edge that faces the camera instead" fallback
/// is covered too, not just the common case. Captures a frame per orbit
/// with `MF_ARTIFACTS` set (milestone evidence: the rails and dots
/// actually visible, not just correct in world space).
#[test]
fn align_dots_sit_on_their_axis_rail() {
    use mf_desktop::align_tool::Mode;
    require_gpu!("align_dots_sit_on_their_axis_rail");
    // Three balls spread on every axis, off-centre so the box's min,
    // centre and max never coincide on any axis.
    let mut h = opened(vec![
        ("a", ball(5.0), at([0.0, 0.0, 0.0])),
        ("b", ball(5.0), at([40.0, 25.0, 0.0])),
        ("c", ball(5.0), at([20.0, 10.0, 30.0])),
    ]);
    open_align(&mut h, &["a", "b", "c"]);

    // (yaw, pitch) in degrees: the tool's own default orbit, then two
    // that each flip a different pair of the brief's default rail
    // corners (both their adjacent faces back-facing at once) to
    // exercise `rail_corner`'s "hidden" fallback.
    let orbits: [(&str, f64, f64); 3] = [
        // The tool's own default orbit: flips Y's own bottom-left corner
        // (X and Z are both back-facing from here).
        ("default", -60.0, 25.0),
        // Flips X's bottom-front corner's Y side only (Y and Z stay put).
        ("from-behind", 150.0, 10.0),
        // Flips Z's front-left corner only (X and Y stay put): looking
        // down from a steep angle in the +X+Y quadrant.
        ("from-above-right", 45.0, -20.0),
    ];
    for (scene, yaw_deg, pitch_deg) in orbits {
        {
            let app = h.state_mut();
            let mut cam = app.camera.unwrap();
            cam.yaw = yaw_deg.to_radians();
            cam.pitch = pitch_deg.to_radians();
            app.camera = Some(cam);
        }
        settle(&mut h);
        capture(&mut h, &format!("rails-{scene}"));

        let the_rails = mf_desktop::align_tool::rails(h.state())
            .unwrap_or_else(|| panic!("no rails at orbit {scene:?}"));
        let the_dots = mf_desktop::align_tool::dots(h.state());
        assert_eq!(the_dots.len(), 9, "9 dots at orbit {scene:?}");

        for dot in &the_dots {
            let rail = the_rails[dot.axis];
            assert_eq!(
                rail.axis, dot.axis,
                "rail/dot axis mismatch at orbit {scene:?}"
            );
            let (low, high) = rail.ends;
            // On the rail's own line: the two cross-axis coordinates
            // (every component except `dot.axis`) match the rail's
            // exactly (the rail's own two ends share them).
            for other in (0..3).filter(|&a| a != dot.axis) {
                assert!(
                    (dot.world[other] - low[other]).abs() < 1e-9,
                    "{scene:?} axis {} {:?}: dot {:?} isn't on its rail's cross-axis {other} \
                     (rail {low:?}..{high:?})",
                    dot.axis,
                    dot.mode,
                    dot.world
                );
            }
            // Between the rail's two ends on its own axis, at the end
            // (or midpoint) the dot's mode names.
            let (rail_lo, rail_hi) = (
                low[dot.axis].min(high[dot.axis]),
                low[dot.axis].max(high[dot.axis]),
            );
            assert!(
                dot.world[dot.axis] >= rail_lo - 1e-9 && dot.world[dot.axis] <= rail_hi + 1e-9,
                "{scene:?} axis {} {:?}: dot at {} isn't within the rail's segment [{rail_lo}, {rail_hi}]",
                dot.axis,
                dot.mode,
                dot.world[dot.axis]
            );
            let expect = match dot.mode {
                Mode::Min => low[dot.axis],
                Mode::Max => high[dot.axis],
                Mode::Centre => (low[dot.axis] + high[dot.axis]) * 0.5,
                Mode::DistributeGaps | Mode::DistributeCentres => continue,
            };
            assert!(
                (dot.world[dot.axis] - expect).abs() < 1e-9,
                "{scene:?} axis {} {:?}: dot at {} should be at {expect} on the rail",
                dot.axis,
                dot.mode,
                dot.world[dot.axis]
            );
        }

        // Draw/hit parity: every dot `dots()` reports is also where
        // `view_overlay` would actually paint it (the same projection
        // `dot_point` uses, which is `view_overlay`'s own) — i.e. hit
        // testing and drawing read the same geometry, not two copies
        // that could drift apart.
        for dot in &the_dots {
            let p = dot_point(&h, *dot);
            assert!(
                h.state().viewport.last_rect.unwrap().contains(p),
                "{scene:?} axis {} {:?}: dot projects outside the viewport ({p:?})",
                dot.axis,
                dot.mode
            );
        }
    }
}

// ---------------------------------------------------------------------
// UC3 "The hinged box" (Phase 1 Task 30): Hollow a box, Split it into a
// lid and a body with a gap, place two hinges and a clasp from the
// Connectors library, resize a hinge with its Size field, Merge each,
// then a view-only swing of the lid.
// ---------------------------------------------------------------------

/// With `MF_ARTIFACTS` set, saves a frame to
/// `<dir>/uc3/<step>-<w>x<h>.png`.
fn uc3_capture(h: &mut Harness<'_, MeshFixerApp>, step: &str) {
    let Some(dir) = std::env::var_os("MF_ARTIFACTS") else {
        return;
    };
    let dir = PathBuf::from(dir).join("uc3");
    std::fs::create_dir_all(&dir).unwrap();
    let size = h.ctx.content_rect().size();
    let name = format!("{step}-{}x{}.png", size.x as u32, size.y as u32);
    h.render().unwrap().save(dir.join(name)).unwrap();
}

/// A closed box, outer corner at the origin, size `[sx, sy, sz]` mm
/// (`mf_fixtures::cube`, non-uniformly scaled).
fn hinge_box(sx: f64, sy: f64, sz: f64) -> Mesh {
    let (p, t) = mf_fixtures::cube(1.0).to_arrays();
    let p = p
        .into_iter()
        .map(|[x, y, z]| [x * sx, y * sy, z * sz])
        .collect();
    Mesh::new(p, t).unwrap()
}

/// Clicks the Connectors tool's "Attach to A and B" button on the
/// selection panel (`shape_panel::editor`'s own button, shown once a
/// linked group's members know which two objects they join) and waits
/// for the merge to commit, one undo step.
fn attach_selected_group(h: &mut Harness<'_, MeshFixerApp>, label_contains: &str) {
    let history = history_len(h);
    h.get_by_label_contains(label_contains).click();
    wait_until(h, "the attach", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(history_len(h), history + 1, "one undo step");
}

/// The hinged box, end to end through the real UI: a hollow box with
/// thick walls, split into a lid and a body with a gap, two hinges and
/// a clasp placed from the Connectors library and snapped to the seam,
/// one hinge resized with its Size field, each connector merged, and a
/// view-only swing of the lid.
#[test]
fn uc3_hinged_box_through_the_gui() {
    require_gpu!("uc3_hinged_box_through_the_gui");
    let mut h = opened(vec![(
        "box",
        hinge_box(80.0, 60.0, 40.0),
        at([0.0, 0.0, 0.0]),
    )]);
    uc3_capture(&mut h, "0-opened");

    // Step 1: Hollow, 3 mm walls (a coarse cell so the GUI test stays
    // fast; matches the command-layer scenario's own `inner_detail`).
    h.get_by_label("Hollow").click();
    settle(&mut h);
    let box_id = id_of(&h, "box");
    h.state_mut().hollow.wall = Some(mf_units::Length::new(3.0, Unit::Millimeter));
    h.state_mut().hollow.detail = Some(mf_units::Length::new(1.0, Unit::Millimeter));
    settle(&mut h);
    wait_until(&mut h, "the hollow preview", |a| {
        mf_desktop::hollow_tool::preview_render(a, box_id).is_some()
    });
    settle(&mut h);
    uc3_capture(&mut h, "1-hollow-preview");
    let history = history_len(&h);
    panel_button(&h, "Hollow").click();
    wait_until(&mut h, "the hollow", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(history_len(&h), history + 1, "one undo step");
    uc3_capture(&mut h, "2-hollowed");

    // Step 2: Split 8 mm below the top (z = 32), with a 0.4 mm gap at
    // the cut ("Print-in-place lid": the cut's own gap, non-zero, so
    // the two halves separate cleanly) — "box (top)" is the lid, "box
    // (bottom)" the body.
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] = 32.0;
    h.state_mut().split.gap = mf_units::Length::new(0.4, Unit::Millimeter);
    settle(&mut h);
    let history = history_len(&h);
    let names = cut_with_keep(&mut h, "Both");
    assert_eq!(names, ["box (top)", "box (bottom)"], "{names:?}");
    assert_eq!(history_len(&h), history + 1, "one undo step");
    uc3_capture(&mut h, "3-split");
    let (lid_id, body_id) = (id_of(&h, "box (top)"), id_of(&h, "box (bottom)"));

    // Step 3: two hinges on the back wall (y = 60), from the
    // Connectors tool, placed 25 mm either side of the shared edge's
    // own middle (the 80 mm wall's middle sits at x = 40, so this
    // lands them 15 mm from each end, same as the brief): the
    // "Position along the edge" slider this task adds (there was no
    // way to place a second hinge clear of the first before).
    h.get_by_label("Connectors").click();
    settle(&mut h);
    h.state_mut()
        .tool_state_mut::<mf_desktop::connector_tool::ConnectorsState>()
        .fit_class = "Free running".into(); // Snug would print it fused.
    h.state_mut().hinge.a = Some(body_id);
    h.state_mut().hinge.b = Some(lid_id);
    settle(&mut h);
    uc3_capture(&mut h, "4-hinge-section");

    let place_hinge_at = |h: &mut Harness<'_, MeshFixerApp>, position: f64, step: &str| {
        h.state_mut().hinge.position = Some(position);
        settle(h);
        let history = history_len(h);
        scroll_and_click(h, "Place hinge");
        wait_until(h, "the hinge placed", |a| {
            a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
        });
        settle(h);
        assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
        assert_eq!(history_len(h), history + 1, "one undo step");
        uc3_capture(h, step);
    };
    place_hinge_at(&mut h, -25.0, "5-hinge-1-placed");
    let hinge1_leaf_a = id_of(&h, "Pin hinge (leaf a)");
    place_hinge_at(&mut h, 25.0, "6-hinge-2-placed");
    // The second hinge's own leaf a: the first's is found by name
    // before the second is placed (both pieces share the same name,
    // "Pin hinge (leaf a)" — `connector_tool::new_group_id` numbers the
    // *groups*, not each piece's own name) — so collect every leaf a
    // by group instead, once both are down.
    let leaf_as: Vec<mf_command::ObjectId> = h
        .state()
        .session
        .document()
        .objects()
        .iter()
        .filter(|o| o.name == "Pin hinge (leaf a)")
        .map(|o| o.id)
        .collect();
    assert_eq!(leaf_as.len(), 2, "two separate hinges: {leaf_as:?}");
    assert!(leaf_as.contains(&hinge1_leaf_a));
    let hinge2_leaf_a = *leaf_as.iter().find(|&&id| id != hinge1_leaf_a).unwrap();

    // The two hinges don't overlap (placed at x = 15 and x = 65 on the
    // 80 mm wall, each up to 24 mm long: well clear of each other).
    assert!(
        !mf_desktop::shape_panel::members(h.state(), hinge1_leaf_a)
            .iter()
            .any(|(m, _)| *m == hinge2_leaf_a),
        "the two hinges are separate groups"
    );

    // Step 3b: one clasp on the front wall (y = 0), from the library,
    // then dragged into place with Transform — the same "drop, then
    // place exactly" flow Connect's own dovetail/clasp tests use
    // (`connectors.rs::merge_folds_the_connector_into_its_two_sides`):
    // the library always drops a connector on the active object's own
    // highest point, never where a second connector on a different
    // wall actually belongs, so it's moved after. Its own frame here
    // mirrors the command-layer scenario's `clasp_frame` exactly (a
    // narrower width than the spec default so its footprint fits the
    // hollow box's 3 mm wall rim, and its height chosen so the catch
    // stays on the body's own side of the cut gap and the hook on the
    // lid's).
    h.state_mut().set_active(body_id);
    settle(&mut h);
    h.get_by_label("Connectors").click();
    settle(&mut h);
    let history = history_len(&h);
    scroll_and_click(&mut h, "Snap clasp");
    wait_until(&mut h, "the clasp placed", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let clasp_id = h.state().active.unwrap();
    // Narrow its width to fit the hollow box's 3 mm wall rim (the
    // spec's own default, 12 mm, is sized for a thick block, not a
    // thin shell): the selection panel's own "Width" field,
    // `shape_panel::set_param`.
    let clasp_width = 2.4_f64;
    h.get_by_label("Width");
    let history = history_len(&h);
    mf_desktop::shape_panel::set_param(
        h.state_mut(),
        "width",
        mf_spec::ParamValue::Float(clasp_width),
    );
    wait_until(&mut h, "the width change", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    let arm_length = 10.0_f64; // clasp.snap's own spec default.
    let ridge_height = 2.0 * 1.6_f64.max(0.3); // 2 * max(arm_thickness, lip_depth).
    let clasp_frame: [f64; 12] = [
        1.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
        0.0,
        0.0,
        1.0,
        40.0 - arm_length / 2.0,
        0.0,
        32.0 - 0.2 - ridge_height,
    ];
    h.state_mut()
        .commit_transform(clasp_id, clasp_frame, "test place clasp".into());
    wait_until(&mut h, "the clasp's move", |a| a.busy.is_none());
    settle(&mut h);
    uc3_capture(&mut h, "7-clasp-placed");

    // Step 4: resize hinge 1 to 24 mm with its Size field (the
    // selection panel's schema-driven "Length" field,
    // `shape_panel::set_param` — the same code path typing into the
    // field itself would commit through).
    h.get_by_label("Select").click();
    settle(&mut h);
    h.state_mut().set_active(hinge1_leaf_a);
    settle(&mut h);
    h.get_by_label("Length");
    let history = history_len(&h);
    mf_desktop::shape_panel::set_param(h.state_mut(), "length", mf_spec::ParamValue::Float(24.0));
    wait_until(&mut h, "the resize", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(history_len(&h), history + 1, "one undo step");
    let resized_length = h
        .state()
        .session
        .document()
        .object(hinge1_leaf_a)
        .unwrap()
        .mesh()
        .unwrap()
        .bounding_box()
        .unwrap()
        .max[0];
    assert!((resized_length - 24.0).abs() < 1e-6, "{resized_length}");
    uc3_capture(&mut h, "8-hinge-1-resized");

    // A view-only rotation of the lid (the swing preview), before any
    // merge: `hinge_tool::swing_setup` reads the group through its own
    // live member objects (`attach_to`), which the merge below removes
    // for the hinges' leaves — so the preview is checked here, while
    // the connector is still its own linked group, the same point
    // `connectors.rs::a_hinge_between_two_objects_swings_and_attaches`
    // checks it, just before its own Attach.
    h.state_mut().set_active(hinge1_leaf_a);
    settle(&mut h);
    wait_until(&mut h, "the swing check", |a| {
        mf_desktop::hinge_tool::swing_report(a).is_some()
    });
    settle(&mut h);
    if let Some(s) = h.state_mut().hinge.swing.as_mut() {
        s.angle = 90.0;
    }
    settle(&mut h);
    let lid_shown = h.state().shown_transform(lid_id).expect("the lid turns");
    uc3_capture(&mut h, "8b-swing-90");
    let report = mf_desktop::hinge_tool::swing_report(h.state())
        .unwrap()
        .unwrap();
    assert_eq!(
        report.first_contact_deg,
        None,
        "the lid swings clear on hinge 1's own axis: {:?}",
        report.contacts.first()
    );
    let _ = lid_shown;
    if let Some(s) = h.state_mut().hinge.swing.as_mut() {
        s.angle = 0.0;
    }
    settle(&mut h);

    // Step 5: Merge each connector.
    //
    // The hinges know their own two sides already (picked in the
    // Connectors panel's A/B pickers before placing, `hinge_tool`'s own
    // flow), so their merge is the selection panel's "Attach to A and
    // B" button, which only shows while the Select tool is active
    // (`tool_panels::inspector_card`).
    h.get_by_label("Select").click();
    settle(&mut h);
    for leaf_a in [hinge1_leaf_a, hinge2_leaf_a] {
        h.state_mut().set_active(leaf_a);
        settle(&mut h);
        attach_selected_group(&mut h, "Attach to");
    }
    // The clasp, dropped from the library, doesn't know its sides yet
    // (`place_connector`'s own members carry no `attach_to`): picked
    // here instead, the Connectors panel's own "Side A"/"Side B"/
    // "Attach" flow (`connectors.rs::merge_folds_the_connector_into_
    // its_two_sides`'s own route) — the still-live `placed` state from
    // when the clasp was placed, several steps and tool switches ago.
    h.get_by_label("Connectors").click();
    settle(&mut h);
    scroll_and_click(&mut h, "Side A: box (bottom)");
    settle(&mut h);
    scroll_and_click(&mut h, "Side B: box (top)");
    settle(&mut h);
    let history = history_len(&h);
    scroll_and_click(&mut h, "Attach");
    wait_until(&mut h, "the clasp's merge", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(history_len(&h), history + 1, "one undo step");
    uc3_capture(&mut h, "9-merged");

    // Only body, lid and the two hinges' captive pins remain.
    let names = objects(&h);
    let pins = names.iter().filter(|n| n.contains("pin")).count();
    assert_eq!(pins, 2, "{names:?}");
    assert_eq!(names.len(), 2 + pins, "{names:?}");
    assert!(names.contains(&"box (top)".to_owned()));
    assert!(names.contains(&"box (bottom)".to_owned()));

    // Assert: two closed, manifold objects that never touch.
    let body_mesh = h
        .state()
        .session
        .document()
        .object(body_id)
        .unwrap()
        .mesh()
        .unwrap()
        .clone();
    let lid_mesh = h
        .state()
        .session
        .document()
        .object(lid_id)
        .unwrap()
        .mesh()
        .unwrap()
        .clone();
    for (mesh, label) in [(&body_mesh, "body"), (&lid_mesh, "lid")] {
        let d = mf_geometry::inspect(mesh);
        assert!(d.is_closed && d.is_manifold, "{label}: {d:?}");
    }
    let ctx = mf_runtime::RunContext::none();
    let bvh = mf_geometry::Bvh::build(&body_mesh, &ctx).unwrap();
    let gap =
        mf_geometry::minimum_distance(&body_mesh, &bvh, &lid_mesh, &glam::DMat4::IDENTITY, &ctx)
            .unwrap()
            .unwrap();
    // The binding clearance is "Free running"'s own (0.3 mm), tighter
    // than the cut's own gap (0.4 mm): the hinges and the clasp were
    // all merged with that as their `gap` check.
    assert!(
        gap >= 0.3 - 1e-6,
        "body and lid stay apart by the gap: {gap}"
    );
}

/// The import flow end to end (MF-ISSUE-045, owner 2026-11-14 "smarter
/// import units"): opening a unitless STL asks for its unit, defaulting
/// to whichever "fits the plate" best — here, with no Machine profile
/// configured, the plain 250 mm default plate. An 8×6×4-unit box reads
/// as 8×6×4 mm (fits, tiny), 80×60×40 mm (fits, cm), 8000 mm (too big,
/// metres) or 203×152×102 mm (fits, inches): inches gives the largest
/// reading that still fits, so it's preselected over millimetres and
/// centimetres.
#[test]
fn import_flow_asks_for_units_and_preselects_the_one_that_fits_the_plate() {
    require_gpu!("import_flow_asks_for_units_and_preselects_the_one_that_fits_the_plate");
    let settings_dir = tempfile::tempdir().expect("a temp settings dir");
    let import_dir = tempfile::tempdir().expect("a temp import dir");
    let path = import_dir.path().join("part.stl");
    let mesh = mf_fixtures::cube(1.0);
    // 8 x 6 x 4 "units": unscaled numbers, but non-cubic so the
    // dialog's rows are distinguishable per axis.
    let positions: Vec<[f64; 3]> = mesh
        .vertices()
        .map(|(_, p)| [p[0] * 8.0, p[1] * 6.0, p[2] * 4.0])
        .collect();
    let (_, triangles) = mesh.to_arrays();
    let scaled = Mesh::new(positions, triangles).unwrap();
    mf_io::write_path(&scaled, &path, Format::StlBinary, Unit::Millimeter).unwrap();

    let settings = Settings {
        unit_prompt: true,
        config_dir: Some(settings_dir.path().to_owned()),
        ..Settings::default()
    };
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| {
            MeshFixerApp::with_settings(cc, Some(path.clone()), settings).unwrap()
        });
    wait_until(&mut h, "the unit dialog", |a| a.unit_prompt().is_some());
    h.run();
    let prompt = h.state().unit_prompt().unwrap();
    assert_eq!(prompt.extent, [8.0, 6.0, 4.0], "{prompt:?}");
    assert_eq!(prompt.choice, Unit::Inch, "{prompt:?}");
    assert!(
        !prompt.needs_scale,
        "203 x 152 x 102 mm fits a 250 mm plate"
    );
    h.get_by_label(
        "Inches · 8.00 × 6.00 × 4.00 in (203 × 152 × 102 mm) — Fits a 250 mm plate \
         (203 × 152 × 102 mm)",
    );
    h.get_by_label("Import").click();
    wait_until(&mut h, "import + analysis", |a| {
        a.unit_prompt().is_none() && a.current_analysis().is_some()
    });
    assert_eq!(h.state().session.document().units(), Unit::Inch);
}

// ---------------------------------------------------------------------
// Hollow acceptance (owner 2026-11-14): the four canonical shapes, each
// hollowed plain, with Auto drain holes, and opened through a flat cut
// face, clicked through the app. Their own group (`hollow_acceptance::`),
// skipped by scripts/core-workflows.sh (which stays fast) and run by
// scripts/hollow-acceptance.sh; the same checks as the command layer's
// crates/mf-command/tests/hollow_acceptance.rs.
// ---------------------------------------------------------------------

mod hollow_acceptance {
    use super::*;
    use mf_fixtures::hollow_check::{Opening, check, flat_at, holes_found, stood_bunny};

    const WALL: f64 = 2.0;

    /// A Parts-library shape's mesh, generated by the shape itself.
    fn generated(id: &str, params: &[(&str, &str)]) -> Mesh {
        let g = mf_parts::find_generator(id).unwrap();
        let raw: std::collections::BTreeMap<String, String> = params
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let params = g.spec().validate(&raw).unwrap();
        let ctx = mf_runtime::RunContext::none();
        let mut parts = g
            .generate(&params, &mf_parts::GenContext::new(&ctx))
            .unwrap();
        parts.remove(0).mesh
    }

    /// The object's mesh (object space) and its translation.
    fn object(h: &Harness<'_, MeshFixerApp>, name: &str) -> (Mesh, [f64; 3]) {
        let o = h.state().session.document().object(id_of(h, name)).unwrap();
        (
            o.mesh().unwrap().clone(),
            [o.transform[9], o.transform[10], o.transform[11]],
        )
    }

    /// Hollow's own automatic cell size for `mesh` at the default wall.
    fn cell_of(mesh: &Mesh) -> f64 {
        mf_operations::hollow::cell_size(0.0, WALL, mesh.bounding_box().unwrap().diagonal())
    }

    /// Opens Hollow on `name`, sets the drain mode by its label, and runs
    /// it; then the result, which must not have been refused.
    fn run_hollow(h: &mut Harness<'_, MeshFixerApp>, name: &str, drain: &str) -> Mesh {
        row(h, name).click();
        settle(h);
        if h.state().active_tool != "hollow" {
            h.get_by_label("Hollow").click();
            settle(h);
        }
        let id = id_of(h, name);
        h.get_by_label(drain).click();
        settle(h);
        wait_until(h, "the hollow preview", |a| {
            mf_desktop::hollow_tool::preview_render(a, id).is_some()
        });
        settle(h);
        hollow_now(h, name)
    }

    /// Presses the panel's Hollow button and waits for it.
    fn hollow_now(h: &mut Harness<'_, MeshFixerApp>, name: &str) -> Mesh {
        let rev = h.state().session.current();
        panel_button(h, "Hollow").click();
        wait_until(h, &format!("the hollow of {name}"), |a| {
            a.busy.is_none()
                && (a.session.current() != rev || a.refusal.is_some() || a.error.is_some())
        });
        settle(h);
        assert!(
            h.state().refusal.is_none(),
            "{name}: {:?}",
            h.state().refusal
        );
        assert!(h.state().error.is_none(), "{name}: {:?}", h.state().error);
        let parent = h
            .state()
            .session
            .node(h.state().session.current())
            .unwrap()
            .parent;
        assert_eq!(parent, Some(rev), "{name}: one undo step");
        object(h, name).0
    }

    fn undo_to(h: &mut Harness<'_, MeshFixerApp>, name: &str, mesh: &Mesh) {
        let id = id_of(h, name);
        let want = mesh.content_hash();
        h.get_by_label("Undo").click();
        wait_until(h, "the undo", |a| {
            a.busy.is_none()
                && a.session
                    .document()
                    .object(id)
                    .and_then(|o| o.mesh())
                    .is_some_and(|m| m.content_hash() == want)
        });
        settle(h);
    }

    /// `name` hollowed three ways through the app: plain, Auto drain
    /// holes, and (after Split at world `cut_z`, Keep Bottom) opened
    /// through the clicked cut face; each checked, each undone after.
    fn three_ways(h: &mut Harness<'_, MeshFixerApp>, name: &str, cut_z: f64) {
        let (before, t) = object(h, name);
        let cell = cell_of(&before);

        let out = run_hollow(h, name, "None");
        check(
            &format!("{name}, plain"),
            &before,
            &out,
            WALL,
            cell,
            &Opening::None,
        );
        undo_to(h, name, &before);

        let out = run_hollow(h, name, "Auto");
        let holes = holes_found(&before, &out, [0.0, 0.0, -1.0]);
        assert!(!holes.is_empty(), "{name}: Auto cut no drain hole");
        let doc = h.state().session.document().units();
        let diameter = h.state().hollow.diameter.unwrap().in_doc(doc);
        check(
            &format!("{name}, auto drain holes"),
            &before,
            &out,
            WALL,
            cell,
            &Opening::Holes(holes, diameter),
        );
        undo_to(h, name, &before);
        h.get_by_label("None").click();
        settle(h);

        // Split, Keep Bottom, at `cut_z`.
        row(h, name).click();
        settle(h);
        h.get_by_label("Split").click();
        settle(h);
        h.state_mut().split.plane[11] = cut_z;
        settle(h);
        h.get_by_label("Bottom").click();
        settle(h);
        let rev = h.state().session.current();
        scroll_and_click(h, "Cut");
        wait_until(h, "the cut", |a| {
            a.busy.is_none() && (a.session.current() != rev || a.refusal.is_some())
        });
        settle(h);
        assert!(
            h.state().refusal.is_none(),
            "{name}: {:?}",
            h.state().refusal
        );
        let bottom = format!("{name} (bottom)");
        let (cut, _) = object(h, &bottom);
        let z = cut_z - t[2];
        let faces = flat_at(&cut, z, 1.0);

        // Hollow, click the cut face, Open through, Hollow.
        row(h, &bottom).click();
        settle(h);
        h.get_by_label("Hollow").click();
        settle(h);
        let id = id_of(h, &bottom);
        wait_until(h, "the cut part's preview", |a| {
            mf_desktop::hollow_tool::preview_render(a, id).is_some()
        });
        settle(h);
        // A point on the cut face itself, well inside it.
        let f = *faces.first().expect("a cut face");
        let p = cut.face(FaceId(f)).unwrap().map(|v| cut.corner_position(v));
        let seed: [f64; 3] = std::array::from_fn(|k| (p[0][k] + p[1][k] + p[2][k]) / 3.0 + t[k]);
        let p = h.state().screen_point(seed).unwrap();
        click_at(h, p);
        settle(h);
        let picked: Vec<u32> = h
            .state()
            .hollow
            .picked_region
            .as_ref()
            .map(|r| r.faces.iter().map(|f| f.0).collect())
            .unwrap_or_default();
        assert!(!picked.is_empty(), "{name}: the click picked no surface");
        if !h.state().hollow.open_through {
            h.get_by_label("Open through this surface").click();
            settle(h);
        }
        assert!(h.state().hollow.open_through);
        let out = hollow_now(h, &bottom);
        check(
            &format!("{name}, open through the cut face"),
            &cut,
            &out,
            WALL,
            cell_of(&cut),
            &Opening::Through {
                z,
                side: 1.0,
                faces: &picked,
            },
        );
    }

    use mf_geometry::FaceId;

    /// Case 1: A single parametric shape: a Box, and a Cylinder.
    #[test]
    fn a_single_parametric_box_and_cylinder() {
        require_gpu!("hollow_acceptance::a_single_parametric_box_and_cylinder");
        let mut h = opened(vec![
            (
                "box",
                generated(
                    "shape.box",
                    &[("width", "30"), ("depth", "24"), ("height", "20")],
                ),
                at([0.0, 0.0, 0.0]),
            ),
            (
                "cylinder",
                generated("shape.cylinder", &[("diameter", "26"), ("height", "20")]),
                at([60.0, 0.0, 0.0]),
            ),
        ]);
        three_ways(&mut h, "box", 12.25);
        three_ways(&mut h, "cylinder", 12.25);
    }

    /// Opens a box with a cylinder standing up out of its top, then
    /// Combines them (selecting both, Combine) into "box".
    fn combined() -> Harness<'static, MeshFixerApp> {
        let boxed = generated(
            "shape.box",
            &[("width", "40"), ("depth", "40"), ("height", "20")],
        );
        let bb = boxed.bounding_box().unwrap();
        let centre = [
            (bb.min[0] + bb.max[0]) / 2.0,
            (bb.min[1] + bb.max[1]) / 2.0,
            0.0,
        ];
        let mut h = opened(vec![
            ("box", boxed, at([0.0, 0.0, 0.0])),
            (
                "cylinder",
                generated("shape.cylinder", &[("diameter", "24"), ("height", "40")]),
                at(centre),
            ),
        ]);
        select_together(&mut h, &["box", "cylinder"]);
        h.get_by_label("Combine").click();
        settle(&mut h);
        let history = history_len(&h);
        panel_button(&h, "Combine").click();
        wait_until(&mut h, "the combine", |a| {
            a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
        });
        settle(&mut h);
        assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
        assert_eq!(objects(&h), ["box"]);
        h
    }

    /// Case 2: Combined parametric shapes: Box ∪ Cylinder.
    #[test]
    fn combined_box_and_cylinder() {
        require_gpu!("hollow_acceptance::combined_box_and_cylinder");
        let mut h = combined();
        three_ways(&mut h, "box", 30.25);
    }

    /// Case 3: Make solid on Box ∪ Cylinder, then Hollow.
    #[test]
    fn make_solid_of_combined_box_and_cylinder() {
        require_gpu!("hollow_acceptance::make_solid_of_combined_box_and_cylinder");
        let mut h = combined();
        row(&h, "box").click();
        settle(&mut h);
        h.get_by_label("Make solid").click();
        settle(&mut h);
        let history = history_len(&h);
        h.get_by_label("Make it solid").click();
        wait_until(&mut h, "make solid", |a| {
            a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
        });
        settle(&mut h);
        assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
        three_ways(&mut h, "box", 30.25);
    }

    /// Case 4: The Stanford bunny: Repair (Fill all holes, its base's), then
    /// Hollow three ways, the open-through one on a cut taken across its
    /// middle (Keep Bottom). Skipped (recorded) if it hasn't been
    /// fetched (scripts/fetch-fixtures.sh).
    #[test]
    fn the_stanford_bunny() {
        require_gpu!("hollow_acceptance::the_stanford_bunny");
        let Some(path) = mf_fixtures::real::path(
            "stanford-bunny.obj",
            "hollow_acceptance::the_stanford_bunny",
        ) else {
            return;
        };
        let model = mf_io::read_model(&path).unwrap();
        let bunny = stood_bunny(&model.objects.into_iter().next().unwrap().mesh);
        let low = bunny.bounding_box().unwrap().min[2];
        let mut h = opened(vec![("bunny", bunny, at([0.0, 0.0, 0.0]))]);
        h.get_by_label("Repair").click();
        settle(&mut h);
        h.get_by_label("Fill all holes").click();
        settle(&mut h);
        let rev = h.state().session.current();
        h.get_by_label("Apply").click();
        wait_until(&mut h, "the repair", |a| {
            a.session.current() != rev && a.busy.is_none() && a.hole_view().is_some()
        });
        settle(&mut h);
        let (repaired, _) = object(&h, "bunny");
        let topo = repaired
            .topology_with(&mf_runtime::RunContext::none())
            .unwrap();
        assert_eq!(topo.boundary_edges, 0, "Repair left the bunny open");
        three_ways(&mut h, "bunny", low + 50.25);
    }

    /// Case 5 (owner bug): the Stanford bunny opened at its own scale, in
    /// metres — the unit a person actually gets picking "Import" on the
    /// unit prompt's suggestion (`suggested_unit`, `import_dialog.rs`:
    /// the bunny's ~0.156 largest dimension lands in the 10 mm..2 m window
    /// only as metres). Repair (Fill all holes), then Make solid at the
    /// default cell size, then Hollow: every length-param op must cross
    /// document units to mm and back correctly (D20), or Make solid
    /// refuses with a grid many orders too large (the owner's exact bug:
    /// a 0.001 m cell size reaching `prepare` as 0.000001).
    #[test]
    fn the_stanford_bunny_opened_in_metres() {
        require_gpu!("hollow_acceptance::the_stanford_bunny_opened_in_metres");
        let Some(path) = mf_fixtures::real::path(
            "stanford-bunny.obj",
            "hollow_acceptance::the_stanford_bunny_opened_in_metres",
        ) else {
            return;
        };
        let model = mf_io::read_model(&path).unwrap();
        let bunny = model.objects.into_iter().next().unwrap().mesh;
        let diag = bunny.bounding_box().unwrap().diagonal();
        // Genuinely metre-scale (the owner's report: 0.156 x 0.154 x
        // 0.121 m), not `stood_bunny`'s mm-friendly rescale.
        assert!(
            diag < 1.0,
            "the raw bunny should be metre-scale, got diagonal {diag}"
        );
        let mut h = opened_with_units(vec![("bunny", bunny, at([0.0, 0.0, 0.0]))], Unit::Meter);
        assert_eq!(h.state().session.document().units(), Unit::Meter);
        h.get_by_label("Repair").click();
        settle(&mut h);
        h.get_by_label("Fill all holes").click();
        settle(&mut h);
        let rev = h.state().session.current();
        h.get_by_label("Apply").click();
        wait_until(&mut h, "the repair", |a| {
            a.session.current() != rev && a.busy.is_none() && a.hole_view().is_some()
        });
        settle(&mut h);
        let (repaired, _) = object(&h, "bunny");
        let topo = repaired
            .topology_with(&mf_runtime::RunContext::none())
            .unwrap();
        assert_eq!(topo.boundary_edges, 0, "Repair left the bunny open");

        // Make solid at the default (automatic) cell size: must not be
        // refused (grid_too_large was the owner's exact symptom).
        row(&h, "bunny").click();
        settle(&mut h);
        h.get_by_label("Make solid").click();
        settle(&mut h);
        let history = history_len(&h);
        h.get_by_label("Make it solid").click();
        wait_until(&mut h, "make solid", |a| {
            a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
        });
        settle(&mut h);
        assert!(
            h.state().refusal.is_none(),
            "Make solid at the default cell size was refused: {:?}",
            h.state().refusal
        );

        // Hollow at its default wall thickness: must not be refused
        // either (the same double-conversion would have broken this
        // too, on wall_thickness/hole_diameter).
        row(&h, "bunny").click();
        settle(&mut h);
        h.get_by_label("Hollow").click();
        settle(&mut h);
        let id = id_of(&h, "bunny");
        wait_until(&mut h, "the hollow preview", |a| {
            mf_desktop::hollow_tool::preview_render(a, id).is_some()
        });
        settle(&mut h);
        hollow_now(&mut h, "bunny");
    }
}

/// Every length-param tool, in an mm document and in an m document, on
/// the same model at the same real-world size (MF-UNIT-001, owner bug):
/// Make solid's cell size, Hollow's wall thickness, Split's gap and a
/// placed shape's own size must come out the same real-world size after
/// scaling, whichever unit the document is in. Each panel edits and
/// shows lengths in document units (a "2" in an mm document's field and
/// a "0.002" in an m document's field are the same 2 mm), which
/// `Session::prepare` converts to mm exactly once (D20); a desktop panel
/// that skips the cross to mm before building its invocation makes
/// `prepare` convert an already-document-units number a second time.
mod unit_parity {
    use super::*;

    /// A document unit, paired with how many of its own units make one
    /// millimetre (the inverse of `transform_tool::mm_per`): a
    /// real-world length in mm, times this, is the number a panel's
    /// document-units field shows for it.
    const UNITS: [(Unit, f64); 2] = [(Unit::Millimeter, 1.0), (Unit::Meter, 0.001)];

    /// A closed cube of `mm` millimetres across, opened as the one
    /// object "cube" in a document at `unit` (its own numbers scaled so
    /// the cube is the same real-world size in either unit).
    fn cube_doc(mm: f64, unit: Unit, doc_units_per_mm: f64) -> Harness<'static, MeshFixerApp> {
        let size = mm * doc_units_per_mm;
        opened_with_units(
            vec![("cube", mf_fixtures::cube(size), at([0.0, 0.0, 0.0]))],
            unit,
        )
    }

    /// "cube"'s world-space bounding box diagonal, in millimetres
    /// (document units divided by `doc_units_per_mm`): the one number
    /// every unit's run of a tool is compared by.
    fn diagonal_mm(h: &Harness<'_, MeshFixerApp>, name: &str, doc_units_per_mm: f64) -> f64 {
        let id = id_of(h, name);
        world_bounds(h, id).diagonal() / doc_units_per_mm
    }

    /// Make solid at a cell size of `cell_mm` millimetres (entered in
    /// the panel's own document-units field): the result's bounding box
    /// diagonal, in millimetres.
    fn make_solid_diagonal_mm(unit: Unit, doc_units_per_mm: f64, cell_mm: f64) -> f64 {
        let mut h = cube_doc(20.0, unit, doc_units_per_mm);
        h.get_by_label("Make solid").click();
        settle(&mut h);
        h.state_mut()
            .tool_state_mut::<mf_desktop::shape_tools::SolidState>()
            .cell = Some(mf_units::Length::new(cell_mm * doc_units_per_mm, unit));
        let history = history_len(&h);
        h.get_by_label("Make it solid").click();
        wait_until(&mut h, "make solid", |a| {
            a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
        });
        settle(&mut h);
        assert!(
            h.state().refusal.is_none(),
            "Make solid ({unit:?}, cell {cell_mm} mm): {:?}",
            h.state().refusal
        );
        diagonal_mm(&h, "cube", doc_units_per_mm)
    }

    #[test]
    fn make_solid_matches_in_mm_and_m_documents() {
        require_gpu!("unit_parity::make_solid_matches_in_mm_and_m_documents");
        let results: Vec<f64> = UNITS
            .iter()
            .map(|&(unit, doc_units_per_mm)| make_solid_diagonal_mm(unit, doc_units_per_mm, 1.0))
            .collect();
        assert!(
            (results[0] - results[1]).abs() < 0.5,
            "Make solid's result differs between mm and m documents: {results:?}"
        );
    }

    /// Hollow at its default wall thickness (2 mm, D20's auto default,
    /// resolved from document units the same way the panel shows it):
    /// the shell's resulting bounding box diagonal (unchanged by
    /// Hollow, which keeps the outer surface exactly) and whether it
    /// was refused, in millimetres.
    fn hollow_result(unit: Unit, doc_units_per_mm: f64) -> (f64, bool) {
        let mut h = cube_doc(20.0, unit, doc_units_per_mm);
        h.get_by_label("Hollow").click();
        settle(&mut h);
        let id = id_of(&h, "cube");
        wait_until(&mut h, "the hollow preview", |a| {
            mf_desktop::hollow_tool::preview_render(a, id).is_some() || a.refusal.is_some()
        });
        settle(&mut h);
        let history = history_len(&h);
        panel_button(&h, "Hollow").click();
        wait_until(&mut h, "hollow", |a| {
            a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
        });
        settle(&mut h);
        let refused = h.state().refusal.is_some();
        (diagonal_mm(&h, "cube", doc_units_per_mm), refused)
    }

    #[test]
    fn hollow_matches_in_mm_and_m_documents() {
        require_gpu!("unit_parity::hollow_matches_in_mm_and_m_documents");
        let results: Vec<(f64, bool)> = UNITS
            .iter()
            .map(|&(unit, doc_units_per_mm)| hollow_result(unit, doc_units_per_mm))
            .collect();
        assert!(
            !results[0].1 && !results[1].1,
            "Hollow was refused in one of the documents: {results:?}"
        );
        assert!(
            (results[0].0 - results[1].0).abs() < 0.5,
            "Hollow's result differs between mm and m documents: {results:?}"
        );
    }

    /// Split at a gap of `gap_mm` millimetres, Keep both: the top
    /// part's resulting bounding box diagonal, in millimetres.
    fn split_gap_diagonal_mm(unit: Unit, doc_units_per_mm: f64, gap_mm: f64) -> f64 {
        let mut h = cube_doc(20.0, unit, doc_units_per_mm);
        h.get_by_label("Split").click();
        settle(&mut h);
        h.state_mut().split.gap = mf_units::Length::new(gap_mm * doc_units_per_mm, unit);
        let history = history_len(&h);
        scroll_and_click(&mut h, "Cut");
        wait_until(&mut h, "the cut", |a| {
            a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
        });
        settle(&mut h);
        assert!(
            h.state().refusal.is_none(),
            "Split ({unit:?}, gap {gap_mm} mm): {:?}",
            h.state().refusal
        );
        diagonal_mm(&h, "cube (top)", doc_units_per_mm)
    }

    #[test]
    fn split_gap_matches_in_mm_and_m_documents() {
        require_gpu!("unit_parity::split_gap_matches_in_mm_and_m_documents");
        let results: Vec<f64> = UNITS
            .iter()
            .map(|&(unit, doc_units_per_mm)| split_gap_diagonal_mm(unit, doc_units_per_mm, 1.0))
            .collect();
        assert!(
            (results[0] - results[1]).abs() < 0.5,
            "Split's gapped result differs between mm and m documents: {results:?}"
        );
    }

    /// A placed library shape (a 10 mm Box) keeps its real-world size:
    /// its world-space bounding box diagonal, in millimetres. Its own
    /// transform's linear part carries the shape's mm-to-document-units
    /// scale (its mesh is generated in mm, D16/D20), unlike every other
    /// object `world_bounds` is used for in this file (placed with [`at`]
    /// alone, no scale) — so every one of its local bounding box's 8
    /// corners is carried through the full affine transform, not just
    /// translated.
    fn placed_box_diagonal_mm(unit: Unit, doc_units_per_mm: f64) -> f64 {
        let mut h = cube_doc(20.0, unit, doc_units_per_mm);
        h.get_by_label("Parts").click();
        settle(&mut h);
        let history = history_len(&h);
        h.get_by_label("Box").click_accesskit();
        wait_until(&mut h, "the shape placed", |a| {
            a.busy.is_none() && a.session.history_labels().len() > history
        });
        settle(&mut h);
        let id = h.state().active.unwrap();
        let doc = h.state().session.document();
        let o = doc.object(id).unwrap();
        let bb = o.mesh().unwrap().bounding_box().unwrap();
        let t = o.transform;
        let m = glam::DMat3::from_cols(
            DVec3::new(t[0], t[1], t[2]),
            DVec3::new(t[3], t[4], t[5]),
            DVec3::new(t[6], t[7], t[8]),
        );
        let translation = DVec3::new(t[9], t[10], t[11]);
        let corners = [bb.min, bb.max].into_iter().flat_map(|p0| {
            [bb.min, bb.max].into_iter().flat_map(move |p1| {
                [bb.min, bb.max]
                    .into_iter()
                    .map(move |p2| DVec3::new(p0[0], p1[1], p2[2]))
            })
        });
        let (mut lo, mut hi) = (DVec3::splat(f64::MAX), DVec3::splat(f64::MIN));
        for c in corners {
            let world = m * c + translation;
            lo = lo.min(world);
            hi = hi.max(world);
        }
        (hi - lo).length() / doc_units_per_mm
    }

    #[test]
    fn a_placed_shape_matches_in_mm_and_m_documents() {
        require_gpu!("unit_parity::a_placed_shape_matches_in_mm_and_m_documents");
        let results: Vec<f64> = UNITS
            .iter()
            .map(|&(unit, doc_units_per_mm)| placed_box_diagonal_mm(unit, doc_units_per_mm))
            .collect();
        assert!(
            (results[0] - results[1]).abs() < 0.5,
            "A placed shape's real-world size differs between mm and m documents: {results:?}"
        );
    }
}

// ---------------------------------------------------------------------
// Gizmo snapping is zoom-adaptive and unit-aware (owner bug, MF-OBJ-002):
// the gizmo's grid used to be a fixed 1 *document unit* step
// (`mf_render::Snap::default`), which on the owner's metre-scale
// stanford-bunny document was 1000 mm — every drag on the 0.15 m model
// snapped to zero, reading as "the move handle wasn't working". The fix
// (`transform_tool::snap_step`) picks a 1-2-5-mm step sized to stay
// visible on screen at the current zoom, in document units, applied to
// both the Transform gizmo and Split's plane gizmo.
mod gizmo_snap {
    use super::*;

    /// `id`'s world-space bounding box, correct under rotation (unlike
    /// the file-level `world_bounds`, which only adds translation —
    /// fine for every other test here, none of which rotate the
    /// object, but wrong once a test does): all 8 corners of the local
    /// bounding box, carried through the transform's full linear part,
    /// not just its translation column.
    fn world_bounds_rotated(h: &Harness<'_, MeshFixerApp>, id: mf_command::ObjectId) -> Aabb {
        let doc = h.state().session.document();
        let o = doc.object(id).unwrap();
        let bb = o.mesh().unwrap().bounding_box().unwrap();
        let t = o.transform;
        let m = glam::DMat3::from_cols(
            DVec3::new(t[0], t[1], t[2]),
            DVec3::new(t[3], t[4], t[5]),
            DVec3::new(t[6], t[7], t[8]),
        );
        let translation = DVec3::new(t[9], t[10], t[11]);
        let corners = [bb.min, bb.max].into_iter().flat_map(|p0| {
            [bb.min, bb.max].into_iter().flat_map(move |p1| {
                [bb.min, bb.max]
                    .into_iter()
                    .map(move |p2| DVec3::new(p0[0], p1[1], p2[2]))
            })
        });
        let (mut lo, mut hi) = (DVec3::splat(f64::MAX), DVec3::splat(f64::MIN));
        for c in corners {
            let world = m * c + translation;
            lo = lo.min(world);
            hi = hi.max(world);
        }
        Aabb {
            min: lo.to_array(),
            max: hi.to_array(),
        }
    }

    /// Opens the raw Stanford bunny scan (metre-scale, y-up, exactly as
    /// the owner's file reads: 0.156 x 0.154 x 0.121 m — see
    /// `hollow_acceptance::the_stanford_bunny_opened_in_metres`) as a
    /// metre document, the owner's own exact first step. `None` (and a
    /// recorded skip) if the fixture hasn't been fetched.
    fn opened_bunny_in_metres(test: &str) -> Option<Harness<'static, MeshFixerApp>> {
        let path = mf_fixtures::real::path("stanford-bunny.obj", test)?;
        let model = mf_io::read_model(&path).unwrap();
        let bunny = model.objects.into_iter().next().unwrap().mesh;
        Some(opened_with_units(
            vec![("bunny", bunny, at([0.0, 0.0, 0.0]))],
            Unit::Meter,
        ))
    }

    /// A point to grab (and the outward drag direction) on gizmo `g`'s
    /// `handle`, read from its own drawn geometry — the same way
    /// `x_arrow`/`handle_point` do for the Transform gizmo, generalised
    /// to any gizmo (here, Split's).
    fn axis_handle_point(
        h: &Harness<'_, MeshFixerApp>,
        g: &mf_render::Gizmo,
        handle: mf_render::Handle,
    ) -> (egui::Pos2, egui::Vec2) {
        let app = h.state();
        let cam = app.camera.unwrap();
        let rect = app.viewport.last_rect.unwrap();
        let view = (rect.width() as f64, rect.height() as f64);
        let shape = g
            .handles(&cam, view)
            .into_iter()
            .find(|s| s.handle == handle)
            .unwrap_or_else(|| panic!("{handle:?} is drawn"));
        let mf_render::HandleGeometry::Arrow {
            shaft_start,
            shaft_end,
            ..
        } = shape.geometry
        else {
            panic!("{handle:?}'s geometry isn't an arrow");
        };
        let mid = (
            (shaft_start.0 + shaft_end.0) / 2.0,
            (shaft_start.1 + shaft_end.1) / 2.0,
        );
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

    /// The owner's exact steps: open the bunny (metres), open Split,
    /// choose Vertical Y, then drag the plane's move handle. Before the
    /// fix, the gizmo's grid was a fixed 1 document unit (1000 mm on
    /// this document), so every drag snapped to zero and the handle
    /// read as broken; the plane must actually move and follow the
    /// cursor along its own normal (+Y).
    #[test]
    fn split_vertical_y_move_handle_follows_the_cursor_on_the_metre_bunny() {
        require_gpu!(
            "gizmo_snap::split_vertical_y_move_handle_follows_the_cursor_on_the_metre_bunny"
        );
        let Some(mut h) = opened_bunny_in_metres(
            "gizmo_snap::split_vertical_y_move_handle_follows_the_cursor_on_the_metre_bunny",
        ) else {
            return;
        };
        h.get_by_label("Split").click();
        settle(&mut h);
        h.get_by_label("Vertical Y").click();
        settle(&mut h);
        assert_eq!(h.state().split.mode, mf_render::GizmoMode::Move);
        let n = h.state().split.normal();
        assert!((n.y - 1.0).abs() < 1e-9, "{n:?}");
        let before = h.state().split.point();

        let (g, _) = mf_desktop::shape_tools::split_gizmo(h.state()).unwrap();
        let (p, dir_y) = axis_handle_point(&h, &g, mf_render::Handle::Axis(1));
        // A real screen drag, well short of the gizmo's own 110pt arm,
        // so it only ever tests the grid step, never the arm's length.
        let to = p + dir_y * 40.0;
        drag(&mut h, p, to, 10);
        settle(&mut h);

        let after = h.state().split.point();
        assert!(
            (after - before).length() > 1e-6,
            "the plane's move handle didn't move it: before {before:?}, after {after:?}"
        );
        // Follows the cursor: the point moved along +Y (the dragged
        // axis), clearly more than off-axis noise — the ray-plane drag
        // math moves a little off the pure axis near the screen edge of
        // the arm, on the order of a couple of grid steps, not a sign
        // the drag went somewhere else entirely — and nowhere near 0,
        // which an oversized (pre-fix) snap step would have clamped it
        // to.
        let moved = after - before;
        assert!(moved.y > 0.01, "{moved:?}");
        assert!(moved.x.abs() < moved.y * 0.2, "{moved:?}");
        assert!(moved.z.abs() < moved.y * 0.2, "{moved:?}");
    }

    /// The shared Snap checkbox (owner, 2026-11-14: "can't we disable
    /// snap?"), unchecked: dragging Split's plane on the metre bunny
    /// moves smoothly — continuously tracking the cursor one pixel step
    /// at a time, not jumping between quantised grid positions — the
    /// same way Transform's own "Snap" checkbox has always let a drag
    /// move freely when unchecked.
    #[test]
    fn split_plane_drags_smoothly_with_snap_unchecked_on_the_metre_bunny() {
        require_gpu!(
            "gizmo_snap::split_plane_drags_smoothly_with_snap_unchecked_on_the_metre_bunny"
        );
        let Some(mut h) = opened_bunny_in_metres(
            "gizmo_snap::split_plane_drags_smoothly_with_snap_unchecked_on_the_metre_bunny",
        ) else {
            return;
        };
        h.get_by_label("Split").click();
        settle(&mut h);
        h.get_by_label("Vertical Y").click();
        settle(&mut h);
        assert!(h.state().gizmo_snap, "snap is on by default");
        h.state_mut().gizmo_snap = false;
        settle(&mut h);

        let (g, _) = mf_desktop::shape_tools::split_gizmo(h.state()).unwrap();
        let (p, dir_y) = axis_handle_point(&h, &g, mf_render::Handle::Axis(1));

        // Drag one pixel at a time over a longer run than egui's own
        // click-vs-drag deadzone (the first handful of pixel steps
        // move nothing yet, while the pointer is still inside that
        // deadzone — real UI behaviour, not a snap defect): once past
        // it, every further pixel step should move the plane a little
        // with no snap on, not sit on a handful of quantised
        // positions. The back half of the run (steps 31-60, well clear
        // of any deadzone) is what's checked.
        let mut ys = Vec::new();
        h.event(egui::Event::PointerMoved(p));
        h.event(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        const STEPS: i32 = 60;
        for i in 1..=STEPS {
            let at = p + dir_y * i as f32;
            h.event(egui::Event::PointerMoved(at));
            h.step();
            ys.push(h.state().split.point().y);
        }
        h.event(egui::Event::PointerButton {
            pos: p + dir_y * STEPS as f32,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        settle(&mut h);

        let back_half = &ys[(STEPS as usize / 2)..];
        let distinct = {
            let mut sorted = back_half.to_vec();
            sorted.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
            sorted.len()
        };
        assert!(
            distinct >= back_half.len() - 1,
            "a smooth (unsnapped) drag should move on almost every pixel step, \
             only {distinct} distinct positions out of {} steps: {ys:?}",
            back_half.len()
        );
        assert!(
            back_half.windows(2).all(|w| w[1] >= w[0] - 1e-9),
            "a smooth drag along +Y should move monotonically: {ys:?}"
        );
    }

    /// Transform move on the metre-scale bunny actually moves it: the
    /// same fixed-1-document-unit grid bug affected the Transform
    /// gizmo too (`app::transform_snap`), not just Split's.
    #[test]
    fn transform_move_on_the_metre_bunny_moves_it() {
        require_gpu!("gizmo_snap::transform_move_on_the_metre_bunny_moves_it");
        let Some(mut h) =
            opened_bunny_in_metres("gizmo_snap::transform_move_on_the_metre_bunny_moves_it")
        else {
            return;
        };
        let id = id_of(&h, "bunny");
        open_transform(&mut h);
        settle(&mut h);
        let before = transform_of(&h, id);
        let history = history_len(&h);

        let (p, dir_x) = x_arrow(&h);
        let to = p + dir_x * 40.0;
        drag(&mut h, p, to, 10);
        wait_until(&mut h, "the move commits", |a| {
            a.busy.is_none() && a.session.history_labels().len() == history + 1
        });
        settle(&mut h);

        let after = transform_of(&h, id);
        assert!(
            after[9] > before[9] + 1e-6,
            "the bunny didn't move: before {before:?}, after {after:?}"
        );
        assert_eq!(after[..9], before[..9], "only the translation changes");
    }

    /// Drags the Z-axis move handle so the plane's point lands on
    /// `target_z` (document units), the same gizmo interaction the
    /// owner used: each pass measures how much world Z the last screen
    /// drag covered (and its sign, since whether the arm's on-screen
    /// direction is +Z or -Z depends on the view), to work out the next
    /// pixel delta — a perspective camera's screen-to-world ratio isn't
    /// quite constant, so a few passes converge where one guess
    /// wouldn't.
    /// Drags the Z-axis move handle so the plane's point lands within
    /// `tolerance` (document units) of `target_z`: a first calibration
    /// drag measures how much world Z a screen point of drag covers
    /// (and its sign, since whether the arm's on-screen direction is +Z
    /// or -Z depends on the view), then a few passes correct for a
    /// perspective camera's screen-to-world ratio not being quite
    /// constant. Panics (with where it got stuck) if it never lands
    /// within tolerance.
    fn drag_split_plane_z_to(h: &mut Harness<'_, MeshFixerApp>, target_z: f64, tolerance: f64) {
        let (g, _) = mf_desktop::shape_tools::split_gizmo(h.state()).unwrap();
        let (p, dir_z) = axis_handle_point(h, &g, mf_render::Handle::Axis(2));
        const CALIBRATE_PX: f32 = 30.0;
        let mut world_per_px = {
            let before = h.state().split.point().z;
            drag(h, p, p + dir_z * CALIBRATE_PX, 6);
            settle(h);
            let after = h.state().split.point().z;
            assert!(
                (after - before).abs() > 1e-9,
                "the Z handle moved nothing on a {CALIBRATE_PX}pt drag (world {after} vs {before})"
            );
            (after - before) / CALIBRATE_PX as f64
        };
        for _ in 0..10 {
            let z = h.state().split.point().z;
            let remaining = target_z - z;
            if remaining.abs() < tolerance {
                return;
            }
            // Never a wild, possibly off-screen guess: capped to a
            // screen-reasonable step even if `world_per_px` came out
            // tiny (e.g. a snap step landed exactly on the calibration
            // probe's end, moving it by ~0).
            let px = (remaining / world_per_px).clamp(-500.0, 500.0) as f32;
            drag(h, p, p + dir_z * px, 10);
            settle(h);
            let moved = h.state().split.point().z - z;
            if moved.abs() > 1e-9 && px.abs() > 1e-6 {
                world_per_px = moved / px as f64;
            }
        }
        let z = h.state().split.point().z;
        assert!(
            (z - target_z).abs() < tolerance,
            "dragging the Z handle never converged on {target_z}, stuck at {z}"
        );
    }

    /// Splitting the bunny with a horizontal plane near its feet (its
    /// bounding box's low Z), placed by *dragging the move handle*
    /// (not a typed field) — the owner's own way of placing a plane —
    /// succeeds, and the kept bottom part's own floor is flat and
    /// capped: a zoom-adaptive grid step must still let a dragged plane
    /// land close to a real surface, not snap it somewhere coarse.
    #[test]
    fn split_near_the_feet_on_the_metre_bunny_leaves_a_flat_bottom() {
        require_gpu!("gizmo_snap::split_near_the_feet_on_the_metre_bunny_leaves_a_flat_bottom");
        let Some(mut h) = opened_bunny_in_metres(
            "gizmo_snap::split_near_the_feet_on_the_metre_bunny_leaves_a_flat_bottom",
        ) else {
            return;
        };
        // Repair first (Split refuses an open mesh), same as
        // `hollow_acceptance::the_stanford_bunny_opened_in_metres`.
        h.get_by_label("Repair").click();
        settle(&mut h);
        h.get_by_label("Fill all holes").click();
        settle(&mut h);
        let rev = h.state().session.current();
        h.get_by_label("Apply").click();
        wait_until(&mut h, "the repair", |a| {
            a.session.current() != rev && a.busy.is_none() && a.hole_view().is_some()
        });
        settle(&mut h);

        let id = id_of(&h, "bunny");
        let bb0 = world_bounds(&h, id);
        let low_z = bb0.min[2];
        let height = bb0.max[2] - low_z;
        // Near enough the feet that most of the bunny stays in the
        // kept "Bottom" part's complement (document units: metres):
        // within the bottom tenth of its own height, with a tolerance
        // comfortably inside that band — loose enough for the
        // zoom-adaptive grid's own step (a few mm here) and a
        // perspective camera's screen-to-world ratio not being quite
        // constant, but still nowhere near the bunny's middle.
        let target_z = low_z + height * 0.05;
        let tolerance = (height * 0.08).max(0.01);

        h.get_by_label("Split").click();
        settle(&mut h);
        let n = h.state().split.normal();
        assert!(
            (n.z - 1.0).abs() < 1e-9,
            "a default plane's normal is +Z: {n:?}"
        );
        drag_split_plane_z_to(&mut h, target_z, tolerance);
        let cut_z = h.state().split.point().z;
        assert!(
            cut_z < low_z + height * 0.2,
            "the dragged plane should have landed near the feet, not the middle: {cut_z} (feet {low_z}, height {height})"
        );

        let names = cut_with_keep(&mut h, "Bottom");
        assert_eq!(names, ["bunny (bottom)"], "{names:?}");
        let kept = h
            .state()
            .session
            .document()
            .object(id_of(&h, "bunny (bottom)"))
            .unwrap()
            .mesh()
            .unwrap()
            .clone();
        let topo = kept.topology_with(&mf_runtime::RunContext::none()).unwrap();
        assert_eq!(topo.boundary_edges, 0, "the cut's cap left the bottom open");
        let bb = world_bounds(&h, id_of(&h, "bunny (bottom)"));
        assert!(
            bb.max[2] <= cut_z + 1e-6,
            "the kept bottom part reaches above the cut: {bb:?}"
        );
        assert!(
            (bb.min[2] - low_z).abs() < 1e-6,
            "the kept bottom part should still reach down to the original feet: {bb:?}"
        );
    }

    /// An mm-scale model still snaps to a plain 1 mm step at a normal
    /// zoom (the fix is zoom-*adaptive*, not merely "always finer"): the
    /// zoom-adaptive grid must not regress the common case every other
    /// test in this file already relies on implicitly.
    #[test]
    fn mm_model_snaps_to_one_millimetre_at_normal_zoom() {
        require_gpu!("gizmo_snap::mm_model_snaps_to_one_millimetre_at_normal_zoom");
        let mut h = opened(vec![("cube", mf_fixtures::cube(40.0), at([0.0; 3]))]);
        open_transform(&mut h);
        settle(&mut h);
        let app = h.state();
        let g = app.transform_gizmo().unwrap();
        let cam = app.camera.unwrap();
        let rect = app.viewport.last_rect.unwrap();
        let view = (rect.width() as f64, rect.height() as f64);
        let (grid, step_mm) =
            mf_desktop::transform_tool::snap_step(&cam, g.pivot, view, Unit::Millimeter);
        assert!(
            (step_mm - 1.0).abs() < 1e-9,
            "a normal-zoom mm document should still pick a 1 mm step, got {step_mm} mm"
        );
        assert!(
            (grid - 1.0).abs() < 1e-9,
            "{grid} document units != 1 mm here"
        );
    }

    /// The owner's follow-up report (2026-11-14): import the bunny
    /// (metres), rotate it 90 degrees about X, drop it to the plate, fill
    /// its holes, *then* open Split. Before this fix, Split's plane
    /// (`SplitState::placed_for`) cached the *selection* it was last
    /// centred for, not that selection's transform — so once Split had
    /// placed the plane for "bunny" at all (even in an earlier, unrelated
    /// open of the tool), a later open with the same selection but a
    /// moved bunny left the plane exactly where it used to be: Point
    /// showed (0, 0, 0), off to the side of the rotated, dropped model,
    /// and the move handle read as broken because it was there on the
    /// gizmo's own small on-screen arm, just nowhere near the bunny.
    /// Opening Split must re-centre the plane on the selection's CURRENT
    /// world bounding box, on open and after any transform change.
    #[test]
    fn split_recentres_the_plane_after_a_rotate_and_drop_to_plate() {
        require_gpu!("gizmo_snap::split_recentres_the_plane_after_a_rotate_and_drop_to_plate");
        let Some(mut h) = opened_bunny_in_metres(
            "gizmo_snap::split_recentres_the_plane_after_a_rotate_and_drop_to_plate",
        ) else {
            return;
        };
        let id = id_of(&h, "bunny");

        // Open Split once already (so `placed_for` caches "bunny" at its
        // *original* placement) before the rotate and drop — reproducing
        // the owner's own session shape, where Split wasn't necessarily
        // opened for the first time afterwards.
        h.get_by_label("Split").click();
        settle(&mut h);
        open_transform(&mut h);
        settle(&mut h);

        // Rotate 90 degrees about X, about the bounding-box centre.
        let before = h.state().session.document().object(id).unwrap().transform;
        let centre = {
            let doc = h.state().session.document();
            let mesh = doc.object(id).unwrap().mesh().unwrap();
            DVec3::from(mesh.bounding_box().unwrap().center())
        };
        let rotated = mf_desktop::transform_tool::with_rotation_scale(
            &before,
            centre,
            DVec3::new(90.0, 0.0, 0.0),
            DVec3::ONE,
        )
        .unwrap();
        h.state_mut().active = Some(id);
        h.state_mut()
            .commit_transform(id, rotated, "Rotated bunny 90 about X".into());
        wait_until(&mut h, "the rotate commits", |a| {
            a.busy.is_none() && a.session.document().object(id).unwrap().transform != before
        });
        settle(&mut h);

        // Drop to plate.
        let before_drop = h.state().session.document().object(id).unwrap().transform;
        h.state_mut().drop_to_plate();
        wait_until(&mut h, "drop to plate commits", |a| {
            a.busy.is_none() && a.session.document().object(id).unwrap().transform != before_drop
        });
        settle(&mut h);

        // Fill holes (Repair).
        h.get_by_label("Repair").click();
        settle(&mut h);
        h.get_by_label("Fill all holes").click();
        settle(&mut h);
        let rev = h.state().session.current();
        h.get_by_label("Apply").click();
        wait_until(&mut h, "the repair", |a| {
            a.session.current() != rev && a.busy.is_none() && a.hole_view().is_some()
        });
        settle(&mut h);

        // The bunny's own current world bounds, after rotate and drop
        // (rotation-aware — the bunny is no longer axis-aligned with its
        // local box after a 90-degree turn).
        let bb = world_bounds_rotated(&h, id);
        let expected_centre = (DVec3::from(bb.min) + DVec3::from(bb.max)) * 0.5;

        // Now open Split: the plane must be centred on the CURRENT
        // bounds, not wherever it was left before the rotate.
        h.get_by_label("Split").click();
        settle(&mut h);
        let point = h.state().split.point();
        assert!(
            (point - expected_centre).length() < 1e-6,
            "the plane didn't re-centre on the rotated, dropped bunny: \
             point {point:?}, expected near {expected_centre:?} (bounds {bb:?})"
        );

        // And the move handle actually moves it from there (not stuck,
        // the owner's literal complaint).
        let (g, _) = mf_desktop::shape_tools::split_gizmo(h.state()).unwrap();
        let (p, dir_z) = axis_handle_point(&h, &g, mf_render::Handle::Axis(2));
        drag(&mut h, p, p + dir_z * 40.0, 10);
        settle(&mut h);
        let after = h.state().split.point();
        assert!(
            (after - point).length() > 1e-6,
            "the plane's move handle still didn't move it: {point:?} -> {after:?}"
        );
    }
}

// ---------------------------------------------------------------------
// Tool sessions (ADR-0007, C21): Esc cancels a drag in every tool, and
// undo leaves no stale tool state.
// ---------------------------------------------------------------------

/// A point of the view (window points) the open tool claims a press at,
/// nearest `near` (the shell's own `claims` test, scanned on a grid).
fn claimed_point(h: &Harness<'_, MeshFixerApp>, near: egui::Pos2) -> Option<egui::Pos2> {
    let app = h.state();
    let rect = app.viewport.last_rect?;
    let claims = (app.active_tool_def().claims)(app)?;
    let mut best: Option<egui::Pos2> = None;
    let mut y = 2.0;
    while y < rect.height() {
        let mut x = 2.0;
        while x < rect.width() {
            if claims((x as f64, y as f64)) {
                let p = rect.left_top() + egui::vec2(x, y);
                if best.is_none_or(|b| (p - near).length() < (b - near).length()) {
                    best = Some(p);
                }
            }
            x += 4.0;
        }
        y += 4.0;
    }
    best
}

/// Press at `from` and move to `to` in steps, the button still held.
fn press_and_move(h: &mut Harness<'_, MeshFixerApp>, from: egui::Pos2, to: egui::Pos2) {
    h.event(egui::Event::PointerMoved(from));
    h.step();
    h.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    for i in 1..=8 {
        let p = from + (to - from) * (i as f32 / 8.0);
        h.event(egui::Event::PointerMoved(p));
        h.step();
    }
}

fn release_at(h: &mut Harness<'_, MeshFixerApp>, at: egui::Pos2) {
    h.event(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.run_steps(2);
}

/// Owner: "Esc can't cancel the Split drag". Dragging the cut plane's
/// gizmo, then Esc while the button is still down, puts the plane back
/// where it was; Split stays open, and the release afterwards neither
/// moves it again nor cuts.
#[test]
fn esc_cancels_a_split_drag() {
    require_gpu!("esc_cancels_a_split_drag");
    let mut h = opened(vec![("ball", ball(30.0), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "split");
    let plane = h.state().split.plane;
    let centre = h.state().screen_point([0.0, 0.0, 30.0]).unwrap();
    let from = claimed_point(&h, centre).expect("the plane's gizmo is under the pointer somewhere");
    let history = history_len(&h);
    let to = from + egui::vec2(0.0, -60.0);
    press_and_move(&mut h, from, to);
    assert_ne!(h.state().split.plane, plane, "the drag moved the plane");

    h.key_press(egui::Key::Escape);
    h.step();
    assert_eq!(h.state().split.plane, plane, "Esc put the plane back");
    assert_eq!(
        h.state().active_tool,
        "split",
        "Esc took one step: the drag, not the tool"
    );

    h.event(egui::Event::PointerMoved(to + egui::vec2(0.0, -30.0)));
    h.step();
    release_at(&mut h, to + egui::vec2(0.0, -30.0));
    settle(&mut h);
    assert_eq!(
        h.state().split.plane,
        plane,
        "the cancelled drag stays cancelled"
    );
    assert_eq!(history_len(&h), history, "nothing was cut");
}

/// Owner: "Esc can't cancel the Align drag". Pressing an Align dot and
/// holding it shows where every target would go; Esc while it's held
/// takes the ghosts away, Align stays open, and the release applies
/// nothing.
#[test]
fn esc_cancels_an_align_drag() {
    use mf_desktop::align_tool::Mode;
    require_gpu!("esc_cancels_an_align_drag");
    let mut h = opened(three_balls_on(0));
    open_align(&mut h, &["a", "b", "c"]);
    let ids: Vec<mf_command::ObjectId> = ["a", "b", "c"].iter().map(|n| id_of(&h, n)).collect();
    let before: Vec<[f64; 12]> = ids
        .iter()
        .map(|&id| h.state().session.document().object(id).unwrap().transform)
        .collect();
    let shown = |h: &Harness<'_, MeshFixerApp>| -> Vec<Option<[f64; 12]>> {
        ids.iter()
            .map(|&id| h.state().shown_transform(id))
            .collect()
    };
    let dot = dot_point(&h, find_dot(&h, 0, Mode::Max));
    let history = history_len(&h);
    press_and_move(&mut h, dot, dot + egui::vec2(10.0, 10.0));
    assert!(
        shown(&h).iter().any(Option::is_some),
        "holding the dot shows where the targets would go"
    );

    h.key_press(egui::Key::Escape);
    h.step();
    assert!(
        shown(&h).iter().all(Option::is_none),
        "Esc took the ghosts away: {:?}",
        shown(&h)
    );
    assert_eq!(
        h.state().active_tool,
        "align",
        "Esc took one step: the drag, not the tool"
    );

    release_at(&mut h, dot + egui::vec2(10.0, 10.0));
    settle(&mut h);
    assert_eq!(history_len(&h), history, "nothing was aligned");
    let after: Vec<[f64; 12]> = ids
        .iter()
        .map(|&id| h.state().session.document().object(id).unwrap().transform)
        .collect();
    assert_eq!(after, before);
}

/// Owner: "Esc can't cancel the Connect drag". Dragging a placed dowel
/// across the cut face, then Esc with the button still down, puts it
/// back where it was; Connect stays open and the release doesn't move it
/// again.
#[test]
fn esc_cancels_a_connect_drag() {
    require_gpu!("esc_cancels_a_connect_drag");
    let mut h = opened(vec![("ball", ball(30.0), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    let history = history_len(&h);
    h.get_by_label("Cut").click();
    wait_until(&mut h, "the cut", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    h.get_by_label("Connect").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    let p = cap_point(&h, [0.0, 0.0]);
    click_at(&mut h, p);
    settle(&mut h);
    let placed = placements(&h);
    assert_eq!(placed.len(), 1, "{placed:?}");

    let to = cap_point(&h, [12.0, 0.0]);
    press_and_move(&mut h, p, to);
    assert_ne!(
        placements(&h)[0].at,
        placed[0].at,
        "the drag moved the dowel"
    );

    h.key_press(egui::Key::Escape);
    h.step();
    assert_eq!(placements(&h), placed, "Esc put the dowel back");
    assert_eq!(
        h.state().active_tool,
        "connect",
        "Esc took one step: the drag, not the tool"
    );

    release_at(&mut h, to);
    settle(&mut h);
    assert_eq!(placements(&h), placed, "the cancelled drag stays cancelled");
}

/// Undo after Hollow leaves nothing of the hollowed revision in the tool:
/// no preview, pick or result note for it (the panel no longer says the
/// object was hollowed), and the view draws the solid ball again.
#[test]
fn undo_after_hollow_clears_the_preview() {
    require_gpu!("undo_after_hollow_clears_the_preview");
    let mut h = opened(vec![("ball", ball(30.0), at([0.0, 0.0, 30.0]))]);
    let solid = mesh_of(&h, "ball").content_hash();
    h.get_by_label("Hollow").click();
    settle(&mut h);
    let id = id_of(&h, "ball");
    wait_until(&mut h, "the hollow preview", |a| {
        mf_desktop::hollow_tool::preview_render(a, id).is_some()
    });
    settle(&mut h);
    let history = history_len(&h);
    panel_button(&h, "Hollow").click();
    wait_until(&mut h, "the hollow", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert!(
        h.state().hollow.last_result.is_some(),
        "the panel says it hollowed"
    );
    let revisions = h.state().revisions_seen("hollow");

    h.get_by_label("Undo").click();
    wait_until(&mut h, "the undo", |a| {
        a.busy.is_none() && mesh_of_state(a, id).is_some_and(|m| m.content_hash() == solid)
    });
    settle(&mut h);
    assert!(
        h.state().revisions_seen("hollow") > revisions,
        "Hollow heard about the undo"
    );
    assert!(
        h.state().hollow.last_result.is_none(),
        "no result note for the undone hollow: {:?}",
        h.state().hollow.last_result
    );
    assert_eq!(h.state().tool_stale("hollow"), Vec::<&str>::new());
    // Whatever preview shows now is of the solid ball on screen, never
    // of the shell the undo took away.
    assert!(
        mf_desktop::hollow_tool::preview_asked_for_current(h.state()),
        "any preview asked for is of the revision on screen"
    );
}

fn mesh_of_state(a: &MeshFixerApp, id: mf_command::ObjectId) -> Option<&Mesh> {
    a.session.document().object(id)?.mesh()
}
