#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The Connect tool (owner 2026-11-11): its own rail entry, next to
//! Split, for adding connectors to a cut joint — driven like a user:
//! pick a joint (the list, or its cut face in the view), choose the
//! see-through piece, click to place, drag, resize, set depth, the
//! checks and their one-click fixes, Delete, Apply, reopen. Split's own
//! cutting workflow is in `split_tool.rs`; this file opens Split only
//! long enough to make the joints Connect needs.
//!
//! With `MF_ARTIFACTS` set, a frame of every step goes to
//! `<dir>/design/connect-<step>-<w>x<h>.png`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use glam::DVec3;
use mf_desktop::connect_tool::CONNECT;
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::{ContentHash, Mesh};
use mf_io::threemf::{ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_operations::{JointSide, check_id};

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

fn still(h: &mut Harness<'_, MeshFixerApp>, scene: &str) {
    let Some(dir) = std::env::var_os("MF_ARTIFACTS") else {
        return;
    };
    let dir = PathBuf::from(dir).join("design");
    std::fs::create_dir_all(&dir).unwrap();
    let size = h.ctx.content_rect().size();
    let name = format!("connect-{scene}-{}x{}.png", size.x as u32, size.y as u32);
    h.render().unwrap().save(dir.join(name)).unwrap();
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
            path: "connect.3mf".into(),
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

/// A closed 60 mm ball.
fn ball() -> Mesh {
    mf_fixtures::scaled(&mf_fixtures::uv_sphere(48, 96), 30.0)
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

/// Opens a ball, Splits it with one slice (off the seam of vertices) so
/// there's exactly one joint, and returns the harness with Split closed
/// again (Connect isn't open yet).
fn ball_cut_in_two() -> Harness<'static, MeshFixerApp> {
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    let history = h.state().session.history_labels().len();
    h.get_by_label("Cut").click();
    wait_until(&mut h, "the cut", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(h.state().split_flow.joints.len(), 1, "the cut is a joint");
    h
}

/// How much of the window's bottom edge the status bar covers (its own
/// text row): a node whose rect's bottom falls within this band is
/// visually hidden behind it even though it's within the window's own
/// pixel bounds, so [`scroll_and_click`] keeps scrolling past it rather
/// than treating it as "on screen" and clicking through the status bar
/// onto whatever's really underneath (`core_workflows.rs`'s own copy of
/// this helper has the identical fix, for the identical reason).
const STATUS_BAR_HEIGHT: f32 = 36.0;

/// Scrolls the tool panel until `label` is inside the window (clear of
/// the status bar at the bottom), then clicks it (Connect's panel is
/// taller than the window).
fn scroll_and_click(h: &mut Harness<'_, MeshFixerApp>, label: &str) {
    scroll_and_click_by(h, label, None);
}

/// [`scroll_and_click`] on the control with `role` and `label` (a schema
/// field's control shares its name with the field's text label).
fn scroll_and_click_role(
    h: &mut Harness<'_, MeshFixerApp>,
    role: egui::accesskit::Role,
    label: &str,
) {
    scroll_and_click_by(h, label, Some(role));
}

fn scroll_and_click_by(
    h: &mut Harness<'_, MeshFixerApp>,
    label: &str,
    role: Option<egui::accesskit::Role>,
) {
    let rect_of = |h: &Harness<'_, MeshFixerApp>| match role {
        Some(r) => h.get_by_role_and_label(r, label).rect(),
        None => h.get_by_label(label).rect(),
    };
    let over = egui::pos2(200.0, WINDOW.1 * 0.5);
    h.event(egui::Event::PointerMoved(over));
    h.step();
    let mut tries = 0;
    loop {
        let rect = rect_of(h);
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
    match role {
        Some(r) => h.get_by_role_and_label(r, label).click(),
        None => h.get_by_label(label).click(),
    }
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
fn drag(h: &mut Harness<'_, MeshFixerApp>, from: egui::Pos2, to: egui::Pos2) {
    h.event(egui::Event::PointerMoved(from));
    h.step();
    h.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    for k in 1..=10 {
        let p = from + (to - from) * (k as f32 / 10.0);
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

/// Where the open joint's solid cut face point `uv` shows in the window.
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

/// With no joints, Connect says so and offers a button that opens
/// Split.
#[test]
fn with_no_joints_connect_points_to_split() {
    require_gpu!("with_no_joints_connect_points_to_split");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Connect").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, CONNECT);
    h.get_by_label("Split a model first to create joints");
    still(&mut h, "no-joints");

    h.get_by_label("Open Split").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, mf_desktop::shape_tools::SPLIT);
}

/// Defaults to a Sliding fit (positive clearance), not Snug at 0 mm
/// (owner 2026-11-11).
#[test]
fn defaults_to_sliding_fit() {
    require_gpu!("defaults_to_sliding_fit");
    let mut h = ball_cut_in_two();
    h.get_by_label("Connect").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    assert_eq!(h.state().split_flow.fit_class(), "Sliding");
    let sliding = mf_parts::FitClass::builtins()
        .into_iter()
        .find(|c| c.name == "Sliding")
        .unwrap();
    assert!(sliding.clearance > 0.0, "{:?}", sliding);
}

/// Placed by clicking on the solid cut face, dragged, checked (a
/// breakout and its fix), deleted, applied in one command, and the
/// joint reopened and applied again.
#[test]
fn place_drag_check_and_apply_on_the_joint() {
    require_gpu!("place_drag_check_and_apply_on_the_joint");
    let mut h = ball_cut_in_two();

    // Connect: the joint opens, the top piece see-through, the camera
    // turned to the bottom piece's cut face.
    h.get_by_label("Connect").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    let (a_id, b_id) = {
        let j = h.state().split_flow.open_joint().unwrap();
        (j.a, j.b)
    };
    assert_eq!(
        h.state().split_flow.open_joint().unwrap().transparent,
        JointSide::B
    );
    assert!(mf_desktop::split_flow::is_ghost(h.state(), b_id));
    assert!(!mf_desktop::split_flow::is_ghost(h.state(), a_id));
    let cam = h.state().camera.unwrap();
    assert!(
        cam.view_dir().z > 0.85,
        "looking down onto the bottom piece's cut face: {:?}",
        cam.view_dir()
    );
    still(&mut h, "joint");

    // Click to place: a dowel, then a peg, then a magnet.
    let p = cap_point(&h, [-12.0, 0.0]);
    click_at(&mut h, p);
    settle(&mut h);
    assert_eq!(
        placements(&h).len(),
        1,
        "a click on the cut face places a connector"
    );
    assert_eq!(placements(&h)[0].connector, "joint.dowel");
    assert_eq!(placements(&h)[0].on, JointSide::A, "on the solid piece");
    h.get_by_label("Peg").click();
    settle(&mut h);
    let p = cap_point(&h, [12.0, 0.0]);
    click_at(&mut h, p);
    h.get_by_label("Magnet").click();
    settle(&mut h);
    let p = cap_point(&h, [0.0, 14.0]);
    click_at(&mut h, p);
    settle(&mut h);
    let ps = placements(&h);
    assert_eq!(
        ps.iter().map(|p| p.connector.as_str()).collect::<Vec<_>>(),
        ["joint.dowel", "joint.peg", "joint.magnet"]
    );
    assert!(
        (ps[0].at[0] + 12.0).abs() < 0.5 && ps[0].at[1].abs() < 0.5,
        "{:?}",
        ps[0].at
    );
    still(&mut h, "placed");

    // Drag the dowel to a new spot: its counterpart follows (drawn from
    // the same placement).
    let (from, to) = (cap_point(&h, [-12.0, 0.0]), cap_point(&h, [-10.0, -8.0]));
    drag(&mut h, from, to);
    settle(&mut h);
    let moved = placements(&h)[0].at;
    assert!(
        (moved[0] + 10.0).abs() < 0.5 && (moved[1] + 8.0).abs() < 0.5,
        "dragged to {moved:?}"
    );
    assert_eq!(h.state().split_flow.selected, Some(0));

    // A dowel hole far too deep breaks out of the bottom: the warning,
    // and its one-click fix.
    {
        let a = h.state_mut();
        let j = a.split_flow.joint.unwrap();
        a.split_flow.joints[j].placements[0]
            .params
            .insert("depth_on".into(), 45.0);
    }
    settle(&mut h);
    let checks = h.state().split_flow.checks().unwrap();
    assert!(
        checks[0].iter().any(|c| c.id == check_id::BREAKOUT_A),
        "{checks:?}"
    );
    still(&mut h, "breakout");
    scroll_and_click(&mut h, "Make it shallower");
    settle(&mut h);
    let checks = h.state().split_flow.checks().unwrap();
    assert!(checks[0].is_empty(), "the fix cleared it: {checks:?}");
    assert!(placements(&h)[0].params["depth_on"] < 45.0);

    // Delete the magnet.
    {
        let a = h.state_mut();
        a.split_flow.selected = Some(2);
    }
    h.event(egui::Event::PointerMoved(egui::pos2(
        WINDOW.0 * 0.6,
        WINDOW.1 * 0.5,
    )));
    h.step();
    h.key_press(egui::Key::Delete);
    settle(&mut h);
    assert_eq!(
        placements(&h).len(),
        2,
        "Delete removes the picked connector"
    );
    assert_eq!(objects(&h).len(), 2, "and no object");

    // Apply: one command; the dowel is its own object on the plate.
    let history = h.state().session.history_labels().len();
    scroll_and_click(&mut h, "Add connectors");
    wait_until(&mut h, "the connectors", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(
        h.state().session.history_labels().len(),
        history + 1,
        "one undo step"
    );
    let names = objects(&h);
    assert!(
        names.contains(&"ball (bottom) dowel 1".to_owned()),
        "{names:?}"
    );
    still(&mut h, "applied");

    // Reopened: change the peg and apply again, from before the first
    // connectors (the joint's pieces, not the holed ones, are checked).
    h.get_by_label("Update connectors");
    {
        let a = h.state_mut();
        let j = a.split_flow.joint.unwrap();
        a.split_flow.joints[j].placements[1]
            .params
            .insert("diameter".into(), 6.0);
    }
    settle(&mut h);
    let checks = h.state().split_flow.checks().unwrap();
    assert!(checks.iter().all(Vec::is_empty), "{checks:?}");
    scroll_and_click(&mut h, "Update connectors");
    wait_until(&mut h, "the update", |a| a.busy.is_none());
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let names = objects(&h);
    assert_eq!(
        names.iter().filter(|n| n.contains("dowel")).count(),
        1,
        "the update replaces the first connectors: {names:?}"
    );
    still(&mut h, "updated");
}

/// Picking another joint's cut face in the view opens it (Print volume
/// makes several joints at once).
#[test]
fn clicking_another_joints_cut_face_opens_it() {
    require_gpu!("clicking_another_joints_cut_face_opens_it");
    let mut h = opened(vec![("scan", ball(), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.get_by_label("Print volume").click();
    settle(&mut h);
    {
        let a = h.state_mut();
        a.split_flow.volume.size = DVec3::new(40.3, 37.9, 44.1);
        a.split_flow.volume.center = DVec3::new(0.7, -0.4, 24.35);
    }
    settle(&mut h);
    let v = h.state().split_flow.volume.clone();
    let history = h.state().session.history_labels().len();
    h.get_by_label("Cut").click();
    wait_until(&mut h, "the cut", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert_eq!(h.state().split_flow.joints.len(), 6);

    // The link opens Connect on the first joint from this cut.
    h.get_by_label("Add connectors to these joints ->").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    still(&mut h, "volume-joint");
    assert_eq!(
        h.state().split_flow.joint,
        Some(0),
        "the -X face's joint is open"
    );
    let y = v.min().y;
    let p = h.state().screen_point([10.0, y, 25.0]).unwrap();
    click_at(&mut h, p);
    settle(&mut h);
    assert_eq!(
        h.state().split_flow.joint,
        Some(2),
        "the -Y face's joint opened"
    );
    assert!(
        placements(&h).is_empty(),
        "a click on another joint's face places nothing"
    );
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    still(&mut h, "volume-other-joint");
}

/// I1 (owner 2026-11-11): Delete and Backspace, while Connect is open,
/// remove only the picked connector — never the active object, whether
/// or not a connector is picked.
#[test]
fn delete_never_falls_through_to_the_active_object() {
    require_gpu!("delete_never_falls_through_to_the_active_object");
    let mut h = ball_cut_in_two();
    h.get_by_label("Connect").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    let p = cap_point(&h, [-12.0, 0.0]);
    click_at(&mut h, p);
    settle(&mut h);
    assert_eq!(placements(&h).len(), 1);
    assert_eq!(objects(&h).len(), 2, "the ball's two pieces, no more");

    // With nothing picked, Delete does nothing (I1): not the connector,
    // not the object.
    {
        let a = h.state_mut();
        a.split_flow.selected = None;
    }
    h.event(egui::Event::PointerMoved(egui::pos2(
        WINDOW.0 * 0.6,
        WINDOW.1 * 0.5,
    )));
    h.step();
    h.key_press(egui::Key::Delete);
    settle(&mut h);
    assert_eq!(
        placements(&h).len(),
        1,
        "nothing picked: Delete removes nothing"
    );
    assert_eq!(objects(&h).len(), 2, "and not the object either");

    // Picked: Delete removes the connector, never the object (the
    // original bug: the global object.delete ran instead).
    {
        let a = h.state_mut();
        a.split_flow.selected = Some(0);
    }
    settle(&mut h);
    h.key_press(egui::Key::Delete);
    settle(&mut h);
    assert_eq!(placements(&h).len(), 0, "Delete removed the connector");
    assert_eq!(
        objects(&h).len(),
        2,
        "the object was never touched: {:?}",
        objects(&h)
    );
    assert!(
        h.state().active.is_some(),
        "nothing was deleted from the tree"
    );
}

/// I2 (owner 2026-11-11): each row under "Connectors on this joint" has
/// its own trash button, and the selected connector's details block has
/// a "Remove connector" button; both remove just that connector.
#[test]
fn rows_and_the_details_block_have_their_own_remove_button() {
    require_gpu!("rows_and_the_details_block_have_their_own_remove_button");
    let mut h = ball_cut_in_two();
    h.get_by_label("Connect").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    let p1 = cap_point(&h, [-12.0, 0.0]);
    click_at(&mut h, p1);
    settle(&mut h);
    h.get_by_label("Peg").click();
    settle(&mut h);
    let p2 = cap_point(&h, [12.0, 0.0]);
    click_at(&mut h, p2);
    settle(&mut h);
    assert_eq!(placements(&h).len(), 2);

    // The details block's "Remove connector" removes the selected one
    // (connector 2, the peg, is selected after placing it).
    assert_eq!(h.state().split_flow.selected, Some(1));
    scroll_and_click(&mut h, "Remove connector");
    settle(&mut h);
    assert_eq!(
        placements(&h).len(),
        1,
        "the details block's button removed it"
    );
    assert_eq!(placements(&h)[0].connector, "joint.dowel");

    // Row 1's own trash button removes it without selecting it first.
    assert_eq!(h.state().split_flow.selected, None);
    scroll_and_click(&mut h, "Remove connector 1");
    settle(&mut h);
    assert_eq!(placements(&h).len(), 0, "the row's trash button removed it");
}

/// I3 (owner 2026-11-11): a "N warnings" row expands on click to show
/// each warning in plain words with its one-click fix, and collapses
/// again on a second click.
#[test]
fn a_warning_row_expands_on_click_to_show_its_fix() {
    require_gpu!("a_warning_row_expands_on_click_to_show_its_fix");
    let mut h = ball_cut_in_two();
    h.get_by_label("Connect").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    let p1 = cap_point(&h, [-12.0, 0.0]);
    click_at(&mut h, p1);
    settle(&mut h);
    {
        let a = h.state_mut();
        let j = a.split_flow.joint.unwrap();
        a.split_flow.joints[j].placements[0]
            .params
            .insert("depth_on".into(), 45.0);
    }
    settle(&mut h);
    assert!(
        h.state().split_flow.checks().unwrap()[0]
            .iter()
            .any(|c| c.id == check_id::BREAKOUT_A),
        "a breakout warning is live"
    );

    // Shown by default (I3 keeps today's always-visible behaviour as
    // the open state): the fix button is already there.
    assert!(
        !h.state().split_flow.collapsed_warnings.contains(&0),
        "warnings open by default"
    );
    scroll_and_click(&mut h, "1. Dowel: 1 warning");
    settle(&mut h);
    assert!(
        h.state().split_flow.collapsed_warnings.contains(&0),
        "a click on the row collapses its warnings"
    );

    // Click again: it reopens, and the fix still works.
    scroll_and_click(&mut h, "1. Dowel: 1 warning");
    settle(&mut h);
    assert!(!h.state().split_flow.collapsed_warnings.contains(&0));
    scroll_and_click(&mut h, "Make it shallower");
    settle(&mut h);
    let checks = h.state().split_flow.checks().unwrap();
    assert!(checks[0].is_empty(), "the fix cleared it: {checks:?}");
}

/// I4 (owner 2026-11-11): a connector's counterpart is drawn flattened
/// onto the solid cap (`flatten_on_cap`), so it lands at the same 2D
/// place in the joint frame as the connector's own footprint — no
/// perspective offset between the two, coincident when viewed along the
/// joint normal.
#[test]
fn the_counterpart_is_flattened_onto_the_same_place_as_the_footprint() {
    require_gpu!("the_counterpart_is_flattened_onto_the_same_place_as_the_footprint");
    let mut h = ball_cut_in_two();
    h.get_by_label("Connect").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    h.get_by_label("Magnet").click();
    settle(&mut h);
    let pm = cap_point(&h, [9.0, -6.0]);
    click_at(&mut h, pm);
    settle(&mut h);
    let p = placements(&h)[0].clone();
    assert_eq!(p.connector, "joint.magnet");

    let a = h.state();
    let joint = a.split_flow.open_joint().unwrap().clone();
    let scale = mf_desktop::split_flow::mm(a);
    let parts = mf_operations::placed_parts(&joint.frame, &p, scale).unwrap();
    let solid = joint.solid();
    let counterpart = parts
        .iter()
        .find(|part| part.side.is_some_and(|s| s != solid))
        .expect("the magnet has a counterpart pocket");
    // The counterpart's true depth differs from the solid cap's (it's
    // behind the *other* cap): the bug this guards against is real here,
    // not vacuously fixed.
    let depths: Vec<f64> = counterpart
        .world
        .vertices()
        .map(|(_, vp)| (DVec3::from(vp) - joint.frame.cap_point(solid)).dot(joint.frame.normal))
        .collect();
    assert!(
        depths.iter().any(|d| d.abs() > 1.0),
        "the counterpart's true depth is away from the solid cap: {depths:?}"
    );
    // Every vertex of the counterpart part, flattened onto the solid
    // cap, lands at the placement's own 2D place (I4): before the fix,
    // its true depth put it elsewhere once drawn, offset from its own
    // footprint by perspective.
    for (_, vp) in counterpart.world.vertices() {
        let v = DVec3::from(vp);
        let flat = mf_desktop::split_flow::flatten_on_cap(&joint.frame, solid, v);
        let uv = joint.frame.to_2d(flat);
        let r = ((uv[0] - p.at[0]).powi(2) + (uv[1] - p.at[1]).powi(2)).sqrt();
        assert!(
            r < 10.0,
            "a flattened counterpart vertex at {uv:?} is {r} mm from the \
             footprint centre {:?}",
            p.at
        );
    }
}

/// Owner decision 2026-11-12: the dovetail moved out of Connect's point
/// placement into the parametric shape system
/// (`mf_parts::connectors::dovetail_slide`, `dovetail_tool.rs`), so its
/// kind is no longer offered in the picker a click places from — only
/// the Dovetail *shortcut* button, which drops the whole linked pair at
/// once rather than letting a click pick "Dovetail" first (that flow is
/// `core_workflows.rs`'s own
/// `dovetail_placed_rotated_and_applied_exits_the_piece_and_fits_the_groove`).
#[test]
fn the_kind_picker_no_longer_offers_dovetail_but_the_shortcut_button_does() {
    require_gpu!("the_kind_picker_no_longer_offers_dovetail_but_the_shortcut_button_does");
    let mut h = ball_cut_in_two();
    h.get_by_label("Connect").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    assert_eq!(
        h.get_all_by_label("Dovetail").count(),
        1,
        "only the shortcut button says Dovetail; the kind picker's segmented control doesn't"
    );
    h.get_by_label("Dovetail").click();
    settle(&mut h);
    let names = objects(&h);
    assert!(
        names
            .iter()
            .any(|n| n.contains("Dovetail") && n.contains("tail"))
            && names
                .iter()
                .any(|n| n.contains("Dovetail") && n.contains("groove")),
        "the shortcut placed a linked tail/groove pair: {names:?}"
    );
    still(&mut h, "dovetail-shortcut");
}

/// A joint whose placements already include a legacy `joint.dovetail`
/// (from before the owner's 2026-11-12 decision, or a log replayed from
/// then) still shows, edits and replays it unchanged: the details
/// block's reach/through fields keep working on an already-placed
/// dovetail even though a new one can no longer be started from the
/// kind picker.
#[test]
fn a_legacy_dovetail_placement_still_edits_its_reach_and_open_state() {
    require_gpu!("a_legacy_dovetail_placement_still_edits_its_reach_and_open_state");
    let mut h = ball_cut_in_two();
    h.get_by_label("Connect").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    // Seed a legacy dovetail placement directly (as an old save or a
    // replayed log would already have one in `joint.placements`, never
    // through the removed click-to-place UI).
    {
        let a = h.state_mut();
        let j = a.split_flow.joint.unwrap_or(0);
        let on = a.split_flow.joints[j].transparent.other();
        a.split_flow.joints[j]
            .placements
            .push(mf_operations::Placement {
                connector: "joint.dovetail".to_owned(),
                on,
                at: [0.0, 0.0],
                angle: 0.0,
                params: std::collections::BTreeMap::from([
                    ("groove_a".to_owned(), 20.0),
                    ("groove_b".to_owned(), 20.0),
                    ("open_a".to_owned(), 1.0),
                    ("open_b".to_owned(), 1.0),
                ]),
            });
        a.split_flow.selected = Some(0);
    }
    settle(&mut h);
    assert_eq!(placements(&h).len(), 1);
    assert_eq!(placements(&h)[0].connector, "joint.dovetail");
    assert_eq!(
        placements(&h)[0].params["open_a"],
        1.0,
        "through, as seeded"
    );

    // Unchecking the start end's "Open a" box (built from the schema's
    // own bool) makes it stopped.
    scroll_and_click_role(&mut h, egui::accesskit::Role::CheckBox, "Open a");
    settle(&mut h);
    let ps = placements(&h);
    assert_eq!(ps[0].params["open_a"], 0.0, "unchecked: now stopped");
    still(&mut h, "dovetail-stopped");
}

/// D22 (owner 2026-11-11): a dowel's shape picker in the details block
/// switches its cross-section, recorded in the placement so replay is
/// Exact (the generic schema field: a choice is a combo box of its
/// words).
#[test]
fn the_dowel_shape_picker_switches_its_cross_section() {
    require_gpu!("the_dowel_shape_picker_switches_its_cross_section");
    let mut h = ball_cut_in_two();
    h.get_by_label("Connect").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    let p = cap_point(&h, [-12.0, 0.0]);
    click_at(&mut h, p);
    settle(&mut h);
    assert_eq!(placements(&h)[0].connector, "joint.dowel");
    assert!(
        !placements(&h)[0].params.contains_key("shape"),
        "round by default: no shape param set yet"
    );
    still(&mut h, "dowel-shape-round");

    // The schema's own choice: a combo box of its words.
    scroll_and_click_role(&mut h, egui::accesskit::Role::ComboBox, "Shape");
    settle(&mut h);
    h.get_by_label("Square").click();
    settle(&mut h);
    assert_eq!(placements(&h)[0].params["shape"], 1.0);
    still(&mut h, "dowel-shape-square");

    scroll_and_click_role(&mut h, egui::accesskit::Role::ComboBox, "Shape");
    settle(&mut h);
    h.get_by_label("N sided").click();
    settle(&mut h);
    assert_eq!(placements(&h)[0].params["shape"], 2.0);
    // Its "Sides" field.
    h.get_by_label("Sides");
    still(&mut h, "dowel-shape-nsided");
}

/// D22: a joint with a single round dowel warns that the parts can
/// rotate about it, above the connector list; "Make square" fixes it in
/// place and the warning clears.
#[test]
fn a_single_round_dowel_warns_about_rotation_in_the_joint_summary() {
    require_gpu!("a_single_round_dowel_warns_about_rotation_in_the_joint_summary");
    let mut h = ball_cut_in_two();
    h.get_by_label("Connect").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    let p = cap_point(&h, [-12.0, 0.0]);
    click_at(&mut h, p);
    settle(&mut h);
    assert_eq!(placements(&h).len(), 1);
    assert_eq!(placements(&h)[0].connector, "joint.dowel");

    h.get_by_label(
        "The parts can rotate around one round dowel. Add a second, or use a square \
                    or keyed shape",
    );
    still(&mut h, "rotation-warning");

    scroll_and_click(&mut h, "Make square");
    settle(&mut h);
    assert_eq!(placements(&h)[0].params["shape"], 1.0);
    assert!(
        h.query_by_label(
            "The parts can rotate around one round dowel. Add a second, or use a square or \
             keyed shape"
        )
        .is_none(),
        "the fix cleared the warning"
    );
    still(&mut h, "rotation-fixed");
}

fn group_of(h: &Harness<'_, MeshFixerApp>, id: mf_command::ObjectId) -> Option<String> {
    h.state()
        .session
        .document()
        .object(id)?
        .properties
        .get(mf_command::GROUP)
        .cloned()
}

fn size_x(h: &Harness<'_, MeshFixerApp>, id: mf_command::ObjectId) -> f64 {
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

/// Connect as a placement helper (parametric-shapes brief): "Add as
/// editable shapes" makes a linked pair (the peg on the solid piece,
/// its hole on the other, aligned), editing one regenerates both in one
/// step, and Attach joins each onto its own piece.
#[test]
fn connect_creates_a_linked_pair_that_edits_and_attaches_together() {
    require_gpu!("connect_creates_a_linked_pair_that_edits_and_attaches_together");
    let mut h = ball_cut_in_two();
    h.get_by_label("Connect").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    let (a_id, b_id) = {
        let j = h.state().split_flow.open_joint().unwrap();
        (j.a, j.b)
    };
    h.get_by_label("Peg").click();
    settle(&mut h);
    let p = cap_point(&h, [0.0, 0.0]);
    click_at(&mut h, p);
    settle(&mut h);
    assert_eq!(placements(&h).len(), 1);
    let before: Vec<mf_command::ObjectId> = h
        .state()
        .session
        .document()
        .objects()
        .iter()
        .map(|o| o.id)
        .collect();
    let history = h.state().session.history_labels().len();
    scroll_and_click(&mut h, "Add as editable shapes");
    wait_until(&mut h, "the linked pair", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    let doc = h.state().session.document().clone();
    let pair: Vec<&mf_command::Object> = doc
        .objects()
        .iter()
        .filter(|o| !before.contains(&o.id))
        .collect();
    assert_eq!(pair.len(), 2, "a peg and its hole");
    let peg = pair.iter().find(|o| !o.is_hole()).unwrap();
    let hole = pair.iter().find(|o| o.is_hole()).unwrap();
    assert!(peg.is_parametric() && hole.is_parametric());
    assert_eq!(
        group_of(&h, peg.id),
        group_of(&h, hole.id),
        "one linked group"
    );
    let to = |o: &mf_command::Object| o.properties[mf_operations::shapes::ATTACH_TO].clone();
    assert_eq!(to(peg), a_id.0.to_string(), "the peg joins the solid piece");
    assert_eq!(to(hole), b_id.0.to_string(), "its hole is cut in the other");
    assert_eq!(peg.transform, hole.transform, "aligned across the cut");
    let (peg, hole) = (peg.id, hole.id);
    still(&mut h, "as-shapes");

    // Editing one member regenerates both, as one step.
    h.get_by_label("Select").click();
    settle(&mut h);
    h.state_mut().set_active(peg);
    settle(&mut h);
    let widths = (size_x(&h, peg), size_x(&h, hole));
    let history = h.state().session.history_labels().len();
    h.state_mut().set_active(peg);
    mf_desktop::shape_panel::set_param(h.state_mut(), "diameter", mf_spec::ParamValue::Float(8.0));
    wait_until(&mut h, "the pair regenerated", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    assert_eq!(h.state().session.history_labels().len(), history + 1);
    assert!(size_x(&h, peg) > widths.0 + 2.0, "the peg grew");
    assert!(size_x(&h, hole) > widths.1 + 2.0, "and so did its hole");
    still(&mut h, "pair-edited");

    // Attach: the peg joins its piece, the hole is cut from the other.
    let history = h.state().session.history_labels().len();
    let label = {
        let d = h.state().session.document();
        format!(
            "Attach to {} and {}",
            d.object(a_id).unwrap().name,
            d.object(b_id).unwrap().name
        )
    };
    h.state_mut().set_active(peg);
    settle(&mut h);
    assert!(
        mf_desktop::shape_panel::attach_sides(h.state(), peg).is_some(),
        "the pair knows its two pieces"
    );
    assert!(
        h.query_by_label_contains("Editable shape").is_some(),
        "the shape section shows"
    );
    h.get_by_label(label.as_str()).click();
    wait_until(&mut h, "the attach", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let doc = h.state().session.document();
    assert!(doc.object(peg).is_none() && doc.object(hole).is_none());
    assert!(doc.object(a_id).is_some() && doc.object(b_id).is_some());
    still(&mut h, "pair-attached");
}
