#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Spatial-grid cost bounds (MF-ISSUE-026, MF-ISSUE-028).

use std::time::Instant;

use mf_fixtures::{translated, uv_sphere};
use mf_geometry::{equivalent, inspect};
use mf_metrics::{CountingAlloc, measure};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// The allocator counters are process-wide: every test in this binary runs
/// serially so a measurement never includes another test's allocations.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|p| p.into_inner())
}

#[test]
fn inspect_peak_per_vertex_is_bounded() {
    let _serial = serial();
    let m = uv_sphere(300, 600); // ~180k vertices, ~360k faces
    let _ = m.topology(); // measure diagnostics only
    let (_, phase) = measure("inspect", || inspect(&m));
    let per_vertex = phase.peak_bytes as f64 / m.vertex_count() as f64;
    eprintln!(
        "inspect peak: {} B ({per_vertex:.0} B/vertex)",
        phase.peak_bytes
    );
    assert!(per_vertex < 80.0, "inspect peak {per_vertex:.0} B/vertex");
}

#[test]
fn equivalence_stays_fast_with_offset_coordinates_and_tiny_tolerance() {
    let _serial = serial();
    // Before the relative grid, offset coordinates with a tiny tolerance put
    // every point in one saturated cell: O(n²), i.e. minutes here. Compare
    // against the same mesh at the origin under the same load instead of a
    // wall-clock cap, so the test doesn't depend on machine speed.
    let base = uv_sphere(140, 280);
    let timed = |m: &mf_geometry::Mesh| {
        let start = Instant::now();
        let r = equivalent(m, m, 1e-9);
        assert!(r.within, "{r:?}");
        start.elapsed().as_secs_f64()
    };
    let at_origin = timed(&base);
    let offset = timed(&translated(&base, [1e15, -2e15, 3e15]));
    assert!(
        offset < 4.0 * at_origin + 0.5,
        "offset {offset:.2}s vs origin {at_origin:.2}s"
    );
}

#[test]
fn inspect_far_from_origin_does_not_overflow() {
    let _serial = serial();
    // Before the relative grid, coordinates / tolerance saturated i64 keys
    // and the neighbour search overflowed (panic in debug, wrap in release).
    let m = translated(&uv_sphere(8, 16), [1e14, -1e14, 0.0]);
    let d = inspect(&m);
    assert_eq!(d.duplicate_vertices_tolerance, 0);
    assert_eq!(d.vertex_count, m.vertex_count());
}
