#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The tool session contract (ADR-0007, consolidation plan C21), checked
//! for every tool the shell registers (`shell.tools()`), so a new tool is
//! covered the moment it is on the rail:
//!
//! - (a) Esc during a drag the tool claimed puts back whatever the drag
//!   changed (every object's shown transform, and what the tool shows in
//!   the view), and the release afterwards commits nothing;
//! - (b) Undo after an Apply issued from the tool tells its session
//!   (`on_revision`) and leaves no preview or state bound to the undone
//!   revision; an Apply from the tool's own intent makes one history row;
//! - (c) a refusal of a command the tool issued lands in that tool's own
//!   refusal card (its panel, or Select's inspector card), never the
//!   generic error bar, and its session hears of it;
//! - (d) closing the tool cancels its previews;
//! - (e) no transient tool state survives a switch to another tool
//!   (settings a session keeps are declared by not listing them in
//!   `ToolSession::held`).

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_command::{Invocation, ObjectId};
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::{ContentHash, Mesh};
use mf_io::threemf::{ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};

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
            "timed out waiting for: {what} (tool {}, status {:?}, error {:?}, refusal {:?})",
            h.state().active_tool,
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

fn ball() -> Mesh {
    mf_fixtures::scaled(&mf_fixtures::uv_sphere(24, 32), 10.0)
}

fn settings_dir() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().expect("a temp dir for test settings"))
        .path()
}

