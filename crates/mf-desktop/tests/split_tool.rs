#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The Split tool (owner 2026-11-11): cutting only, driven like a user —
//! Slice or Print volume (with its box and face handles), one Cut
//! button, the pieces preview and miss/grazing warnings, then the
//! status line's piece count and the "Add connectors to these joints ->"
//! link into Connect. Connect's own workflow (placing, dragging, the
//! checks and their fixes, Apply, reopening) is in `connect_tool.rs`.
//!
//! With `MF_ARTIFACTS` set, a frame of every step goes to
//! `<dir>/design/split-<step>-<w>x<h>.png`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use glam::DVec3;
use mf_desktop::split_flow::CutMode;
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::{ContentHash, Mesh};
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

/// Slice: one cut plane, Cut, the status line's piece count and the
/// link into Connect on the new joint.
#[test]
fn slice_cuts_and_links_to_connect() {
    require_gpu!("slice_cuts_and_links_to_connect");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    assert_eq!(h.state().split_flow.mode, CutMode::Slice);
    // Off the ball's seam of vertices.
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    still(&mut h, "slice");

    let history = h.state().session.history_labels().len();
    h.get_by_label("Cut").click();
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
    still(&mut h, "cut");

    // The status line's piece count, and the link into Connect.
    h.get_by_label("Add connectors to these joints ->").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, mf_desktop::connect_tool::CONNECT);
    assert_eq!(h.state().split_flow.joint, Some(0), "the new joint opens");
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
    still(&mut h, "volume-preset");
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
    still(&mut h, "volume");

    let v = h.state().split_flow.volume.clone();
    let history = h.state().session.history_labels().len();
    h.get_by_label("Cut").click();
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
    still(&mut h, "volume-cut");

    // The link opens Connect on the first joint.
    h.get_by_label("Add connectors to these joints ->").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, mf_desktop::connect_tool::CONNECT);
    assert!(h.state().split_flow.joint.is_some());

    // Clicking away from Connect and back into Split leaves the joints
    // untouched (Split itself carries no connector state).
    h.get_by_label("Split").click();
    settle(&mut h);
    assert_eq!(h.state().split_flow.joints.len(), 6);
}

/// A plane that misses the model entirely warns before Cut, live, as the
/// gizmo moves (owner 2026-11-11: "Split looked broken" when the default
/// plane missed an off-origin scan).
#[test]
fn a_miss_warns_before_cut() {
    require_gpu!("a_miss_warns_before_cut");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    // Drag the plane far away so it can't cross the ball.
    {
        let a = h.state_mut();
        a.split.plane[11] += 500.0;
    }
    settle(&mut h);
    h.get_by_label("The plane doesn't cross ball; drag it into the model");
    still(&mut h, "miss-warning");

    // A click on the cut face was the old step 3's affordance; placing a
    // connector has no home in Split's panel at all.
    let p = egui::pos2(WINDOW.0 * 0.5, WINDOW.1 * 0.5);
    click_at(&mut h, p);
    settle(&mut h);
    assert!(
        h.state().split_flow.joints.is_empty(),
        "Split places no connectors"
    );
}
