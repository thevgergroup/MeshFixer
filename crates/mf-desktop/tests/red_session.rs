#![allow(clippy::unwrap_used)] // tests may panic on failure

//! C02 red tests (consolidation-plan.md): queue, refusals, placement.
//!
//! - `rapid_strokes_are_all_recorded`: the command queue coalesces any
//!   queued invocation with the same `op_id`/`target` (`app.rs`'s
//!   `run_invocation`), right for `set_transform`/`set_shape_params`
//!   (absolute params), wrong for `mesh.sculpt_stroke` (every stroke is
//!   a delta). Three quick strokes while the first is still busy must
//!   give three history rows, not two.
//! - `refusal_reaches_issuing_panel`, one per tool (Hollow, Align,
//!   Sculpt, Repair): an Apply refusal must set `app.refusal` (so the
//!   issuing tool's own panel shows it), never fall through to the
//!   generic `app.error`. `shape_tools::op_of`/`ops_of` don't list
//!   these tools' ops, so today they do.
//! - `refused_connector_does_not_open_transform`: the Connector arm of
//!   the pending placement (`app.rs`, around `pending_open_transform`'s
//!   drain) has no `new_object != scan` guard, unlike the Part arm. A
//!   refused connector placement must leave Transform closed and
//!   record nothing placed.
//!
//! Fixed by: C04 (queue coalescing becomes a declared `coalesce:` spec
//! field, and the Connector arm gets the same guard the Part arm has).
//! Each Apply-refusal-routing test is also fixed by C04 per the plan.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_command::{Invocation, ObjectId};
use mf_desktop::app::PendingPlacement;
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::{ContentHash, Mesh};
use mf_io::threemf::{ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_selection::{Selection, SelectionKind};

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
            path: "red_session.3mf".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
            exporter_hint: None,
        },
        warnings: Vec::new(),
    });
    wait_until(&mut h, "analysis and picking", |a| {
        a.current_analysis().is_some() && a.picking_ready() && a.camera.is_some()
    });
    h.event(egui::Event::PointerGone);
    settle(&mut h);
    h
}

fn at(t: [f64; 3]) -> [f64; 12] {
    [
        1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, t[0], t[1], t[2],
    ]
}

/// A closed 20 mm ball: a plain whole-object target, big enough that
/// the ops below (hollow, a stroke) have real geometry to work with.
fn ball() -> Mesh {
    mf_fixtures::scaled(&mf_fixtures::uv_sphere(24, 32), 10.0)
}

fn pick_point(h: &Harness<'_, MeshFixerApp>, world: [f64; 3]) -> egui::Pos2 {
    h.state()
        .screen_point(world)
        .expect("the point must project to a screen position")
}

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
}

fn release(h: &mut Harness<'_, MeshFixerApp>, at: egui::Pos2) {
    h.event(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

// ---------------------------------------------------------------------
// Queue semantics: rapid sculpt strokes while busy.
// ---------------------------------------------------------------------

/// Three quick strokes, fired one after another without waiting for
/// the previous one's `mesh.sculpt_stroke` command to land (`app.busy`
/// still `Some`), must give three undo steps: each stroke is a delta on
/// the live mesh, not an absolute setting the next one should replace.
/// `run_invocation`'s queue today coalesces any command with the same
/// `op_id`/`target`, so the middle stroke (whichever one is still
/// queued when the next drag starts) is silently overwritten, and only
/// two history rows land.
#[test]
#[ignore = "red: C04"]
fn rapid_strokes_are_all_recorded() {
    require_gpu!("rapid_strokes_are_all_recorded");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 10.0]))]);
    h.get_by_label("Sculpt").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "sculpt");

    let history = h.state().session.history_labels().len();
    let top = pick_point(&h, [0.0, 0.0, 20.0]);

    // Three short strokes, each started and released well before the
    // previous one's command has returned from the worker pool (no
    // `wait_until`/`settle` between them): the real-world "quick
    // strokes" the owner reported.
    for dx in [0.0_f32, 40.0, 80.0] {
        drag(
            &mut h,
            top + egui::vec2(dx, 0.0),
            top + egui::vec2(dx + 20.0, 0.0),
            4,
        );
        release(&mut h, top + egui::vec2(dx + 20.0, 0.0));
    }

    wait_until(&mut h, "every queued stroke to land", |a| {
        a.busy.is_none() && a.pending_invocation_depth() == 0 && a.sculpt_stroke.is_none()
    });
    settle(&mut h);

    assert_eq!(
        h.state().session.history_labels().len(),
        history + 3,
        "three quick strokes must give three undo steps, not fewer: {:?}",
        h.state().session.history_labels()
    );
}

