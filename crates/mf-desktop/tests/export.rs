#![allow(clippy::unwrap_used)] // tests may panic on failure

//! STL/OBJ export of a placed object (M4 final review I3): the object's
//! transform is applied in the export job, not on the UI thread, so the
//! window keeps drawing, Cancel stops it with nothing left behind, and a
//! transform that overflows fails the export by name instead of crashing.

use std::time::{Duration, Instant};

use egui_kittest::Harness;
use mf_desktop::MeshFixerApp;
use mf_desktop::app::CANCELLED_EXPORT;
use mf_geometry::{ContentHash, Mesh};
use mf_io::threemf::{ModelObject, Unit};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};

const WINDOW: (f32, f32) = (1440.0, 900.0);
/// MF-RT-001: no UI frame over 50 ms.
const FRAME_BUDGET: Duration = Duration::from_millis(50);

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
            start.elapsed() < Duration::from_secs(120),
            "timed out waiting for: {what}"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The app with `mesh` open as one object placed by `transform`, analysed.
fn opened(mesh: Mesh, transform: [f64; 12]) -> Harness<'static, MeshFixerApp> {
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .wgpu()
        .build_eframe(|cc| MeshFixerApp::new(cc, None).unwrap());
    h.state_mut().import_model(ImportedModel {
        metadata: Default::default(),
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: "placed".into(),
            mesh,
            transform,
        }],
        unit: Unit::Millimeter,
        source: SourceInfo {
            path: "placed.3mf".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::ThreeMf,
            precision: SourcePrecision::F32,
            exporter_hint: None,
        },
        warnings: vec![],
    });
    wait_until(&mut h, "analysis", |a| a.current_analysis().is_some());
    h
}

/// A 2M-triangle object, rotated and moved: applying its transform takes
/// long enough to watch.
fn placed_big() -> (Mesh, [f64; 12]) {
    (
        mf_fixtures::grid(1000),
        mf_fixtures::rotation_translation(2, 30.0, [5.0, -3.0, 2.0]),
    )
}

/// Starts exporting to `out`; returns how long `export_to` held the UI
/// thread.
fn start_export(h: &mut Harness<'_, MeshFixerApp>, out: &std::path::Path) -> Duration {
    let start = Instant::now();
    h.state_mut().export_to(out.to_owned());
    let took = start.elapsed();
    assert!(h.state().busy.is_some(), "the export didn't start");
    took
}

#[test]
fn an_stl_export_applies_the_transform_in_the_job_while_the_ui_keeps_stepping() {
    require_gpu!("an_stl_export_applies_the_transform_in_the_job_while_the_ui_keeps_stepping");
    let (mesh, transform) = placed_big();
    let mut h = opened(mesh.clone(), transform);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("placed.stl");

    let held = start_export(&mut h, &out);
    assert!(
        held < FRAME_BUDGET,
        "export_to held the UI thread for {held:?}: the transform runs on it"
    );
    // The UI keeps stepping while the job applies the transform: frames
    // run with the export's own progress stage showing.
    let mut frames_transforming = 0;
    let start = Instant::now();
    while h.state().busy.is_some() {
        assert!(start.elapsed() < Duration::from_secs(120), "never finished");
        h.step();
        if h.state()
            .job_progress()
            .is_some_and(|(stage, _)| stage == mf_io::TRANSFORMING)
        {
            frames_transforming += 1;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        frames_transforming > 0,
        "no frame ran while the export job applied the transform"
    );
    let app = h.state();
    assert!(app.error.is_none(), "{:?}", app.error);
    assert!(
        app.status
            .as_deref()
            .is_some_and(|s| s.starts_with("Exported with each object's position applied")),
        "{:?}",
        app.status
    );
    // The file holds the placed positions.
    let back = mf_io::read_path(&out).unwrap().mesh;
    let want = mf_io::transform_mesh(&mesh, &transform, &mf_runtime::RunContext::none()).unwrap();
    let (b, w) = (back.bounding_box().unwrap(), want.bounding_box().unwrap());
    for i in 0..3 {
        assert!((b.min[i] - w.min[i]).abs() < 1e-3, "{b:?} vs {w:?}");
        assert!((b.max[i] - w.max[i]).abs() < 1e-3, "{b:?} vs {w:?}");
    }
}

#[test]
fn cancelling_an_export_while_it_applies_the_transform_leaves_nothing_behind() {
    require_gpu!("cancelling_an_export_while_it_applies_the_transform_leaves_nothing_behind");
    let (mesh, transform) = placed_big();
    let mut h = opened(mesh, transform);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("placed.obj");

    start_export(&mut h, &out);
    wait_until(&mut h, "the export applying the transform", |a| {
        assert!(
            a.busy.is_some(),
            "the export finished before it could be cancelled"
        );
        a.job_progress()
            .is_some_and(|(stage, _)| stage == mf_io::TRANSFORMING)
    });
    assert!(h.state().can_cancel());
    h.state_mut().cancel_job();
    wait_until(&mut h, "cancel", |a| a.busy.is_none());
    let app = h.state();
    assert_eq!(app.status.as_deref(), Some(CANCELLED_EXPORT));
    assert!(app.error.is_none(), "{:?}", app.error);
    let left: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert!(left.is_empty(), "a cancelled export left {left:?}");
}

#[test]
fn an_export_whose_transform_overflows_fails_by_name() {
    require_gpu!("an_export_whose_transform_overflows_fails_by_name");
    // The grid's corners reach 4: scaled by 1e308 they overflow.
    let s = 1e308;
    let transform = [s, 0.0, 0.0, 0.0, s, 0.0, 0.0, 0.0, s, 0.0, 0.0, 0.0];
    let mut h = opened(mf_fixtures::grid(4), transform);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("huge.stl");

    start_export(&mut h, &out);
    wait_until(&mut h, "the export", |a| a.busy.is_none());
    let app = h.state();
    let error = app.error.as_deref().unwrap_or_default();
    assert!(
        error.starts_with("Export failed: the object's transform puts vertex"),
        "{error:?}"
    );
    assert!(
        !error.contains("Choose another location"),
        "another location won't help: {error:?}"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
