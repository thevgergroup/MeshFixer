#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The History panel's row labels (I5, owner 2026-11-11): a short plain
//! sentence naming the operation and its object ("Split scan into 2
//! pieces"), not the raw parameter list ("mesh.split_plane (cut_gap=0,
//! …)"). Covers split, make_solid, combine, place_connectors and
//! set_transform — every op `history_label` (crates/mf-desktop/src/app.rs)
//! names.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
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

fn at(t: [f64; 3]) -> [f64; 12] {
    [
        1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, t[0], t[1], t[2],
    ]
}

/// A closed ball, `d` mm across.
fn ball(d: f64) -> Mesh {
    mf_fixtures::scaled(&mf_fixtures::uv_sphere(32, 48), d / 2.0)
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
            path: "history.3mf".into(),
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

fn row<'a>(h: &'a Harness<'_, MeshFixerApp>, name: &'a str) -> egui_kittest::Node<'a> {
    h.get_by_role_and_label(egui::accesskit::Role::Button, name)
}

fn panel_button<'a>(h: &'a Harness<'_, MeshFixerApp>, label: &'a str) -> egui_kittest::Node<'a> {
    h.query_all_by_label(label)
        .max_by(|a, b| a.rect().width().total_cmp(&b.rect().width()))
        .unwrap_or_else(|| panic!("no {label:?}"))
}

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

/// The current path's most recent label (the step just taken).
fn last_label(h: &Harness<'_, MeshFixerApp>) -> String {
    h.state()
        .breadcrumb()
        .into_iter()
        .rfind(|(_, _, _, reached)| *reached)
        .map(|(_, label, _, _)| label)
        .expect("a step was taken")
}

/// The current path's most recent step's details (its full, applicable,
/// non-default parameters).
fn last_details(h: &Harness<'_, MeshFixerApp>) -> String {
    h.state()
        .breadcrumb()
        .into_iter()
        .rfind(|(_, _, _, reached)| *reached)
        .map(|(_, _, details, _)| details)
        .expect("a step was taken")
}