// ---------------------------------------------------------------------
// Refusal routing: an Apply refusal must reach the issuing tool's own
// panel (`app.refusal`, matched by `op_id`), never the generic
// `app.error`.
// ---------------------------------------------------------------------

/// Hollow: `mesh.hollow` refused (`wall_thickness` of 0, `invalid_
/// parameter`) through a direct Apply, the same `app.run_invocation`
/// call `hollow_tool.rs`'s Apply button makes. `mesh.hollow` is in
/// neither `shape_tools::op_of` nor `ops_of`, so today the refusal
/// falls through to `app.error` and Hollow's own panel (`hollow_tool.
/// rs:778`, `refusal_card_for(app, ui, "mesh.hollow")`) has nothing to
/// show.
#[test]
#[ignore = "red: C16"]
fn refusal_reaches_issuing_panel_hollow() {
    require_gpu!("refusal_reaches_issuing_panel_hollow");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 10.0]))]);
    h.get_by_label("Hollow").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "hollow");
    let id = h.state().active.expect("Hollow needs an active object");

    h.state_mut().refusal = None;
    h.state_mut().error = None;
    h.state_mut().run_invocation(
        Invocation {
            op_id: "mesh.hollow".into(),
            target: id,
            selection: None,
            operands: Vec::new(),
            raw_params: std::collections::BTreeMap::from([
                ("wall_thickness".to_owned(), "0".to_owned()),
                ("inner_detail".to_owned(), "0".to_owned()),
                ("drain_holes".to_owned(), "none".to_owned()),
                ("hole_diameter".to_owned(), "3".to_owned()),
                ("hole_count".to_owned(), "1".to_owned()),
                ("hole_profile".to_owned(), "round".to_owned()),
                ("open_through".to_owned(), "false".to_owned()),
            ]),
        },
        "Hollow".into(),
    );
    wait_until(&mut h, "the refused Apply to land", |a| a.busy.is_none());
    settle(&mut h);

    assert_eq!(
        h.state().refusal.as_ref().map(|r| r.op_id.as_str()),
        Some("mesh.hollow"),
        "the refusal must reach Hollow's own panel (app.refusal), not app.error: {:?} / {:?}",
        h.state().refusal,
        h.state().error,
    );
    assert!(
        h.state().error.is_none(),
        "a refusal that reaches its panel must not also set the generic app.error: {:?}",
        h.state().error
    );
}

/// Align: `object.align` refused (`leaders` isn't valid JSON,
/// `invalid_leaders`) with two objects selected (so Align's own
/// availability gate, which needs 2+, is satisfied — the refusal is
/// the op's, not the tool's). `object.align` is in neither `op_of` nor
/// `ops_of` (`align_tool.rs:687`'s `refusal_card_for(app, ui, "object.
/// align")` only ever sees a preview refusal, never Apply's).
#[test]
#[ignore = "red: C16"]
fn refusal_reaches_issuing_panel_align() {
    require_gpu!("refusal_reaches_issuing_panel_align");
    let mut h = opened(vec![
        ("a", ball(), at([-20.0, 0.0, 10.0])),
        ("b", ball(), at([20.0, 0.0, 10.0])),
    ]);
    let ids: Vec<ObjectId> = h
        .state()
        .session
        .document()
        .objects()
        .iter()
        .map(|o| o.id)
        .collect();
    assert_eq!(ids.len(), 2);
    h.state_mut().picked = ids.clone();
    h.state_mut().active = Some(ids[0]);
    h.get_by_label("Align").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "align");

    h.state_mut().refusal = None;
    h.state_mut().error = None;
    h.state_mut().run_invocation(
        Invocation {
            op_id: "object.align".into(),
            target: ids[0],
            selection: None,
            operands: vec![("target".to_owned(), ids[1], None)],
            raw_params: std::collections::BTreeMap::from([
                ("axis".to_owned(), "x".to_owned()),
                ("mode".to_owned(), "centre".to_owned()),
                ("leaders".to_owned(), "not valid json".to_owned()),
            ]),
        },
        "Align".into(),
    );
    wait_until(&mut h, "the refused Apply to land", |a| a.busy.is_none());
    settle(&mut h);

    assert_eq!(
        h.state().refusal.as_ref().map(|r| r.op_id.as_str()),
        Some("object.align"),
        "the refusal must reach Align's own panel (app.refusal), not app.error: {:?} / {:?}",
        h.state().refusal,
        h.state().error,
    );
    assert!(
        h.state().error.is_none(),
        "a refusal that reaches its panel must not also set the generic app.error: {:?}",
        h.state().error
    );
}

