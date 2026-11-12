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
//! param edit and its Undo.
//!
//! Run with `scripts/core-workflows.sh` (kept under 3 minutes at `-j 4`).

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

fn opened(objects: Vec<(&str, Mesh, [f64; 12])>) -> Harness<'static, MeshFixerApp> {
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| {
            MeshFixerApp::with_settings(cc, None, Settings::default()).unwrap()
        });
    h.state_mut().import_model(ImportedModel {
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
            path: "core-workflows.3mf".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
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
    h.get_by_label("Cut").click();
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
        let mesh = h.state().session.document().object(id).unwrap().mesh().unwrap().clone();
        let topo = mesh
            .topology_with(&mf_runtime::RunContext::none())
            .unwrap();
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
        let mesh = h.state().session.document().object(id).unwrap().mesh().unwrap().clone();
        let topo = mesh
            .topology_with(&mf_runtime::RunContext::none())
            .unwrap();
        assert!(topo.boundary_edges > 0, "{name}: left open at the cut");
    }
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

/// Scrolls the tool panel until `label` is inside the window, then
/// clicks it (Connect's panel is taller than the window).
fn scroll_and_click(h: &mut Harness<'_, MeshFixerApp>, label: &str) {
    let over = egui::pos2(200.0, WINDOW.1 * 0.5);
    h.event(egui::Event::PointerMoved(over));
    h.step();
    let mut tries = 0;
    loop {
        let rect = h.get_by_label(label).rect();
        if rect.bottom() <= WINDOW.1 - 4.0 && rect.top() >= 40.0 {
            break;
        }
        tries += 1;
        assert!(tries < 400, "scrolled to {label:?}");
        let down = rect.bottom() > WINDOW.1 - 4.0;
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
    h.state_mut().sculpt.radius = 6.0;

    // A dab well away from the cap, on the curved surface (the ball's
    // equator, facing the camera).
    let dab = facing_point(&h, [0.0, 0.0, 20.0], 20.0);
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
