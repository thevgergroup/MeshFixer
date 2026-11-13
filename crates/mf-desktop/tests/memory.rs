#![allow(clippy::unwrap_used)] // tests may panic on failure

//! What a realistic session on 5M requested triangles (the damaged-sphere
//! fixture leaves ≈4.8M) holds (M4 Task 9, MF-GEO-002): open,
//! fill holes, undo, redo, select. Reported, not gated (yet): the process's
//! RSS and the GPU buffers, next to the counted storage figure the 400 MB
//! gate is about (`meshfixer bench memory`), so the two can't be confused.
//! With `MF_ARTIFACTS` set, the figures are also written there as
//! `memory-session.json` (milestone evidence).
//!
//! Its own binary (the counting allocator is process-wide), and heavy: run
//! serially, see scripts/run-tests.sh.

use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::MeshFixerApp;
use mf_fixtures::{DamageSpec, damaged_sphere};
use mf_geometry::ContentHash;
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_metrics::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const BIG: usize = 5_000_000;

fn big_model() -> ImportedModel {
    ImportedModel {
        objects: vec![mf_io::threemf::ModelObject {
            name: "big".into(),
            mesh: damaged_sphere(&DamageSpec::for_triangles(BIG, 11)),
            transform: mf_io::threemf::IDENTITY,
        }],
        unit: mf_io::threemf::Unit::Millimeter,
        source: SourceInfo {
            path: "big.stl".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::StlBinary,
            precision: SourcePrecision::F32,
        },
        warnings: vec![],
    }
}

fn wait_until(h: &mut Harness<'_, MeshFixerApp>, what: &str, cond: impl Fn(&MeshFixerApp) -> bool) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(
            start.elapsed() < Duration::from_secs(600),
            "timed out waiting for: {what}"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
}

/// Done with a step: its revision is shown, analysed and pickable, and
/// nothing is running.
fn settled(a: &MeshFixerApp) -> bool {
    a.busy.is_none()
        && a.shows_current_revision()
        && a.current_analysis().is_some()
        && a.picking_ready()
        && !a.is_analyzing()
}

struct Sample {
    step: &'static str,
    /// The current revision's mesh plus its cached topology, counted
    /// (`Mesh::heap_bytes` + `Topology::heap_bytes`): what the 400 MB gate
    /// measures.
    storage_bytes: u64,
    /// Every revision in history, shared chunks counted once.
    history_bytes: u64,
    /// Everything the app has allocated (counting allocator).
    live_heap_bytes: u64,
    /// GPU buffers of the objects on screen.
    gpu_bytes: u64,
    /// From the OS; `None` with the reason in `rss_error`.
    process_rss_bytes: Option<u64>,
    rss_error: Option<String>,
}

impl Sample {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "step": self.step,
            "storage_bytes": self.storage_bytes,
            "history_bytes": self.history_bytes,
            "live_heap_bytes": self.live_heap_bytes,
            "gpu_bytes": self.gpu_bytes,
            "process_rss_bytes": self.process_rss_bytes,
            "rss_error": self.rss_error,
        })
    }
}

fn sample(step: &'static str, app: &MeshFixerApp) -> Sample {
    let mesh = app.active_mesh().unwrap();
    let storage = mesh.heap_bytes() + mesh.topology().heap_bytes();
    let gpu = app
        .object_views()
        .iter()
        .map(|v| v.render.gpu.resident_bytes())
        .sum();
    let (rss, rss_error) = match mf_metrics::process_rss_bytes() {
        Ok(b) => (Some(b), None),
        Err(e) => (None, Some(e)),
    };
    let s = Sample {
        step,
        storage_bytes: storage,
        history_bytes: app.session.history_bytes(),
        live_heap_bytes: mf_metrics::live_bytes(),
        gpu_bytes: gpu,
        process_rss_bytes: rss,
        rss_error,
    };
    let mb = |b: u64| format!("{} MB", b >> 20);
    eprintln!(
        "{step:>8}: storage (gated, 400 MB) {} | history {} | live heap {} | GPU buffers {} | process RSS {}",
        mb(s.storage_bytes),
        mb(s.history_bytes),
        mb(s.live_heap_bytes),
        mb(s.gpu_bytes),
        s.process_rss_bytes.map_or_else(
            || format!("unavailable ({})", s.rss_error.as_deref().unwrap_or("")),
            mb
        ),
    );
    s
}

#[test]
fn realistic_session_rss_is_reported() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("realistic_session_rss_is_reported");
        return;
    }
    let mut h = Harness::builder()
        .with_size((1440.0, 900.0))
        .wgpu()
        .build_eframe(|cc| MeshFixerApp::new(cc, None).unwrap());
    h.state_mut().import_model(big_model());
    wait_until(&mut h, "5M open + analysis", |a| {
        settled(a) && a.viewport.frames_rendered > 0
    });
    let mut samples = vec![sample("open", h.state())];

    let labels = h.state().session.history_labels().len();
    h.get_by_label("Fill all holes").click();
    h.run_steps(2);
    h.get_by_label("Apply").click();
    wait_until(&mut h, "fill holes", |a| {
        a.session.history_labels().len() == labels + 1 && settled(a)
    });
    assert!(h.state().error.is_none(), "{:?}", h.state().error);
    samples.push(sample("fill", h.state()));

    let filled = h.state().session.current();
    h.state_mut().undo();
    wait_until(&mut h, "undo", |a| {
        a.session.current() != filled && settled(a)
    });
    samples.push(sample("undo", h.state()));

    h.state_mut().redo();
    wait_until(&mut h, "redo", |a| {
        a.session.current() == filled && settled(a)
    });
    samples.push(sample("redo", h.state()));

    let piece = h.state().current_analysis().unwrap().defects.loose[0].clone();
    h.state_mut().select_defect(&piece);
    wait_until(&mut h, "select", |a| {
        a.current_selection().is_some() && settled(a)
    });
    h.run_steps(5);
    samples.push(sample("select", h.state()));

    // Reported, not gated: every step has its figures, and the RSS is
    // there wherever the OS gives one.
    for s in &samples {
        assert!(s.storage_bytes > 0 && s.gpu_bytes > 0, "{}", s.step);
        if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
            assert!(
                s.process_rss_bytes.is_some(),
                "{}: {:?}",
                s.step,
                s.rss_error
            );
        }
    }
    if let Ok(dir) = std::env::var("MF_ARTIFACTS") {
        let json = serde_json::json!({
            "triangles": h.state().active_mesh().unwrap().triangle_count(),
            "storage_budget_bytes": 400u64 << 20,
            "note": "storage_bytes is what the 400 MB gate measures (mesh + cached topology); process RSS and GPU buffers are reported, not gated",
            "steps": samples.iter().map(Sample::json).collect::<Vec<_>>(),
        });
        std::fs::write(
            std::path::Path::new(&dir).join("memory-session.json"),
            serde_json::to_string_pretty(&json).unwrap(),
        )
        .unwrap();
    }
}
