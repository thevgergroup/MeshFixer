#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The Split tool's three-step workflow (owner 2026-11-11), driven like a
//! user: 1 Define cut (Slice, or Print volume with its box and face
//! handles), 2 Cut, 3 Connectors (pick a joint, see-through piece, click
//! to place, drag, the checks and their fixes, Delete, Apply, reopen).
//!
//! With `MF_ARTIFACTS` set, a frame of every step goes to
//! `<dir>/design/split-<step>-<w>x<h>.png`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use glam::DVec3;
use mf_desktop::split_flow::{CutMode, Step};
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::{ContentHash, Mesh};
use mf_io::threemf::{ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_operations::{CheckKind, JointSide};

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
    let name = format!("split-{scene}-{}x{}.png", size.x as u32, size.y as u32);
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
            path: "split.3mf".into(),
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

/// Scrolls the tool panel until `label` is inside the window, then
/// clicks it (the step 3 panel is taller than the window).
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

/// Slice, cut, then connectors on the joint: placed by clicking on the
/// solid cut face, dragged, checked (a breakout and its fix), deleted,
/// applied in one command, and the joint reopened and applied again.
#[test]
fn slice_cut_and_connectors_on_the_joint() {
    require_gpu!("slice_cut_and_connectors_on_the_joint");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    assert_eq!(h.state().split_flow.step, Step::Define);
    assert_eq!(h.state().split_flow.mode, CutMode::Slice);
    // Off the ball's seam of vertices.
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    still(&mut h, "step1-slice");

    // 2 Cut.
    h.get_by_label("2 Cut").click();
    settle(&mut h);
    still(&mut h, "step2-cut");
    let history = h.state().session.history_labels().len();
    h.get_by_label("Cut in two").click();
    wait_until(&mut h, "the cut", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(
        h.state().session.history_labels().len(),
        history + 1,
        "one undo step"
    );
    assert_eq!(h.state().split_flow.joints.len(), 1, "the cut is a joint");

    // 3 Connectors: the joint opens, the top piece see-through, the
    // camera turned to the bottom piece's cut face.
    h.get_by_label("Next: Connectors").click();
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
    still(&mut h, "step3-joint");

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
    still(&mut h, "step3-placed");

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
        checks[0]
            .iter()
            .any(|c| c.kind == CheckKind::Breakout(JointSide::A)),
        "{checks:?}"
    );
    still(&mut h, "step3-breakout");
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
    // Back on the joint, its connectors still there to edit.
    h.get_by_label("3 Connectors").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    still(&mut h, "step3-applied");

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
    still(&mut h, "step3-updated");
}

/// Print volume: the box from a printer preset, resized by a face
/// handle, the pieces listed, cut along its faces into pieces that fit
/// it, every cut a joint.
#[test]
fn print_volume_box_cuts_into_pieces_that_fit() {
    require_gpu!("print_volume_box_cuts_into_pieces_that_fit");
    let mut h = opened(vec![("scan", ball(), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.get_by_label("Print volume").click();
    settle(&mut h);
    assert_eq!(h.state().split_flow.mode, CutMode::Volume);
    // The default box is a printer's volume, centred on the ball, on the
    // plate: the 60 mm ball fits, so nothing would be cut.
    let v = h.state().split_flow.volume.clone();
    assert!(v.size.x >= 100.0, "{:?}", v.size);
    assert!(
        v.min().z.abs() < 1e-9,
        "resting on the plate: {:?}",
        v.min()
    );
    h.get_by_label("scan already fits in this box, so there's nothing to cut. Make the box smaller or move it.");
    still(&mut h, "step1-volume-preset");
    // A small printer: type the size.
    {
        let a = h.state_mut();
        a.split_flow.volume.size = DVec3::new(40.3, 37.9, 44.1);
        a.split_flow.volume.center = DVec3::new(0.7, -0.4, 24.35);
    }
    settle(&mut h);
    h.get_by_label("In the box: about 40 × 38 × 44");
    // Drag the +X face's handle outwards: the box grows along X only.
    let before = h.state().split_flow.volume.clone();
    let face = before.center + DVec3::new(before.size.x / 2.0, 0.0, 0.0);
    let from = h.state().screen_point(face.to_array()).unwrap();
    let further = h
        .state()
        .screen_point((face + DVec3::new(6.0, 0.0, 0.0)).to_array())
        .unwrap();
    drag(&mut h, from, further);
    settle(&mut h);
    let after = h.state().split_flow.volume.clone();
    assert!(
        after.size.x > before.size.x + 2.0,
        "{:?} -> {:?}",
        before.size,
        after.size
    );
    assert!(
        (after.min().x - before.min().x).abs() < 1e-9,
        "the -X face stays"
    );
    assert!((after.size.y - before.size.y).abs() < 1e-9);
    still(&mut h, "step1-volume");

    h.get_by_label("2 Cut").click();
    settle(&mut h);
    still(&mut h, "step2-volume");
    let v = h.state().split_flow.volume.clone();
    let history = h.state().session.history_labels().len();
    h.get_by_label("Cut to the print volume").click();
    wait_until(&mut h, "the cut", |a| {
        a.busy.is_none() && (a.session.history_labels().len() > history || a.refusal.is_some())
    });
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    assert_eq!(h.state().session.history_labels().len(), history + 1);
    let names = objects(&h);
    assert!(names.contains(&"scan (in box)".to_owned()), "{names:?}");
    // Every face crosses the ball: six outside pieces, six joints.
    assert_eq!(names.len(), 7, "{names:?}");
    assert_eq!(h.state().split_flow.joints.len(), 6);
    // The inside piece fits the box, and the box stays where it cut.
    let after_cut = h.state().split_flow.volume.clone();
    assert_eq!((after_cut.center, after_cut.size), (v.center, v.size));
    let doc = h.state().session.document();
    let inside = doc
        .objects()
        .iter()
        .find(|o| o.name == "scan (in box)")
        .unwrap();
    let t = inside.transform;
    let mesh = inside.mesh().unwrap();
    for (_, c) in mesh.faces() {
        for corner in c {
            let p = mesh.corner_position(corner);
            let w = DVec3::new(p[0] + t[9], p[1] + t[10], p[2] + t[11]);
            assert!(
                w.cmpge(v.min() - DVec3::splat(1e-6)).all()
                    && w.cmple(v.max() + DVec3::splat(1e-6)).all(),
                "{w} outside the box"
            );
        }
    }
    h.get_by_label("Next: Connectors").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    still(&mut h, "step3-volume-joint");
    // Another joint is picked by clicking its cut face in the view: the
    // -Y face, beside the open -X one.
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
    still(&mut h, "step3-volume-other-joint");
}
