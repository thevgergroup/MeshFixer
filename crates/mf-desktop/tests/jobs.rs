#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The one job system's lanes in the running app (ADR-0009,
//! consolidation plan C19): a panicking preview is reported and the app
//! carries on; closing a tool cancels its preview; a preview is shown
//! only for the settings asked for now; `settle()` waits for every lane.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::{ContentHash, Mesh};
use mf_io::threemf::{ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_runtime::RunContext;

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
            "timed out waiting for: {what} (status {:?}, error {:?})",
            h.state().status,
            h.state().error,
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

fn opened(mesh: Mesh) -> (Harness<'static, MeshFixerApp>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let settings = Settings {
        config_dir: Some(dir.path().to_owned()),
        ..Settings::default()
    };
    let mut h = Harness::builder()
        .with_size((1440.0, 1000.0))
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::with_settings(cc, None, settings).unwrap());
    h.state_mut().import_model(ImportedModel {
        metadata: Default::default(),
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: "ball".into(),
            mesh,
            transform: mf_io::threemf::IDENTITY,
        }],
        unit: Some(Unit::Millimeter),
        source: SourceInfo {
            path: "jobs.3mf".into(),
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
    (h, dir)
}

fn ball() -> Mesh {
    mf_fixtures::scaled(&mf_fixtures::uv_sphere(32, 48), 20.0)
}

/// A preview whose work panics is reported in plain words (the error
/// banner, as any worker failure), never aborting the app; the slot then
/// runs the next request, and the app goes on handling frames and
/// commands.
#[test]
fn a_panicking_preview_reports_and_the_app_survives() {
    require_gpu!("a_panicking_preview_reports_and_the_app_survives");
    let (mut h, _dir) = opened(ball());
    let got: Arc<Mutex<Vec<u32>>> = Arc::default();
    let out = got.clone();
    let slot = h.state().jobs().preview(
        "test preview",
        |n: u32, _: &RunContext| {
            assert!(n != 0, "injected preview failure");
            Ok(n)
        },
        move |_, n| out.lock().unwrap().push(n),
    );
    slot.submit(0);
    wait_until(&mut h, "the failure reported", |a| a.error.is_some());
    let error = h.state().error.clone().unwrap();
    assert!(
        error.contains("test preview") && error.contains("injected preview failure"),
        "{error}"
    );
    // The slot (and the pool thread under it) carries on.
    slot.submit(7);
    let start = Instant::now();
    while got.lock().unwrap().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(10), "no next result");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(*got.lock().unwrap(), [7]);
    // And so does the app: a command still runs and commits.
    let history = h.state().session.history_labels().len();
    h.state_mut().drop_to_plate();
    wait_until(&mut h, "the command", |a| {
        a.busy.is_none() && a.session.history_labels().len() > history
    });
    settle(&mut h);
}

/// Closing Hollow cancels the preview it asked for (ADR-0009: a preview
/// is cancelled when its owner tool closes), however it was closed.
#[test]
fn closing_a_tool_cancels_its_preview() {
    require_gpu!("closing_a_tool_cancels_its_preview");
    let (mut h, _dir) = opened(ball());
    let hollow = mf_desktop::hollow_tool::HOLLOW;
    h.state_mut().open_rail_tool(hollow);
    wait_until(&mut h, "Hollow's preview asked for", |a| {
        a.previews_owned_by(hollow) == 1
    });
    // Closed straight away, while its preview may still be running.
    h.state_mut().close_rail_tool();
    h.step();
    assert_eq!(
        h.state().previews_owned_by(hollow),
        0,
        "Hollow closed but its preview is still owned (not cancelled)"
    );
    settle(&mut h);
    assert!(
        h.state().error.is_none(),
        "cancelling reported a failure: {:?}",
        h.state().error
    );
}

/// After a peg setting changes, the Alignment preview never shows the
/// answer for the old settings: only one worked out for the settings
/// asked for now (ADR-0009: accepted on a matching key and base).
#[test]
fn a_peg_preview_never_shows_stale_settings() {
    require_gpu!("a_peg_preview_never_shows_stale_settings");
    let (mut h, _dir) = opened(ball());
    h.state_mut().open_rail_tool(mf_desktop::shape_tools::SPLIT);
    h.state_mut().alignment.pegs = "dowels".into();
    wait_until(&mut h, "the first peg preview", |a| {
        a.peg_preview().is_some()
    });
    let old = h.state().alignment.hole_diameter;
    let new = old * 0.6;
    h.state_mut().alignment.hole_diameter = new;
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(60), "no new preview");
        h.step();
        let app = h.state();
        if let Some(shown) = app.peg_preview_settings() {
            assert_eq!(
                shown.diameter, new,
                "the preview for the old {old} diameter still shows"
            );
            if !app.background_work_in_flight() {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    // Leaving Split stops it and clears it.
    h.state_mut().close_rail_tool();
    settle(&mut h);
    assert!(h.state().peg_preview().is_none());
    assert_eq!(
        h.state().previews_owned_by(mf_desktop::shape_tools::SPLIT),
        0
    );
}

/// `settle()` (no work in flight, no repaint asked for) waits for the
/// background lane's one-shot tasks too, not only the job and pick
/// pools: a task still running counts as work in flight.
#[test]
fn settle_counts_every_lane() {
    require_gpu!("settle_counts_every_lane");
    let (mut h, _dir) = opened(ball());
    let done = Arc::new(Mutex::new(false));
    let out = done.clone();
    let jobs = h.state().jobs().clone();
    jobs.once(
        &mut None,
        |_| {
            std::thread::sleep(Duration::from_millis(300));
            Ok(())
        },
        move |_| *out.lock().unwrap() = true,
    );
    assert!(
        h.state().background_work_in_flight(),
        "a running one-shot task isn't counted"
    );
    settle(&mut h);
    assert!(
        *done.lock().unwrap(),
        "settle() returned before the task ran"
    );
}
