#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `meshfixer bench orbit` (MF-RENDER-002, MF-RT-005): a full orbit of the
//! damaged fixture rendered offscreen with the real renderer path, each
//! frame waited for on the GPU. A smoke test of the JSON's shape and
//! device tagging, never a gate on fps: CI runs it on lavapipe (a software
//! rasteriser). Heavy (1M triangles): run serially, see scripts/run-tests.sh.
//! With `MF_ARTIFACTS` set, the JSON is also written there as
//! `orbit-1m.json` (milestone evidence).

use std::process::Command;

#[test]
fn bench_orbit_json_has_device_info_and_percentiles() {
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable("bench_orbit_json_has_device_info_and_percentiles");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let metrics = dir.path().join("metrics.json");
    let out = Command::new(env!("CARGO_BIN_EXE_meshfixer"))
        .args([
            "bench",
            "orbit",
            "--triangles",
            "1000000",
            "--frames",
            "24",
            "--size",
            "640x360",
            "--json",
            "--metrics",
            metrics.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    eprintln!("{}", serde_json::to_string_pretty(&v).unwrap());

    // What was measured: requested (actual) triangles, frames, resolution.
    assert_eq!(v["requested_triangles"], 1_000_000, "{v}");
    let actual = v["triangles"].as_u64().unwrap();
    assert!((900_000..=1_000_000).contains(&actual), "{v}");
    assert_eq!(v["frames"], 24, "{v}");
    assert_eq!(v["resolution"]["width"], 640, "{v}");
    assert_eq!(v["resolution"]["height"], 360, "{v}");

    // What the frame time covers and leaves out (I2, P6).
    let measures = v["measures"].as_str().unwrap();
    assert!(
        measures.contains("4x MSAA") && measures.contains("excludes egui"),
        "{v}"
    );
    assert_eq!(v["msaa_samples"], 4, "{v}");
    assert!(
        v["overlay_layers"].as_str().unwrap().starts_with("none"),
        "{v}"
    );
    assert!(
        v["memory_estimate"]["bytes_per_requested_triangle"]
            .as_u64()
            .unwrap()
            > 0,
        "{v}"
    );

    // Device tags: CPU, GPU adapter, RAM, OS.
    let d = &v["device"];
    assert!(!d["cpu"]["model"].as_str().unwrap().is_empty(), "{v}");
    assert!(d["cpu"]["logical_cores"].as_u64().unwrap() >= 1, "{v}");
    assert!(!d["gpu"]["name"].as_str().unwrap().is_empty(), "{v}");
    assert!(!d["gpu"]["backend"].as_str().unwrap().is_empty(), "{v}");
    assert!(d["gpu"]["driver"].is_string(), "{v}");
    assert!(d["memory_bytes"].as_u64().unwrap() > 0, "{v}");
    assert!(!d["os"].as_str().unwrap().is_empty(), "{v}");

    // Whole-frame times (CPU through GPU completion) at p50/p95/p99, the
    // worst frame, and fps from the median.
    let f = &v["frame_ms"];
    let (p50, p95, p99, max) = (
        f["p50_ms"].as_f64().unwrap(),
        f["p95_ms"].as_f64().unwrap(),
        f["p99_ms"].as_f64().unwrap(),
        f["max_ms"].as_f64().unwrap(),
    );
    assert!(0.0 < p50 && p50 <= p95 && p95 <= p99 && p99 <= max, "{v}");
    assert_eq!(f["frames"], 24, "{v}");
    let fps = v["fps"].as_f64().unwrap();
    assert!((fps - 1000.0 / p50).abs() < 1e-6 * fps, "{v}");
    assert!(v["cpu_ms"]["p95_ms"].as_f64().unwrap() > 0.0, "{v}");
    // GPU timestamps: either unsupported, or counted (never a bare figure).
    let g = &v["gpu_ms"];
    if g["supported"] == true {
        let counted = g["measured"]["frames"].as_u64().unwrap_or(0)
            + g["unusable_frames"].as_u64().unwrap()
            + g["pending_frames"].as_u64().unwrap();
        assert_eq!(counted, 24, "{v}");
    } else {
        assert!(g["measured"].is_null(), "{v}");
    }

    // GPU buffers the orbit drew from, by kind.
    let b = &v["gpu_buffer_bytes"];
    assert!(b["geometry"].as_u64().unwrap() >= actual * 3 * 24, "{v}");
    assert!(
        b["lines"].as_u64().unwrap() > 0,
        "the fixture has holes: {v}"
    );
    assert!(b["state"].as_u64().unwrap() > 0, "{v}");
    assert!(b["textures"].as_u64().unwrap() >= 640 * 360 * 4, "{v}");

    // --metrics gains the same GPU bytes and frame percentiles, and the
    // memory breakdown with the process RSS.
    let m: serde_json::Value = serde_json::from_slice(&std::fs::read(&metrics).unwrap()).unwrap();
    assert_eq!(m["gpu_buffer_bytes"], v["gpu_buffer_bytes"], "{m}");
    assert_eq!(m["frame_ms"], v["frame_ms"], "{m}");
    assert_eq!(m["cpu_ms"], v["cpu_ms"], "{m}");
    let mem = &m["memory"];
    assert!(mem["mesh_storage_bytes"].as_u64().unwrap() > 0, "{m}");
    assert_eq!(mem["gpu"], v["gpu_buffer_bytes"], "{m}");
    if cfg!(any(target_os = "linux", target_os = "macos", windows)) {
        assert!(mem["process_rss_bytes"].as_u64().unwrap() > 0, "{m}");
    }
    if cfg!(target_os = "macos") {
        assert!(mem["phys_footprint_bytes"].as_u64().unwrap() > 0, "{m}");
        assert!(v["peak_phys_footprint_bytes"].as_u64().unwrap() > 0, "{v}");
    }
    // No operation ran, and the JSON says so rather than leaving a bare null.
    assert!(mem["scratch_peak_bytes"].is_null(), "{m}");
    assert_eq!(mem["scratch_peak_note"], "no operation run", "{m}");

    if let Ok(dir) = std::env::var("MF_ARTIFACTS") {
        std::fs::write(
            std::path::Path::new(&dir).join("orbit-1m.json"),
            serde_json::to_string_pretty(&v).unwrap(),
        )
        .unwrap();
    }
}
