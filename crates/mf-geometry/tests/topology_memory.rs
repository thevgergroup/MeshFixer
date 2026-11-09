#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The topology build's transient memory at 5M requested triangles (the
//! damaged-sphere fixture cuts holes, leaving ≈4.8M; asserted below) (MF-GEO-003,
//! MF-ISSUE-019). Its own binary: the counting allocator is process-wide.
//! Heavy (a 5M mesh): run serially, see scripts/run-tests.sh.

use mf_fixtures::{DamageSpec, damaged_sphere};
use mf_geometry::Topology;
use mf_metrics::{CountingAlloc, measure};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// MF-ISSUE-019: building the topology of a 5M-requested (≈4.8M actual)
/// mesh peaks at most
/// this far above what was live before it started.
const TOPOLOGY_PEAK_BUDGET: u64 = 150 << 20;

#[test]
fn topology_build_peak_under_150mb_at_5m() {
    let mesh = damaged_sphere(&DamageSpec::for_triangles(5_000_000, 11));
    assert!(mesh.triangle_count() >= 4_800_000, "{mesh:?}");
    let (topo, phase) = measure("topology", || Topology::compute(&mesh));
    eprintln!(
        "topology build at {} triangles: peak {} MB above the mesh, retained {} MB ({} MB by heap_bytes), {:.2} s",
        mesh.triangle_count(),
        phase.peak_bytes >> 20,
        phase.retained_bytes >> 20,
        topo.heap_bytes() >> 20,
        phase.wall_s
    );
    assert!(
        phase.peak_bytes <= TOPOLOGY_PEAK_BUDGET,
        "topology build peak {} MB > {} MB",
        phase.peak_bytes >> 20,
        TOPOLOGY_PEAK_BUDGET >> 20
    );
    // The damaged sphere: the main body and 50 islands; 12 holes and 200
    // slits around unwelded corners.
    assert_eq!(topo.components.len(), 51);
    assert_eq!(topo.boundary_loops.len(), 212);
}
