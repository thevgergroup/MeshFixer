#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Peak heap of `fill_holes` with many large holes (M4 Task 4 review I2).
//! Each hole's triangulation table is O(n²); filling holes in parallel must
//! not hold one per worker at once. Its own binary: the counting allocator
//! is process-wide.

use std::collections::BTreeMap;

use mf_geometry::Mesh;
use mf_metrics::{CountingAlloc, measure};
use mf_operations::{RunContext, Target, find};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// An open cone: apex plus a planar rim of `n` vertices, so its one hole is
/// an `n`-edge loop.
fn cone(n: u32, offset: f64, positions: &mut Vec<[f64; 3]>, triangles: &mut Vec<[u32; 3]>) {
    let apex = positions.len() as u32;
    positions.push([offset, 0.0, 0.5]);
    for k in 0..n {
        let a = std::f64::consts::TAU * k as f64 / n as f64;
        positions.push([offset + a.cos(), a.sin(), 0.0]);
    }
    for k in 0..n {
        triangles.push([apex, apex + 1 + k, apex + 1 + (k + 1) % n]);
    }
}

#[test]
fn many_max_size_holes_fill_within_the_memory_budget() {
    const HOLES: usize = 12;
    const EDGES: u32 = 500; // the default max_loop_edges
    let (mut positions, mut triangles) = (Vec::new(), Vec::new());
    for i in 0..HOLES {
        cone(EDGES, 3.0 * i as f64, &mut positions, &mut triangles);
    }
    let mesh = Mesh::new(positions, triangles).unwrap();
    mesh.topology();
    let op = find("mesh.fill_holes").unwrap();
    let params = mf_spec::builtin::get("mesh.fill_holes")
        .unwrap()
        .validate(&BTreeMap::new())
        .unwrap();
    // As many workers as holes, whatever this machine has.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(HOLES)
        .build()
        .unwrap();
    let (out, phase) = measure("fill", || {
        pool.install(|| {
            op.execute(&mesh, &Target::WholeObject, &params, &RunContext::none())
                .unwrap()
        })
    });
    assert_eq!(out.report["filled_loops"].as_array().unwrap().len(), HOLES);
    // One table is 48·n² bytes (12 MB at 500 edges); twelve at once would
    // be 144 MB. Allow the triangulation budget plus the patches.
    let table = 48 * (EDGES as u64).pow(2);
    let bound = mf_operations::TRIANGULATION_BUDGET_BYTES + 2 * table;
    eprintln!(
        "fill peak {} MB (bound {} MB)",
        phase.peak_bytes >> 20,
        bound >> 20
    );
    assert!(
        phase.peak_bytes <= bound,
        "fill peak {} MB > bound {} MB",
        phase.peak_bytes >> 20,
        bound >> 20
    );
}