/// Sculpt: `mesh.sculpt_stroke` refused (a dab naming a face index the
/// mesh doesn't have, `bad_sample`) through a direct Apply, the same
/// call `sculpt_tool.rs`'s `commit_stroke` makes. Sculpt's panel has no
/// `refusal_card_for` call at all (`sculpt_tool.rs`'s `panel`), so even
/// if routing were fixed there is nowhere to show it yet — this is the
/// routing half: the refusal must at least reach `app.refusal`, not
/// `app.error`.
#[test]
#[ignore = "red: C16"]
fn refusal_reaches_issuing_panel_sculpt() {
    require_gpu!("refusal_reaches_issuing_panel_sculpt");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 10.0]))]);
    h.get_by_label("Sculpt").click();
    settle(&mut h);
    assert_eq!(h.state().active_tool, "sculpt");
    let id = h.state().active.expect("Sculpt needs an active object");

    h.state_mut().refusal = None;
    h.state_mut().error = None;
    // face 999_999 does not exist on any mesh this test builds: the
    // dab is well-formed (in range barycentrics, a valid pressure), so
    // `check_sample`'s only objection is the face id itself.
    let samples = "999999,0.3,0.3,0.4,1".to_owned();
    h.state_mut().run_invocation(
        Invocation {
            op_id: "mesh.sculpt_stroke".into(),
            target: id,
            selection: None,
            operands: Vec::new(),
            raw_params: std::collections::BTreeMap::from([
                ("brush".to_owned(), "inflate".to_owned()),
                ("radius".to_owned(), "2".to_owned()),
                ("strength".to_owned(), "0.5".to_owned()),
                ("falloff".to_owned(), "smooth".to_owned()),
                ("symmetry_x".to_owned(), "false".to_owned()),
                ("symmetry_y".to_owned(), "false".to_owned()),
                ("symmetry_z".to_owned(), "false".to_owned()),
                ("pivot".to_owned(), "0,0,20".to_owned()),
                ("view_dir".to_owned(), "0,0,-1".to_owned()),
                ("samples".to_owned(), samples),
                ("detail_mode".to_owned(), "off".to_owned()),
                ("detail".to_owned(), "0.3".to_owned()),
                ("stay_on_surface".to_owned(), "true".to_owned()),
            ]),
        },
        "Sculpt".into(),
    );
    wait_until(&mut h, "the refused Apply to land", |a| a.busy.is_none());
    settle(&mut h);

    assert_eq!(
        h.state().refusal.as_ref().map(|r| r.op_id.as_str()),
        Some("mesh.sculpt_stroke"),
        "the refusal must reach app.refusal (Sculpt's own panel), not app.error: {:?} / {:?}",
        h.state().refusal,
        h.state().error,
    );
    assert!(
        h.state().error.is_none(),
        "a refusal that reaches its panel must not also set the generic app.error: {:?}",
        h.state().error
    );
}

