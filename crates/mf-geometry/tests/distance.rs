#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The exact minimum distance between two meshes (owner requirements
//! 2026-11-11, the connector fit check).

use glam::DMat4;
use mf_fixtures::{cube, translated};
use mf_geometry::{Bvh, minimum_distance};
use mf_runtime::RunContext;

#[test]
fn separated_cubes_measure_their_gap_exactly() {
    let ctx = RunContext::none();
    let a = cube(1.0);
    let a_bvh = Bvh::build(&a, &ctx).unwrap();
    // A unit cube [0,1]^3 and another shifted 1.5 along x: gap is
    // 1.5 - 1.0 = 0.5 exactly.
    let b = translated(&cube(1.0), [1.5, 0.0, 0.0]);
    let d = minimum_distance(&a, &a_bvh, &b, &DMat4::IDENTITY, &ctx)
        .unwrap()
        .unwrap();
    assert!((d - 0.5).abs() < 1e-9, "{d}");
}

#[test]
fn touching_cubes_measure_zero() {
    let ctx = RunContext::none();
    let a = cube(1.0);
    let a_bvh = Bvh::build(&a, &ctx).unwrap();
    let b = translated(&cube(1.0), [1.0, 0.0, 0.0]);
    let d = minimum_distance(&a, &a_bvh, &b, &DMat4::IDENTITY, &ctx)
        .unwrap()
        .unwrap();
    assert!(d.abs() < 1e-9, "{d}");
}

#[test]
fn overlapping_cubes_measure_zero_not_negative() {
    let ctx = RunContext::none();
    let a = cube(1.0);
    let a_bvh = Bvh::build(&a, &ctx).unwrap();
    let b = translated(&cube(1.0), [0.5, 0.0, 0.0]);
    let d = minimum_distance(&a, &a_bvh, &b, &DMat4::IDENTITY, &ctx)
        .unwrap()
        .unwrap();
    assert!(d.abs() < 1e-9, "overlapping surfaces touch: {d}");
}

#[test]
fn distance_respects_the_tool_transform() {
    let ctx = RunContext::none();
    let a = cube(1.0);
    let a_bvh = Bvh::build(&a, &ctx).unwrap();
    let b = cube(1.0);
    // b_to_a translates b by 2.0 along x: gap is 2.0 - 1.0 = 1.0.
    let b_to_a = DMat4::from_translation(glam::DVec3::new(2.0, 0.0, 0.0));
    let d = minimum_distance(&a, &a_bvh, &b, &b_to_a, &ctx)
        .unwrap()
        .unwrap();
    assert!((d - 1.0).abs() < 1e-9, "{d}");
}
