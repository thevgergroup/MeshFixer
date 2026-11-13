#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Worker failures are reported, never left looking like work in progress.

use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::worker::Message;
use mf_desktop::{MeshFixerApp, Settings};
use mf_geometry::ContentHash;
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_runtime::{Cancelled, RunContext};

fn panicking(
    _: mf_desktop::worker::AnalysisRequest,
    _: &RunContext,
    _: &mf_desktop::worker::Gpu,
) -> Result<Message, Cancelled> {
    panic!("injected analysis failure")
}

fn cube_model() -> ImportedModel {
    ImportedModel {
        objects: vec![mf_io::threemf::ModelObject {
            name: "cube".into(),
            mesh: mf_fixtures::cube(1.0),
            transform: mf_io::threemf::IDENTITY,
        }],
        unit: mf_io::threemf::Unit::Millimeter,
        source: SourceInfo {
            path: "cube.stl".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::StlBinary,
            precision: SourcePrecision::F32,
        },
        warnings: vec![],
    }
}

/// A panic in the analysis says so in plain words, and the app stops
/// saying "Analysing…" (it never would have finished).
#[test]
fn an_analysis_panic_is_reported_not_left_analysing() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("an_analysis_panic_is_reported_not_left_analysing");
        return;
    }
    let settings = Settings {
        analyze: Some(panicking),
        ..Settings::default()
    };
    let mut harness = Harness::builder()
        .with_size((1440.0, 900.0))
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::with_settings(cc, None, settings).unwrap());
    harness.state_mut().import_model(cube_model());
    let start = Instant::now();
    while harness.state().error.is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "no error reported"
        );
        harness.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.run_steps(3);
    let app = harness.state();
    let error = app.error.clone().unwrap();
    assert!(
        error.starts_with("Couldn't analyse this object")
            && error.contains("injected analysis failure"),
        "{error}"
    );
    assert!(
        !app.is_analyzing(),
        "still analysing after the analysis failed"
    );
    assert!(
        harness.query_by_label("Analysing…").is_none(),
        "the status line still says Analysing…"
    );
    // The panels say why there's nothing to show, as well as the banner.
    assert!(
        harness
            .get_all_by_label_contains("Couldn't analyse this object")
            .count()
            >= 2
    );
}
