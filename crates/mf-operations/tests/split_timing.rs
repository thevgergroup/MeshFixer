#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Timing gates for `mesh.split_plane` (Phase 1 Task 12): a split of a 5M
//! model in under 2 s and of a 30M model in under 10 s, on a bias plane
//! with a cut gap (both halves cut and capped). Heavy: on demand only
//! (`scripts/run-tests.sh` timing list), each alone.
//!
//! The model's topology is computed first, as the app's analysis already
//! has when the Split tool opens; the gate times the split itself.

use std::time::{Duration, Instant};

use mf_document::ObjectId;
use mf_fixtures::uv_sphere;
use mf_io::threemf::IDENTITY;
use mf_operations::{ObjectChange, OperandInput, RunContext, Target, find};

fn timed_split(rings: u32, budget: Duration) {
    let mesh = uv_sphere(rings, rings);
    let triangles = mesh.face_count();
    mesh.topology();
    let spec = mf_spec::builtin::get("mesh.split_plane").unwrap();
    let raw = [
        ("plane_point", "0.05,-0.02,0.1"),
        ("plane_normal", "0.3,0.1,1"),
        ("cut_gap", "0.01"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let params = spec.validate(&raw).unwrap();
    let start = Instant::now();
    let out = find("mesh.split_plane")
        .unwrap()
        .execute_doc(
            &[OperandInput {
                object: ObjectId(1),
                name: "scan",
                mesh: &mesh,
                transform: IDENTITY,
                target: Target::WholeObject,
            }],
            &params,
            &RunContext::none(),
        )
        .unwrap();
    let took = start.elapsed();
    let parts = out
        .changes
        .iter()
        .filter(|c| {
            matches!(
                c,
                ObjectChange::Geometry { .. } | ObjectChange::Create { .. }
            )
        })
        .count();
    assert_eq!(parts, 2);
    eprintln!("split of {triangles} triangles: {took:?} (budget {budget:?})");
    assert!(took < budget, "{triangles} triangles took {took:?}");
}

#[test]
fn split_5m_under_2s() {
    // 2 · 1582 · 1581 ≈ 5.0M triangles.
    timed_split(1582, Duration::from_secs(2));
}

#[test]
fn split_30m_under_10s() {
    // 2 · 3874 · 3873 ≈ 30.0M triangles.
    timed_split(3874, Duration::from_secs(10));
}
