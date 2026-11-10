#![allow(clippy::unwrap_used)] // tests may panic on failure

//! View ▸ Metrics (MF-RT-005): frame times at p50/p95/p99 with fps, the
//! last operations' durations, peak heap, the memory breakdown (mesh
//! storage, cached topology, the last operation's peak, GPU buffers by
//! kind, process RSS), the bytes uploaded by the last change, and the
//! thread budget with its reason. Plain-language labels, engineering
//! numbers. With `MF_ARTIFACTS` set, a screenshot is kept there as
//! `metrics-panel.png`.
//!
//! Its own binary: the counting allocator (peak heap) is process-wide.

use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use mf_desktop::MeshFixerApp;
use mf_desktop::metrics::{format_bytes, format_duration};
use mf_metrics::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fn wait_until(h: &mut Harness<'_, MeshFixerApp>, what: &str, cond: impl Fn(&MeshFixerApp) -> bool) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out waiting for: {what}"
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
}

/// The text of a node (a `Role::Label` keeps it in its value).
fn text(n: &egui_kittest::Node<'_>) -> String {
    let n = n.accesskit_node();
    n.value().or_else(|| n.label()).unwrap_or_default()
}

/// The text of the one label containing `part`.
fn label(h: &Harness<'_, MeshFixerApp>, part: &str) -> String {
    text(&h.get_by_label_contains(part))
}

