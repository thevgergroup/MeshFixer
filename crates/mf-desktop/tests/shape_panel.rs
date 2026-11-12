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
