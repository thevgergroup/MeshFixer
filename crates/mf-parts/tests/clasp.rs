#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Snap clasp (Phase 1 Task 27, connector `clasp.snap`).

use std::collections::BTreeMap;

use glam::DMat4;
use mf_geometry::{Bvh, FaceId, intersections_between, minimum_distance};
use mf_parts::connectors::clasp::{MAX_SAFE_STRAIN, SnapClasp, engagement_strain};
use mf_parts::{ConnectorError, Generator, Side};
use mf_runtime::RunContext;

fn params(extra: &[(&str, &str)]) -> mf_spec::Params {
    let spec = SnapClasp.spec();
    let mut raw: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in extra {
        raw.insert(k.to_string(), v.to_string());
    }
    spec.fill_defaults(&mut raw, &mf_parts::FitProfile::builtin())
        .unwrap();
    spec.validate(&raw).unwrap()
}

fn generate(extra: &[(&str, &str)]) -> Result<Vec<mf_parts::ShapePart>, ConnectorError> {
    SnapClasp.generate(
        &params(extra),
        &mf_parts::GenContext::new(&RunContext::none()),
    )
}

fn part<'a>(parts: &'a [mf_parts::ShapePart], role: &str) -> &'a mf_geometry::Mesh {
    &parts
        .iter()
        .find(|p| p.name == role)
        .unwrap_or_else(|| panic!("no part with role {role}"))
        .mesh
}

fn assert_closed_solid(mesh: &mf_geometry::Mesh, label: &str) {
    let ctx = RunContext::none();
    let bvh = Bvh::build(mesh, &ctx).unwrap();
    let all: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
    mf_geometry::validate_solid(mesh, &all, &bvh, &ctx)
        .unwrap()
        .unwrap_or_else(|d| panic!("{label}: not a valid solid: {d}"));
}

#[test]
fn clasp_parts_are_closed_solids_on_their_own_sides() {
    let parts = generate(&[]).unwrap();
    assert_eq!(parts.len(), 2);
    assert_closed_solid(part(&parts, "hook"), "hook");
    assert_closed_solid(part(&parts, "catch"), "catch");
    assert_eq!(
        parts.iter().find(|p| p.name == "hook").unwrap().attach_to,
        Side::B
    );
    assert_eq!(
        parts.iter().find(|p| p.name == "catch").unwrap().attach_to,
        Side::A
    );
}

/// In the closed print pose, the hook (lid) and catch (body) don't touch:
/// the lip's underside is `gap` clear of the catch's top face, measured
/// exactly with the BVH minimum distance, not assumed from the
/// parameters.
#[test]
fn clasp_parts_dont_touch_in_print_pose() {
    let parts = generate(&[("gap", "0.4")]).unwrap();
    let ctx = RunContext::none();
    let hook = part(&parts, "hook");
    let catch = part(&parts, "catch");
    let hook_bvh = Bvh::build(hook, &ctx).unwrap();
    let d = minimum_distance(hook, &hook_bvh, catch, &DMat4::IDENTITY, &ctx)
        .unwrap()
        .unwrap();
    assert!((d - 0.4).abs() < 1e-9, "{d}");

    // Also never cross: no pair of faces intersects or even touches
    // beyond the exact gap above.
    let catch_bvh = Bvh::build(catch, &ctx).unwrap();
    let report =
        intersections_between(hook, &hook_bvh, catch, &catch_bvh, &DMat4::IDENTITY, &ctx).unwrap();
    assert_eq!(report.pair_count, 0, "{report:?}");
}

/// The lip overlaps the catch, in projection along the arm's flex
/// direction (z), by exactly `lip_depth`: the lip's x-span and the
/// catch's x-span are measured on the meshes (their bounding boxes),
/// not assumed from the parameter alone.
#[test]
fn lip_overlaps_catch_by_interference_measured_on_meshes() {
    let parts = generate(&[("lip_depth", "0.3"), ("arm_length", "10")]).unwrap();
    let hook = part(&parts, "hook");
    let catch = part(&parts, "catch");
    let hook_bb = hook.bounding_box().unwrap();
    let catch_bb = catch.bounding_box().unwrap();
    // Both reach the arm's free end (x = arm_length).
    assert!((hook_bb.max[0] - 10.0).abs() < 1e-9);
    assert!((catch_bb.max[0] - 10.0).abs() < 1e-9);
    // The overlap of the two x-spans, measured on the meshes.
    let overlap = hook_bb.max[0].min(catch_bb.max[0]) - hook_bb.min[0].max(catch_bb.min[0]);
    assert!((overlap - 0.3).abs() < 1e-9, "{overlap}");
}

/// The owner's beam formula (`3·t·δ / (2·L²)`): short, thin arms with a
/// deep lip strain too much to flex safely in PLA and are refused by
/// name, with what to change.
#[test]
fn arm_strain_at_engagement_under_3_percent() {
    // Default parameters (arm_length 10, arm_thickness 1.6, lip_depth
    // 0.3): well under the limit.
    let s = engagement_strain(10.0, 1.6, 0.3);
    assert!(s < MAX_SAFE_STRAIN, "{s}");
    assert!(generate(&[]).is_ok());

    // A short, thick arm with a deep lip strains far past 3% and is
    // refused by name, not silently generated.
    match generate(&[
        ("arm_length", "3"),
        ("arm_thickness", "1.6"),
        ("lip_depth", "0.3"),
    ]) {
        Err(ConnectorError::Failed { code, message }) => {
            assert_eq!(code, "arm_strain_too_high");
            assert!(message.contains("longer or thinner"), "{message}");
        }
        Ok(_) => panic!("expected a refusal"),
    }
}

#[test]
fn lip_deeper_than_the_arm_is_refused() {
    match generate(&[("lip_depth", "11"), ("arm_length", "10")]) {
        Err(ConnectorError::Failed { code, .. }) => assert_eq!(code, "lip_deeper_than_arm"),
        Ok(_) => panic!("expected a refusal"),
    }
}
