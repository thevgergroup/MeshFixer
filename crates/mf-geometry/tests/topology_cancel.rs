#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The topology build checkpoints at least every 100 ms (MF-RT-002) even
//! when one vertex carries most of the mesh's edges (M4 Task 9 review P1):
//! its bucket of half-edges must not be sorted in one uninterruptible go.
//! A timing test: run alone, see scripts/run-tests.sh.

use std::time::Duration;

use mf_geometry::{Mesh, Topology};
use mf_runtime::{JobPool, RunContext};

const CHECKPOINT_BUDGET: Duration = Duration::from_millis(100);

/// A fan of `n` triangles around hub vertex 0, its rim visited in a
/// scrambled order, so the hub's edges arrive in no particular order.
fn hub_fan(n: u32) -> Mesh {
    let mut positions = vec![[0.0, 0.0, 1.0]];
    for i in 0..n {
        let a = std::f64::consts::TAU * i as f64 / n as f64;
        positions.push([a.cos(), a.sin(), 0.0]);
    }
    // A full-period LCG over 0..n (n a power of two) scrambles the rim.
    assert!(n.is_power_of_two());
    let rim = |i: u32| 1 + (i.wrapping_mul(2_654_435_761 | 1).wrapping_add(12_345) & (n - 1));
    let triangles = (0..n).map(|i| [0, rim(i), rim((i + 1) % n)]).collect();
    Mesh::new(positions, triangles).unwrap()
}

#[test]
fn topology_checkpoints_every_100ms_around_a_million_face_hub() {
    let mesh = hub_fan(1 << 20);
    let (pool, _) = JobPool::new(Some(1));
    let (ctx, probe) = RunContext::instrumented();
    let topo = pool
        .install(|| Topology::compute_with(&mesh, &ctx))
        .unwrap();
    assert_eq!(topo.components.len(), 1);
    let worst = probe.worst().unwrap();
    eprintln!(
        "hub fan of {} faces, 1 thread: {} checkpoints, worst gap {:?} ({:?} -> {})",
        mesh.face_count(),
        probe.checkpoints(),
        worst.gap,
        worst.from.map(|l| l.to_string()),
        worst.to
    );
    assert!(
        worst.gap <= CHECKPOINT_BUDGET,
        "worst gap {:?} > {CHECKPOINT_BUDGET:?}, ending at {}",
        worst.gap,
        worst.to
    );
}
