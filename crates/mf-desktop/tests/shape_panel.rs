#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Editable shapes in the selection panel (parametric-shapes brief):
//! a placed shape shows "Editable shape" and its params; editing one
//! regenerates it as one undo step; Combine bakes it ("Mesh").
//!
//! With `MF_ARTIFACTS` set, frames go to
//! `<dir>/design/shapes-<scene>-<w>x<h>.png`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
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
    let name = format!("shapes-{scene}-{}x{}.png", size.x as u32, size.y as u32);
    h.render().unwrap().save(dir.join(name)).unwrap();
}

fn opened(objects: Vec<(&str, Mesh, [f64; 12])>) -> Harness<'static, MeshFixerApp> {
    opened_with(objects, Settings::default())
}

/// As [`opened`], with its own settings: tests that save a part set
/// `config_dir` to a temp directory, so the real user's settings folder
/// is never touched.
fn opened_with(
    objects: Vec<(&str, Mesh, [f64; 12])>,
    settings: Settings,
) -> Harness<'static, MeshFixerApp> {
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
        unit: Unit::Millimeter,
        source: SourceInfo {
            path: "shapes.3mf".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
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

/// A closed 20 mm ball.
fn ball() -> Mesh {
    mf_fixtures::scaled(&mf_fixtures::uv_sphere(32, 48), 10.0)
}

fn place(h: &mut Harness<'_, MeshFixerApp>, label: &str) -> mf_command::ObjectId {
    h.get_by_label("Parts").click();
    settle(h);
    let history = h.state().session.history_labels().len();
    h.get_by_label(label).click();
    wait_until(h, "the shape placed", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(h);
    h.state().active.unwrap()
}

fn run(h: &mut Harness<'_, MeshFixerApp>, what: &str, f: impl FnOnce(&mut MeshFixerApp)) {
    let history = h.state().session.history_labels().len();
    f(h.state_mut());
    wait_until(h, what, |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(h);
}

fn width(h: &Harness<'_, MeshFixerApp>, id: mf_command::ObjectId) -> f64 {
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

/// Selecting a shape shows its params; editing one regenerates the mesh
/// as one undo step, keeping the object; Combine bakes it into a Mesh.
#[test]
fn the_selection_panel_edits_a_shape_and_shows_when_it_is_baked() {
    require_gpu!("the_selection_panel_edits_a_shape_and_shows_when_it_is_baked");
    let mut h = opened(vec![("scan", ball(), at([0.0, 0.0, 10.0]))]);
    let id = place(&mut h, "Box");
    assert_eq!(h.state().active_tool, "transform");
    h.get_by_label("Editable shape · Box");
    still(&mut h, "transform-panel");
    // Back to Select: the inspector card shows the same params.
    h.get_by_label("Select").click();
    settle(&mut h);
    h.state_mut().set_active(id);
    settle(&mut h);
    h.get_by_label("Editable shape · Box");
    h.get_by_label("Width");
    h.get_by_label("Corner radius");
    still(&mut h, "inspector");
    assert!((width(&h, id) - 10.0).abs() < 1e-9);

    let before = h.state().session.history_labels().len();
    run(&mut h, "the width change", |a| {
        mf_desktop::shape_panel::set_param(a, "width", mf_spec::ParamValue::Float(25.0));
    });
    assert_eq!(h.state().session.history_labels().len(), before + 1);
    assert!((width(&h, id) - 25.0).abs() < 1e-9, "regenerated");
    assert!(
        h.state().session.document().object(id).is_some(),
        "same object"
    );
    still(&mut h, "edited");
    h.state_mut().undo();
    settle(&mut h);
    assert!((width(&h, id) - 10.0).abs() < 1e-9, "undone");
    h.state_mut().redo();
    settle(&mut h);
    assert!((width(&h, id) - 25.0).abs() < 1e-9, "redone");

    // Sink it into the ball and Combine: the box is baked.
    let scan = h.state().session.document().objects()[0].id;
    let mut t = h.state().session.document().object(id).unwrap().transform;
    t[11] -= 4.0;
    run(&mut h, "the sink-in", |a| {
        a.commit_transform(id, t, "sink".into())
    });
    run(&mut h, "the combine", |a| {
        a.run_invocation(
            mf_command::Invocation {
                op_id: "mesh.combine".into(),
                target: id,
                selection: None,
                operands: vec![("solids".to_owned(), scan, None)],
                raw_params: Default::default(),
            },
            "Combine".into(),
        )
    });
    h.state_mut().set_active(id);
    settle(&mut h);
    assert!(
        !h.state()
            .session
            .document()
            .object(id)
            .unwrap()
            .is_parametric()
    );
    h.get_by_label("Mesh");
    assert!(h.query_by_label("Editable shape · Box").is_none());
    still(&mut h, "baked");
}

fn recipe(h: &Harness<'_, MeshFixerApp>, id: mf_command::ObjectId) -> mf_parts::GeneratorRecord {
    mf_desktop::shape_panel::recipe(h.state(), id).unwrap()
}

/// Fix item 9: a hole defaults to Auto, re-evaluated when its transform
/// changes (one more step after the move, never after undo); a hole kept
/// Round while it prints sideways shows "Hole N prints sideways…" with
/// a one-click switch to Teardrop.
#[test]
fn a_sideways_hole_is_re_aimed_and_warned_about() {
    require_gpu!("a_sideways_hole_is_re_aimed_and_warned_about");
    let mut h = opened(vec![("scan", ball(), at([0.0, 0.0, 10.0]))]);
    let id = place(&mut h, "Magnet pocket");
    let r = recipe(&h, id);
    assert_eq!(r.params.get_choice("hole_profile").unwrap(), "auto");
    assert_eq!(
        r.params.get_float("profile").unwrap(),
        0.0,
        "upright: round"
    );

    // Turn it on its side: the move, then the re-aim.
    let mut t = h.state().session.document().object(id).unwrap().transform;
    // Rotate 90° about world Y: local z now runs along world X.
    let (x, z) = ([t[0], t[1], t[2]], [t[6], t[7], t[8]]);
    t[0..3].copy_from_slice(&[-z[0], -z[1], -z[2]]);
    t[6..9].copy_from_slice(&x);
    let before = h.state().session.history_labels().len();
    h.state_mut().commit_transform(id, t, "on its side".into());
    wait_until(&mut h, "the move and the re-aim", |a| {
        a.busy.is_none()
            && a.pending_reorient.is_none()
            && a.session.history_labels().len() >= before + 2
    });
    settle(&mut h);
    assert_eq!(h.state().session.history_labels().len(), before + 2);
    assert_eq!(
        recipe(&h, id).params.get_float("profile").unwrap(),
        1.0,
        "teardrop"
    );
    // Undo takes back the re-aim only, and doesn't trigger another.
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(recipe(&h, id).params.get_float("profile").unwrap(), 0.0);
    assert!(h.state().session.can_redo());
    h.state_mut().redo();
    settle(&mut h);

    // Kept Round on purpose: the warning and its fix.
    let before = h.state().session.history_labels().len();
    h.state_mut().set_active(id);
    mf_desktop::shape_panel::set_param(
        h.state_mut(),
        "hole_profile",
        mf_spec::ParamValue::Text("round".into()),
    );
    wait_until(&mut h, "kept round", |a| {
        a.busy.is_none() && a.session.history_labels().len() > before
    });
    h.get_by_label("Select").click();
    settle(&mut h);
    h.state_mut().set_active(id);
    settle(&mut h);
    h.get_by_label("Hole 1 prints sideways and will sag at the top; switch to Teardrop");
    still(&mut h, "sideways-warning");
    let before = h.state().session.history_labels().len();
    h.get_by_label("Switch to Teardrop").click();
    wait_until(&mut h, "switched to teardrop", |a| {
        a.busy.is_none() && a.session.history_labels().len() > before
    });
    settle(&mut h);
    assert_eq!(
        recipe(&h, id).params.get_choice("hole_profile").unwrap(),
        "teardrop"
    );
    assert!(
        h.query_by_label_contains("prints sideways").is_none(),
        "the warning is gone"
    );
}

/// I5 single-queue fix: a second edit fired before the first one's
/// worker job has returned (`app.busy` still `Some`) must never be
/// prepared against the same base revision as the first (the owner's
/// "document changed while the command ran" race). It queues instead,
/// coalesced onto the one op/target it's behind, and runs once that
/// base revision is current, so the document always ends up at the
/// last value asked for with no stale-revision error.
#[test]
fn rapid_slider_edits_queue_instead_of_racing() {
    require_gpu!("rapid_slider_edits_queue_instead_of_racing");
    let mut h = opened(vec![("scan", ball(), at([0.0, 0.0, 10.0]))]);
    let id = place(&mut h, "Box");
    settle(&mut h);
    let before = h.state().session.history_labels().len();

    // Fire several edits back to back, well before the first can have
    // returned from the worker pool (no `wait_until` between them).
    for w in [15.0, 20.0, 25.0, 30.0] {
        mf_desktop::shape_panel::set_param(
            h.state_mut(),
            "width",
            mf_spec::ParamValue::Float(w),
        );
    }
    assert!(h.state().error.is_none(), "no race error fired synchronously");
    wait_until(&mut h, "the coalesced edit to land", |a| {
        a.busy.is_none() && a.pending_invocation_depth() == 0
    });
    settle(&mut h);

    assert!(h.state().error.is_none(), "no stale-revision error");
    // Every queued edit after the first coalesced onto the one op/target
    // behind the running job: one more step beyond the first regenerate,
    // not one per edit.
    assert_eq!(
        h.state().session.history_labels().len(),
        before + 2,
        "the rapid drags left two undo steps (the first running, the rest coalesced into one)"
    );
    assert!((width(&h, id) - 30.0).abs() < 1e-9, "the last value won");
}

/// Two quick, distinct edits on two different objects (so there's no
/// question of which value should win on one of them): asking for the
/// second before the first has returned from the worker must still run
/// both, one after the other, never racing each other's base revision.
/// (Coalescing is for the *same* op on the *same* target, e.g. a
/// redragged slider; different targets always both run.)
#[test]
fn two_quick_edits_both_land_with_no_stale_error() {
    require_gpu!("two_quick_edits_both_land_with_no_stale_error");
    let mut h = opened(vec![("scan", ball(), at([0.0, 0.0, 10.0]))]);
    let a = place(&mut h, "Box");
    h.get_by_label("Select").click();
    settle(&mut h);
    let b = place(&mut h, "Magnet pocket");
    settle(&mut h);
    let before = h.state().session.history_labels().len();

    h.state_mut().set_active(a);
    mf_desktop::shape_panel::set_param(h.state_mut(), "width", mf_spec::ParamValue::Float(18.0));
    // Immediately, with object `a`'s width change still running on the
    // worker, edit a *different* object (as if the user had switched
    // selection a frame before the first edit's job returned):
    h.state_mut().set_active(b);
    mf_desktop::shape_panel::set_param(
        h.state_mut(),
        "magnet_diameter",
        mf_spec::ParamValue::Float(8.0),
    );
    assert!(h.state().error.is_none(), "no race error fired synchronously");
    wait_until(&mut h, "both edits to land", |a| {
        a.busy.is_none() && a.pending_invocation_depth() == 0
    });
    settle(&mut h);

    assert!(h.state().error.is_none(), "no stale-revision error");
    assert_eq!(
        h.state().session.history_labels().len(),
        before + 2,
        "both edits are their own undo step"
    );
    assert!((width(&h, a) - 18.0).abs() < 1e-9, "object a's own edit");
    assert_eq!(
        recipe(&h, b).params.get_float("magnet_diameter").unwrap(),
        8.0,
        "object b's own edit"
    );
}

/// Fix item 9's auto re-aim (`shape_panel::after_commit`) is queued
/// behind the move that triggers it rather than racing it: forcing the
/// move's `object.set_transform` and the re-aim's
/// `object.set_shape_params` to be asked for before either has
/// returned (by calling `commit_transform` and then immediately
/// queuing the re-aim's own invocation by hand) must still land both as
/// separate undo steps with no stale-revision error.
#[test]
fn move_then_reaim_never_races_even_back_to_back() {
    require_gpu!("move_then_reaim_never_races_even_back_to_back");
    let mut h = opened(vec![("scan", ball(), at([0.0, 0.0, 10.0]))]);
    let id = place(&mut h, "Magnet pocket");
    let before = h.state().session.history_labels().len();

    // Turn it on its side: the move, then the re-aim it triggers.
    let mut t = h.state().session.document().object(id).unwrap().transform;
    let (x, z) = ([t[0], t[1], t[2]], [t[6], t[7], t[8]]);
    t[0..3].copy_from_slice(&[-z[0], -z[1], -z[2]]);
    t[6..9].copy_from_slice(&x);
    h.state_mut().commit_transform(id, t, "on its side".into());
    // `pending_reorient` is only set once the move's own `Completed`
    // message is handled; nudge the queue the same way a second
    // frame's UI interaction would, straight after asking for the move,
    // well before it can have returned.
    assert!(h.state().busy.is_some(), "the move is still running");

    wait_until(&mut h, "the move and the re-aim", |a| {
        a.busy.is_none()
            && a.pending_reorient.is_none()
            && a.pending_invocation_depth() == 0
            && a.session.history_labels().len() >= before + 2
    });
    settle(&mut h);

    assert!(h.state().error.is_none(), "no stale-revision error");
    assert_eq!(
        h.state().session.history_labels().len(),
        before + 2,
        "the move and the re-aim are each their own undo step"
    );
    assert_eq!(
        recipe(&h, id).params.get_float("profile").unwrap(),
        1.0,
        "re-aimed to teardrop"
    );
}