/// Two balls side by side, both selected (Align and Combine need two),
/// the first active.
fn opened() -> Harness<'static, MeshFixerApp> {
    let settings = Settings {
        config_dir: Some(settings_dir().to_owned()),
        ..Settings::default()
    };
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::with_settings(cc, None, settings).unwrap());
    h.state_mut().import_model(ImportedModel {
        metadata: Default::default(),
        objects: [("a", -15.0), ("b", 15.0)]
            .into_iter()
            .map(|(name, x)| ModelObject {
                metadata: Default::default(),
                name: name.into(),
                mesh: ball(),
                transform: at([x, 0.0, 10.0]),
            })
            .collect(),
        unit: Some(Unit::Millimeter),
        source: SourceInfo {
            path: "session-contract.3mf".into(),
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

fn ids(h: &Harness<'_, MeshFixerApp>) -> Vec<ObjectId> {
    h.state()
        .session
        .document()
        .objects()
        .iter()
        .map(|o| o.id)
        .collect()
}

/// Every object as the view draws it, and what the open tool shows and
/// previews.
fn view(h: &Harness<'_, MeshFixerApp>, tool: &str) -> (Vec<[f64; 12]>, String, String) {
    let a = h.state();
    let shown = a
        .session
        .document()
        .objects()
        .iter()
        .map(|o| a.shown_transform(o.id).unwrap_or(o.transform))
        .collect();
    (
        shown,
        a.tool_view_state(tool),
        format!("{:?}", a.tool_preview(tool)),
    )
}

/// Let a tool's settling previews start and finish (Hollow waits 250 ms
/// after its settings last changed).
fn quiet(h: &mut Harness<'_, MeshFixerApp>) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(400) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    settle(h);
}

/// How many steps back to the import the document is (an undone step
/// ahead of it, still on the redo path, isn't counted).
fn history_len(h: &Harness<'_, MeshFixerApp>) -> usize {
    h.state().session.history_cursor()
}

/// Back to Select with both balls selected and nothing pending.
fn reset(h: &mut Harness<'_, MeshFixerApp>) {
    h.event(egui::Event::PointerGone);
    h.state_mut().close_rail_tool();
    let all = ids(h);
    h.state_mut().picked = all.clone();
    h.state_mut().active = Some(all[0]);
    h.state_mut().refusal = None;
    h.state_mut().error = None;
    settle(h);
}

fn open(h: &mut Harness<'_, MeshFixerApp>, id: &'static str) {
    reset(h);
    h.state_mut().open_rail_tool(id);
    settle(h);
    assert_eq!(
        h.state().active_tool,
        id,
        "{id} opens on the fixture (status {:?})",
        h.state().status
    );
}

/// Where the harness aims a press: the selection's centre (a gizmo on
/// it, Split's plane), or the active object's (a stroke on its surface).
#[derive(Debug, Clone, Copy)]
enum Aim {
    Selection,
    Active,
}

/// A point of the view (window points) the open tool claims a press at,
/// nearest `aim` on screen.
fn claimed_point(h: &Harness<'_, MeshFixerApp>, aim: Aim) -> Option<egui::Pos2> {
    let app = h.state();
    let rect = app.viewport.last_rect?;
    let claims = (app.active_tool_def().claims)(app)?;
    let doc = app.session.document();
    let centre_of = |id: ObjectId| {
        doc.object(id)
            .map(|o| [o.transform[9], o.transform[10], o.transform[11]])
    };
    let world = match aim {
        Aim::Active => centre_of(app.active?)?,
        Aim::Selection => {
            let all: Vec<[f64; 3]> = doc
                .objects()
                .iter()
                .filter_map(|o| centre_of(o.id))
                .collect();
            let n = all.len() as f64;
            [0, 1, 2].map(|k| all.iter().map(|c| c[k]).sum::<f64>() / n)
        }
    };
    let centre = app.screen_point(world)?;
    let mut best: Option<egui::Pos2> = None;
    let mut y = 2.0;
    while y < rect.height() {
        let mut x = 2.0;
        while x < rect.width() {
            if claims((x as f64, y as f64)) {
                let p = rect.left_top() + egui::vec2(x, y);
                if best.is_none_or(|b| (p - centre).length() < (b - centre).length()) {
                    best = Some(p);
                }
            }
            x += 4.0;
        }
        y += 4.0;
    }
    best
}

fn button(h: &mut Harness<'_, MeshFixerApp>, pos: egui::Pos2, pressed: bool) {
    h.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

/// (a) Esc during a drag restores what the drag changed. True if the
/// drag changed something to put back.
fn esc_cancels_a_drag(h: &mut Harness<'_, MeshFixerApp>, id: &'static str, aim: Aim) -> bool {
    open(h, id);
    quiet(h);
    let Some(from) = claimed_point(h, aim) else {
        // The tool claims no press on this fixture: nothing to drag.
        return false;
    };
    let before = view(h, id);
    let history = history_len(h);
    h.event(egui::Event::PointerMoved(from));
    h.step();
    button(h, from, true);
    let mut changed = false;
    for i in 1..=8 {
        let p = from + egui::vec2(10.0, -12.0) * i as f32;
        h.event(egui::Event::PointerMoved(p));
        h.step();
        changed |= view(h, id) != before;
    }
    h.key_press(egui::Key::Escape);
    h.step();
    if changed {
        assert_eq!(
            view(h, id),
            before,
            "{id}: Esc puts back what the drag changed"
        );
        assert_eq!(
            h.state().active_tool,
            id,
            "{id}: Esc cancels the drag first, not the tool"
        );
    }
    let end = from + egui::vec2(120.0, -140.0);
    h.event(egui::Event::PointerMoved(end));
    h.step();
    button(h, end, false);
    settle(h);
    assert_eq!(
        history_len(h),
        history,
        "{id}: a cancelled drag commits nothing"
    );
    if changed {
        assert_eq!(
            view(h, id),
            before,
            "{id}: the cancelled drag stays cancelled"
        );
    }
    changed
}

/// (b) Undo after Apply notifies the session and leaves nothing stale.
fn undo_after_apply(h: &mut Harness<'_, MeshFixerApp>, id: &'static str) {
    open(h, id);
    let history = history_len(h);
    let own = h.state().tool_intent().is_some();
    if own {
        h.state_mut().apply_tool();
    } else {
        // No Apply of its own here: a placement change issued while it is
        // open stands in for one (it's still a revision to undo past).
        let all = ids(h);
        let mut t = h
            .state()
            .session
            .document()
            .object(all[0])
            .unwrap()
            .transform;
        t[9] += 2.0;
        h.state_mut().commit_transform(all[0], t, "Move".into());
    }
    wait_until(h, "the apply", |a| a.busy.is_none());
    settle(h);
    assert!(h.state().error.is_none(), "{id}: {:?}", h.state().error);
    if h.state().refusal.is_some() {
        // A tool whose Apply this fixture refuses is covered by (c).
        return;
    }
    if own {
        assert_eq!(
            history_len(h),
            history + 1,
            "{id}: one Apply, one history row (status {:?}, notice {:?})",
            h.state().status,
            h.state().notice
        );
    }
    let seen = h.state().revisions_seen(id);
    h.state_mut().undo();
    wait_until(h, "the undo", |a| a.busy.is_none());
    settle(h);
    assert!(
        h.state().revisions_seen(id) > seen,
        "{id}: its session hears about the undo"
    );
    assert!(
        h.state().tool_stale(id).is_empty(),
        "{id}: nothing bound to the undone revision: {:?}",
        h.state().tool_stale(id)
    );
    assert!(
        h.state().settling_placement().is_none(),
        "{id}: no placement left drawn"
    );
}

/// (c) A refused command lands in its issuer's card.
fn refusal_lands_in_its_card(h: &mut Harness<'_, MeshFixerApp>, id: &'static str) {
    open(h, id);
    let results = h.state().results_seen(id);
    let all = ids(h);
    // Always refused: `leaders` isn't valid JSON.
    h.state_mut().run_invocation(
        Invocation {
            op_id: "object.align".into(),
            target: all[0],
            selection: None,
            operands: vec![("target".to_owned(), all[1], None)],
            raw_params: std::collections::BTreeMap::from([
                ("axis".to_owned(), "x".to_owned()),
                ("mode".to_owned(), "centre".to_owned()),
                ("leaders".to_owned(), "not valid json".to_owned()),
            ]),
        },
        "Refused".into(),
    );
    wait_until(h, "the refusal", |a| a.busy.is_none());
    settle(h);
    assert!(h.state().error.is_none(), "{id}: {:?}", h.state().error);
    assert_eq!(
        h.state().refusal_issuer(),
        Some(id),
        "{id}: routed to its issuer"
    );
    assert!(
        h.state().results_seen(id) > results,
        "{id}: its session heard"
    );
    let message = h.state().refusal.clone().unwrap().message;
    assert!(
        h.query_by_label(&message).is_some(),
        "{id}: its card shows {message:?}"
    );
}

/// (d) Closing the tool cancels its previews.
fn close_cancels_previews(h: &mut Harness<'_, MeshFixerApp>, id: &'static str) {
    open(h, id);
    // Long enough for a settling preview (Hollow waits 250 ms) to start.
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(300) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    h.state_mut().close_rail_tool();
    assert_eq!(
        h.state().previews_owned_by(id),
        0,
        "{id}: closing it cancels its previews"
    );
    settle(h);
    assert!(
        h.state().tool_preview(id).is_none(),
        "{id}: no preview shown"
    );
}

/// (e) No transient state survives a tool switch.
fn nothing_survives_a_switch(h: &mut Harness<'_, MeshFixerApp>, id: &'static str) {
    open(h, id);
    // Hover what it claims (or the view's centre), to give it something.
    let rect = h.state().viewport.last_rect.unwrap();
    let p = claimed_point(h, Aim::Active).unwrap_or(rect.center());
    h.event(egui::Event::PointerMoved(p));
    h.run_steps(3);
    let other = if id == "inspect" { "export" } else { "inspect" };
    h.state_mut().open_rail_tool(other);
    settle(h);
    assert_eq!(
        h.state().tool_holds(id),
        Vec::<&str>::new(),
        "{id}: nothing survives the switch"
    );
}

#[test]
fn every_tool_honours_the_session_contract() {
    require_gpu!("every_tool_honours_the_session_contract");
    let mut h = opened();
    let tools: Vec<&'static str> = h.state().shell.tools().iter().map(|t| t.id).collect();
    assert!(tools.len() >= 14, "{tools:?}");
    let mut dragged = Vec::new();
    for id in tools {
        if [Aim::Selection, Aim::Active]
            .into_iter()
            .any(|aim| esc_cancels_a_drag(&mut h, id, aim))
        {
            dragged.push(id);
        }
        undo_after_apply(&mut h, id);
        refusal_lands_in_its_card(&mut h, id);
        close_cancels_previews(&mut h, id);
        nothing_survives_a_switch(&mut h, id);
    }
    // The harness really drags the tools that have drags on this fixture
    // (Connect's needs a cut joint: `esc_cancels_a_connect_drag`).
    for id in ["transform", "align", "split", "sculpt"] {
        assert!(dragged.contains(&id), "{id} was dragged: {dragged:?}");
    }
}
