#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Spatial-grid cost bounds (MF-ISSUE-026, MF-ISSUE-028).

use std::time::Instant;

use mf_fixtures::{translated, uv_sphere};
use mf_geometry::{equivalent, inspect};
use mf_metrics::{CountingAlloc, measure};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[test]
fn inspect_peak_per_vertex_is_bounded() {
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
    let a = translated(&uv_sphere(140, 280), [1e15, -2e15, 3e15]);
    let start = Instant::now();
    let r = equivalent(&a, &a, 1e-9);
    let elapsed = start.elapsed();
    assert!(r.within, "{r:?}");
    assert!(elapsed.as_secs_f64() < 3.0, "equivalence took {elapsed:?}");
}

#[test]
fn inspect_far_from_origin_does_not_overflow() {
    // Before the relative grid, coordinates / tolerance saturated i64 keys
    // and the neighbour search overflowed (panic in debug, wrap in release).
    let m = translated(&uv_sphere(8, 16), [1e14, -1e14, 0.0]);
    let d = inspect(&m);
    assert_eq!(d.duplicate_vertices_tolerance, 0);
    assert_eq!(d.vertex_count, m.vertex_count());
}
