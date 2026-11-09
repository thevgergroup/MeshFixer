#![allow(clippy::unwrap_used)] // tests may panic on failure

//! What building a BVH costs in memory (MF-RENDER-004): the tree itself is
//! ≤ 64 B/face (tests/bvh.rs); this bounds the build's peak, scratch
//! included, measured with the counting allocator.

use mf_fixtures::{DamageSpec, damaged_sphere};
use mf_metrics::{CountingAlloc, measure};
use mf_render::Bvh;
use mf_runtime::RunContext;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[test]
fn bvh_build_peak_is_bounded() {
    // Big enough that subtrees are built in parallel and merged.
    let mesh = damaged_sphere(&DamageSpec::for_triangles(1_000_000, 4));
    let faces = mesh.triangle_count() as f64;
    let (bvh, phase) = measure("bvh build", || {
        Bvh::build(&mesh, &RunContext::none()).unwrap()
    });
    let peak = phase.peak_bytes as f64 / faces;
    let kept = phase.retained_bytes as f64 / faces;
    println!(
        "BVH build at {faces} faces: peak {} B ({peak:.1} B/face), kept {} B ({kept:.1} B/face), \
         tree {} B",
        phase.peak_bytes,
        phase.retained_bytes,
        bvh.heap_bytes()
    );
    // Scratch is one 28-byte item per face, plus the tree as it's built.
    assert!(peak <= 52.0, "build peak {peak:.1} B/face");
    assert!(kept <= 24.0, "kept {kept:.1} B/face");
}