/// Split: "Split {name} into {n} pieces", not the raw parameter list;
/// and the full parameter list, with the pegs-only ones hidden when
/// pegs is "none" (the default) and shown when it isn't.
#[test]
fn split_names_the_object_and_the_piece_count() {
    require_gpu!("split_names_the_object_and_the_piece_count");
    let mut h = opened(vec![("scan", ball(60.0), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    h.get_by_label("Cut").click();
    wait_until(&mut h, "the cut", |a| a.busy.is_none());
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let label = last_label(&h);
    assert!(label.starts_with("Split scan into 2 piece"), "{label:?}");
    assert!(!label.contains("mesh.split_plane"), "{label:?}");
    // pegs: none (the default): the pegs-only parameters don't apply and
    // are hidden from the details, even though some differ from the
    // spec's own defaults (the fit profile fills peg_clearance etc.).
    let details = last_details(&h);
    assert!(!details.contains("Hole diameter"), "{details:?}");
    assert!(!details.contains("Peg shape"), "{details:?}");
}

/// Make solid: "Made {name} solid ({cell} mm cells)".
#[test]
fn make_solid_names_the_object_and_the_cell_size() {
    require_gpu!("make_solid_names_the_object_and_the_cell_size");
    // Two interpenetrating cubes in one object: self-overlapping, so
    // Combine would refuse it and Make solid is the way on.
    let tangled = mf_fixtures::merge(&[
        mf_fixtures::cube(20.0),
        mf_fixtures::translated(&mf_fixtures::cube(20.0), [10.0, 6.0, 4.0]),
    ]);
    let mut h = opened(vec![("tangle", tangled, at([0.0; 3]))]);
    row(&h, "tangle").click();
    settle(&mut h);
    h.get_by_label("Make solid").click();
    settle(&mut h);
    h.get_by_label("Make it solid").click();
    wait_until(&mut h, "make solid", |a| a.busy.is_none());
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let label = last_label(&h);
    assert!(label.starts_with("Made tangle solid"), "{label:?}");
    assert!(label.contains("mm cells"), "{label:?}");
    assert!(!label.contains("mesh.make_solid"), "{label:?}");
}

/// Combine: "Combined {n} objects into {name}".
#[test]
fn combine_names_the_object_count_and_the_result() {
    require_gpu!("combine_names_the_object_count_and_the_result");
    let mut h = opened(vec![
        ("block", mf_fixtures::cube(20.0), at([0.0; 3])),
        ("post", mf_fixtures::cube(8.0), at([6.0, 6.0, 16.0])),
        ("peg", ball(10.0), at([20.0, 10.0, 10.0])),
    ]);
    row(&h, "peg").click();
    settle(&mut h);
    h.key_press(egui::Key::H);
    settle(&mut h);
    select_together(&mut h, &["block", "post", "peg (hole)"]);
    h.get_by_label("Combine").click();
    settle(&mut h);
    panel_button(&h, "Combine").click();
    wait_until(&mut h, "the combine", |a| a.busy.is_none());
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let label = last_label(&h);
    assert_eq!(label, "Combined 3 objects into block", "{label:?}");
    assert!(!label.contains("mesh.combine"), "{label:?}");
}

/// Transform: "Moved {name} by x, y, z mm", reusing the status line's own
/// words (`transform_tool::describe`).
#[test]
fn set_transform_names_the_object_and_the_move() {
    require_gpu!("set_transform_names_the_object_and_the_move");
    let mut h = opened(vec![("part", ball(20.0), at([0.0, 0.0, 10.0]))]);
    let id = id_of(&h, "part");
    let to = at([12.0, 0.0, 10.0]);
    {
        let a = h.state_mut();
        a.active = Some(id);
        a.commit_transform(id, to, "moved".into());
    }
    wait_until(&mut h, "the move", |a| a.busy.is_none());
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let label = last_label(&h);
    assert!(label.starts_with("Moved part by"), "{label:?}");
    assert!(label.contains("mm"), "{label:?}");
    assert!(!label.contains("object.set_transform"), "{label:?}");
}

/// The status bar's height (window points): a control under it is hidden.
const STATUS_BAR_HEIGHT: f32 = 36.0;

/// Scrolls the tool panel until the control labelled `label` is fully
/// in view, then clicks it (as `connect_tool.rs` does).
fn scroll_and_click(h: &mut Harness<'_, MeshFixerApp>, label: &str) {
    h.event(egui::Event::PointerMoved(egui::pos2(200.0, WINDOW.1 * 0.5)));
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

/// Connect's Apply: "Added {n} {kind} to {name}'s joint" when every
/// connector is the same kind.
#[test]
fn place_connectors_names_the_count_kind_and_joint() {
    require_gpu!("place_connectors_names_the_count_kind_and_joint");
    let mut h = opened(vec![("scan", ball(60.0), at([0.0, 0.0, 30.0]))]);
    h.get_by_label("Split").click();
    settle(&mut h);
    h.state_mut().split.plane[11] += 1.3;
    settle(&mut h);
    h.get_by_label("Cut").click();
    wait_until(&mut h, "the cut", |a| a.busy.is_none());
    settle(&mut h);
    assert_eq!(h.state().split_flow.joints.len(), 1);

    h.get_by_label("Connect").click();
    wait_until(&mut h, "the joint's cut faces", |a| {
        a.split_flow.geometry().is_some()
    });
    settle(&mut h);
    let cap = |h: &Harness<'_, MeshFixerApp>, uv: [f64; 2]| {
        let a = h.state();
        let j = a.split_flow.open_joint().unwrap();
        let lift = j.frame.cap_point(j.solid()) - j.frame.point;
        a.screen_point((j.frame.at(uv) + lift).to_array()).unwrap()
    };
    let click_at = |h: &mut Harness<'_, MeshFixerApp>, p: egui::Pos2| {
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
    };
    // Two dowels (the default kind): both the same, so the label names
    // the kind, not just a generic "connectors".
    let p1 = cap(&h, [-10.0, 0.0]);
    click_at(&mut h, p1);
    settle(&mut h);
    let p2 = cap(&h, [10.0, 0.0]);
    click_at(&mut h, p2);
    settle(&mut h);
    assert_eq!(
        h.state().split_flow.open_joint().unwrap().placements.len(),
        2
    );

    // The Placed list puts the button below the panel's fold: scrolled
    // to first, as a user would (a click on a clipped button is lost).
    scroll_and_click(&mut h, "Add connectors");
    wait_until(&mut h, "the connectors", |a| a.busy.is_none());
    settle(&mut h);
    assert!(h.state().refusal.is_none(), "{:?}", h.state().refusal);
    let label = last_label(&h);
    assert!(label.starts_with("Added 2 dowels to"), "{label:?}");
    assert!(label.ends_with("'s joint"), "{label:?}");
    assert!(!label.contains("split.place_connectors"), "{label:?}");
}
