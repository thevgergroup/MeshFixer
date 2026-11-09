#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The 3D view re-uploads only the GPU chunks an edit changed
//! (MF-RENDER-003), and what it draws always equals a full rebuild of the
//! revision shown: after a fill, undo, a branch and a checkout back.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui_wgpu;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mf_desktop::{MeshFixerApp, Settings};
use mf_fixtures::{DamageSpec, damaged_sphere};
use mf_geometry::{ContentHash, Mesh};
use mf_io::{Format, ImportedModel, SourceInfo, SourcePrecision};
use mf_render::{ChunkVertices, UploadStats};

fn model(mesh: Mesh) -> ImportedModel {
    ImportedModel {
        objects: vec![mf_io::threemf::ModelObject {
            name: "sphere".into(),
            mesh,
            transform: mf_io::threemf::IDENTITY,
        }],
        unit: mf_io::threemf::Unit::Millimeter,
        source: SourceInfo {
            path: "sphere.stl".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::StlBinary,
            precision: SourcePrecision::F32,
        },
        warnings: vec![],
    }
}

fn wait_shown(harness: &mut Harness<'_, MeshFixerApp>, what: &str) {
    let start = Instant::now();
    loop {
        let app = harness.state();
        if app.busy.is_none() && app.shows_current_revision() && app.current_analysis().is_some() {
            break;
        }
        assert!(app.error.is_none(), "{what}: {:?}", app.error);
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out waiting for: {what}"
        );
        harness.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.run_steps(2);
}

/// Reads every face chunk the view draws back from the GPU and compares it
/// with that chunk built from scratch; returns what its build uploaded.
fn assert_view_is_a_full_rebuild(
    app: &MeshFixerApp,
    rs: &egui_wgpu::RenderState,
    what: &str,
) -> UploadStats {
    let view = app.object_views().into_iter().find(|v| v.active).unwrap();
    let mesh = app.active_mesh().unwrap();
    assert!(
        Mesh::ptr_eq(&view.render.mesh, mesh),
        "{what}: current revision"
    );
    let gpu = &view.render.gpu;
    assert_eq!(gpu.chunk_count(), mesh.face_chunk_count(), "{what}");
    for i in 0..mesh.face_chunk_count() {
        let on_gpu = gpu.read_surface(&rs.device, &rs.queue, i).unwrap();
        assert!(
            on_gpu == ChunkVertices::build(mesh, i, gpu.origin).surface,
            "{what}: face chunk {i} differs from a full rebuild"
        );
    }
    gpu.last_upload()
}

#[test]
fn the_view_uploads_only_changed_chunks_and_matches_a_full_rebuild() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable(
            "the_view_uploads_only_changed_chunks_and_matches_a_full_rebuild",
        );
        return;
    }
    let mut setup = egui_kittest::wgpu::default_wgpu_setup();
    if let egui_wgpu::WgpuSetup::CreateNew(create) = &mut setup {
        create.device_descriptor = Arc::new(mf_desktop::gpu::device_descriptor);
    }
    let rs =
        egui_kittest::wgpu::create_render_state(setup, egui_wgpu::RendererOptions::PREDICTABLE);
    let mut harness = Harness::builder()
        .with_size((1440.0, 900.0))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::from_render_state(
            rs.clone(),
        ))
        .build_eframe(|cc| MeshFixerApp::with_settings(cc, None, Settings::default()).unwrap());
    // ~160k triangles (5 face chunks), six holes, a few loose pieces.
    let spec = DamageSpec {
        rings: 200,
        segments: 400,
        holes: 6,
        islands: 5,
        unwelded: 0,
        seed: 3,
    };
    harness
        .state_mut()
        .import_model(model(damaged_sphere(&spec)));
    wait_shown(&mut harness, "import");
    let opened = assert_view_is_a_full_rebuild(harness.state(), &rs, "opened");
    let full = opened.surface_bytes;
    let a = harness.state().session.current();

    harness.get_by_label("Fill all holes").click();
    harness.run_steps(2);
    harness.get_by_label("Apply").click();
    harness.step();
    wait_shown(&mut harness, "fill");
    let b = harness.state().session.current();
    assert_ne!(a, b, "the fill committed ({:?})", harness.state().status);
    let filled = assert_view_is_a_full_rebuild(harness.state(), &rs, "filled");
    eprintln!(
        "fill on {} triangles: {} of {full} surface bytes",
        harness.state().active_mesh().unwrap().triangle_count(),
        filled.surface_bytes
    );
    assert!(filled.surface_bytes > 0);
    assert!(
        filled.surface_bytes < full / 2,
        "the fill only appends: {filled:?} of {full}"
    );

    harness.get_by_label("Undo").click();
    wait_shown(&mut harness, "undo");
    assert_view_is_a_full_rebuild(harness.state(), &rs, "undone");

    // A branch off the original: its render mesh is built from what's on
    // screen (the original), not from the fill it doesn't descend from.
    harness.get_by_label("Remove loose pieces").click();
    harness.run_steps(2);
    harness.get_by_label("Apply").click();
    // Most of the pieces: it asks first (MF-ISSUE-052).
    harness.run_steps(2);
    harness.get_by_label("Remove them").click();
    harness.step();
    wait_shown(&mut harness, "branch");
    assert_ne!(harness.state().session.current(), a);
    let branch = assert_view_is_a_full_rebuild(harness.state(), &rs, "branch");
    assert!(branch.surface_bytes < full, "{branch:?} of {full}");

    harness.state_mut().checkout(b);
    wait_shown(&mut harness, "checkout");
    assert_eq!(harness.state().session.current(), b);
    assert_view_is_a_full_rebuild(harness.state(), &rs, "checked out the fill");
}