#[test]
fn metrics_panel_shows_gpu_bytes_and_thread_budget() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("metrics_panel_shows_gpu_bytes_and_thread_budget");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("damaged.stl");
    let mesh = mf_fixtures::damaged_sphere(&mf_fixtures::DamageSpec::for_triangles(20_000, 3));
    mf_io::write_path(
        &mesh,
        &path,
        mf_io::Format::StlBinary,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();
    let mut h = Harness::builder()
        .with_size((1440.0, 900.0))
        .wgpu()
        .build_eframe(move |cc| MeshFixerApp::new(cc, Some(path.clone())).unwrap());
    wait_until(&mut h, "import + analysis", |a| {
        a.busy.is_none() && a.current_analysis().is_some() && a.shows_current_revision()
    });

    h.get_by_label("View").click();
    h.run_steps(2);
    h.get_by_label("Metrics").click();
    h.run_steps(5);
    assert!(h.state().show_metrics);

    // The process RSS is sampled off the UI thread: wait for the first.
    wait_until(&mut h, "an RSS sample", |a| {
        a.metrics_snapshot()
            .is_some_and(|m| m.memory.process_rss_bytes.is_some() || m.memory.rss_error.is_some())
    });
    // The figures shown are the app's cached ones; the holes' outlines
    // are on the GPU.
    let m = h
        .state()
        .metrics_snapshot()
        .expect("refreshed while shown")
        .clone();
    let gpu = m.memory.gpu.expect("the mesh is on the GPU");
    assert!(
        gpu.geometry > 0 && gpu.state > 0 && gpu.lines > 0 && gpu.textures > 0,
        "{gpu:?}"
    );

    let buffers = label(&h, "GPU buffers:");
    for (kind, bytes) in [
        ("geometry", gpu.geometry),
        ("face states", gpu.state),
        ("outlines", gpu.lines),
        ("view textures", gpu.textures),
    ] {
        assert!(
            buffers.contains(&format!("{kind} {}", format_bytes(bytes))),
            "{kind}: {buffers}"
        );
    }
    // An operation (with the panel open), so there's a duration and a
    // peak to show; the snapshot catches up within half a second.
    // Repair holds the whole-object repairs (ui-plan.md §7).
    h.get_by_label("Repair").click();
    h.run_steps(2);
    h.get_by_label("Fill all holes").click();
    h.run_steps(2);
    h.get_by_label("Apply").click();
    wait_until(&mut h, "fill holes", |a| {
        a.busy.is_none() && a.shows_current_revision() && a.current_analysis().is_some()
    });
    assert!(h.state().error.is_none(), "{:?}", h.state().error);

    wait_until(&mut h, "the fill in the snapshot", |a| {
        a.metrics_snapshot()
            .is_some_and(|m| m.jobs.iter().any(|j| j.label == "Fill all holes"))
    });
    let m = h.state().metrics_snapshot().unwrap().clone();
    let uploaded = label(&h, "Uploaded by the last change:");
    assert!(
        uploaded.contains(&format_bytes(m.last_upload_bytes)),
        "{uploaded}"
    );

    let pools = h.state().pool_sizes();
    let budget = label(&h, "Thread budget:");
    assert!(
        budget.contains(&format!(
            "{} of {} cores",
            pools.budget.threads, pools.budget.cores
        )) && budget.contains(pools.budget.reason),
        "{budget}"
    );

    // Frame times with fps, the last operation, peak heap, the memory
    // breakdown with process RSS.
    // fps only over frames redrawn back to back, said to be a redraw rate.
    let redraw = label(&h, "Redraw rate:");
    assert!(
        redraw.contains("only redraws when something changes"),
        "{redraw}"
    );
    assert!(label(&h, "CPU per frame:").contains("p95"));
    // GPU time comes with its counts, or says why there is none.
    let gpu_line = h
        .query_by_label_contains("GPU p95")
        .or_else(|| h.query_by_label_contains("GPU time:"))
        .map(|n| text(&n))
        .expect("a GPU time line");
    assert!(
        gpu_line.contains(" of ")
            || gpu_line.contains("not supported")
            || gpu_line.contains("no frames"),
        "{gpu_line}"
    );
    let fill = m.jobs.iter().find(|j| j.label == "Fill all holes").unwrap();
    let fill_line = label(&h, "Fill all holes:");
    assert!(
        fill_line.contains(&format_duration(fill.duration)),
        "{fill_line}"
    );
    assert!(fill_line.contains("heap peak +"), "{fill_line}");
    assert!(label(&h, "Peak heap:").contains("MB"));
    assert!(label(&h, "Mesh storage:").contains(&format_bytes(m.memory.mesh_storage_bytes)));
    assert!(
        label(&h, "Cached topology:").contains("MB")
            || label(&h, "Cached topology:").contains("KB")
    );
    assert!(label(&h, "Last operation's peak:").contains("above its start"));
    // Resident memory says what it leaves out; macOS adds the footprint.
    let rss = label(&h, "Resident memory (RSS");
    assert!(rss.contains("excludes swapped/compressed"), "{rss}");
    if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
        assert!(rss.contains("MB"), "{rss}");
    }
    if cfg!(target_os = "macos") {
        let footprint = label(&h, "Memory footprint:");
        assert!(
            footprint.contains(&format_bytes(m.memory.phys_footprint_bytes.unwrap())),
            "{footprint}"
        );
    }

    // P4: a job shows its file's name, not a temporary path.
    let import = label(&h, "Importing ");
    assert!(
        import.starts_with("Importing damaged.stl:") && !import.contains('/'),
        "{import}"
    );

    // I3: while open, the panel refreshes even when nothing else repaints.
    h.run_steps(3);
    let delay = h.output().viewport_output[&egui::ViewportId::ROOT].repaint_delay;
    assert!(
        delay <= mf_desktop::metrics::REFRESH_EVERY,
        "the panel asks for a repaint within {:?}, not {delay:?}",
        mf_desktop::metrics::REFRESH_EVERY
    );

    // P5: it opens over the 3D view, not over the side panels.
    let view = h.state().viewport.last_rect.unwrap();
    let window = h.get_by_label("Metrics").rect();
    assert!(
        window.left() >= view.left() && window.top() >= view.top() - 1.0,
        "window {window:?}, view {view:?}"
    );

    if let Ok(dir) = std::env::var("MF_ARTIFACTS") {
        h.render()
            .unwrap()
            .save(std::path::Path::new(&dir).join("metrics-panel.png"))
            .unwrap();
    }
}