/// Repair: `mesh.fill_holes` refused (`invalid_target`: a boundary-loop
/// id the mesh's topology doesn't have) through a direct Apply, the
/// same call the Repair panel's "Fill" buttons make. Repair's `mesh.
/// fill_holes` is driven through the legacy one-shot `tools::CATALOGUE`
/// path (`app.rs::open_tool`), and `shape_tools::ops_of("repair")` is
/// undeclared (defaults to empty), so the refusal falls through to
/// `app.error`; `repair_panel` has no refusal card at all.
#[test]
#[ignore = "red: C16"]
fn refusal_reaches_issuing_panel_repair() {
    require_gpu!("refusal_reaches_issuing_panel_repair");
    let mut h = opened(vec![("ball", ball(), at([0.0, 0.0, 10.0]))]);
    h.state_mut().open_tool("mesh.fill_holes");
    settle(&mut h);
    // `mf_desktop::tool_panels::REPAIR`'s value: the module is private
    // to the crate, so the id is spelled out here instead.
    assert_eq!(h.state().active_tool, "repair");
    let id = h.state().active.expect("Repair needs an active object");
    let revision = h.state().session.current();

    h.state_mut().refusal = None;
    h.state_mut().error = None;
    h.state_mut().run_invocation(
        Invocation {
            op_id: "mesh.fill_holes".into(),
            target: id,
            selection: Some(Selection {
                object: id,
                revision,
                kind: SelectionKind::BoundaryLoop,
                ids: vec![999_999],
            }),
            operands: Vec::new(),
            raw_params: std::collections::BTreeMap::from([
                ("max_loop_edges".to_owned(), "32".to_owned()),
                ("refine".to_owned(), "false".to_owned()),
                ("fill_style".to_owned(), "flat".to_owned()),
            ]),
        },
        "Fill holes".into(),
    );
    wait_until(&mut h, "the refused Apply to land", |a| a.busy.is_none());
    settle(&mut h);

    assert_eq!(
        h.state().refusal.as_ref().map(|r| r.op_id.as_str()),
        Some("mesh.fill_holes"),
        "the refusal must reach app.refusal (Repair's own panel), not app.error: {:?} / {:?}",
        h.state().refusal,
        h.state().error,
    );
    assert!(
        h.state().error.is_none(),
        "a refusal that reaches its panel must not also set the generic app.error: {:?}",
        h.state().error
    );
}

// ---------------------------------------------------------------------
// A refused connector placement must not open Transform on the scan.
// ---------------------------------------------------------------------

/// The Connector arm of the pending placement drain (`app.rs`, the
/// `pending_open_transform` block) has no `new_object != scan` guard,
/// unlike the Part arm right above it. After a refused placement,
/// `app.active` is still the scan (nothing new committed), so the
/// Connector arm records the scan itself as the placed connector and
/// opens Transform on it — exactly what the Part arm's guard exists to
/// prevent.
///
/// This drives the same two steps the real connector placement makes
/// (`connector_tool.rs::run_on`, then setting `pending_open_transform`
/// before the result is known), with a deliberately refused `object.
/// add_shape` (`shapes: "[]"`, `nothing_to_add`) standing in for a
/// placement the fit/collision logic would have refused for real.
#[test]
#[ignore = "red: C04"]
fn refused_connector_does_not_open_transform() {
    require_gpu!("refused_connector_does_not_open_transform");
    let mut h = opened(vec![("scan", ball(), at([0.0, 0.0, 10.0]))]);
    settle(&mut h);
    let scan = h.state().active.expect("the scan must be active");

    h.state_mut().run_invocation(
        Invocation {
            op_id: "object.add_shape".into(),
            target: scan,
            selection: None,
            operands: Vec::new(),
            raw_params: std::collections::BTreeMap::from([
                ("shapes".to_owned(), "[]".to_owned()),
                ("transform".to_owned(), "1,0,0,0,1,0,0,0,1,0,0,0".to_owned()),
                ("group".to_owned(), "g1".to_owned()),
            ]),
        },
        "Place connector".into(),
    );
    // Exactly what `connector_tool.rs::place` does right after `run_on`:
    // record the pending placement before the result is known, gated
    // only on `busy` (never on whether the placement will succeed).
    if h.state().busy.is_some() {
        h.state_mut().pending_open_transform = Some(PendingPlacement::Connector {
            connector_id: "thread_m3".into(),
            connector_params_json: "{}".into(),
            gap: 0.2,
            group: "g1".into(),
        });
    }

    wait_until(&mut h, "the refused placement to land", |a| {
        a.busy.is_none()
    });
    settle(&mut h);

    assert_eq!(
        h.state().active,
        Some(scan),
        "a refused add_shape must leave the scan active (nothing new was created): {:?}",
        h.state().active
    );
    assert_ne!(
        h.state().active_tool,
        mf_desktop::transform_tool::ID,
        "a refused connector placement must not open Transform"
    );
    assert!(
        h.state().connectors.placed.is_none(),
        "a refused connector placement must record nothing placed: {:?}",
        h.state().connectors.placed
    );
}
