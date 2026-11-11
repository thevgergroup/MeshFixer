#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Print-in-place pin hinge (Phase 1 Task 26, connector `hinge.pin`).

use std::collections::BTreeMap;

use glam::DMat4;
use mf_geometry::{Bvh, FaceId, minimum_distance};
use mf_parts::connectors::hinge::PinHinge;
use mf_parts::{Connector, FitClass, MachineProfile, Side};
use mf_runtime::RunContext;

fn params(extra: &[(&str, &str)]) -> mf_spec::Params {
    let spec = PinHinge.spec();
    let mut raw: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in extra {
        raw.insert(k.to_string(), v.to_string());
    }
    // Fill the fit-profile default (`gap` from `fit.hinge_gap`) with the
    // builtin fit class's snug clearance (0.0) resolved through an
    // uncalibrated machine, if the caller hasn't set it explicitly.
    if !raw.contains_key("gap") {
        let snug = FitClass::builtins()[1].clone();
        let gap = mf_parts::resolve(
            &snug.clone(),
            &MachineProfile::uncalibrated(),
            mf_parts::Compensation::None,
        )
        .max(0.2); // a real gap for the test, not the Snug default of 0
        raw.insert("gap".to_string(), gap.to_string());
    }
    spec.fill_defaults(&mut raw, &mf_parts::FitProfile::builtin())
        .unwrap();
    spec.validate(&raw).unwrap()
}

fn generate(extra: &[(&str, &str)]) -> mf_parts::ConnectorParts {
    PinHinge
        .generate(&params(extra), &RunContext::none())
        .unwrap()
}

fn leaf<'a>(parts: &'a mf_parts::ConnectorParts, role: &str) -> &'a mf_geometry::Mesh {
    &parts
        .parts
        .iter()
        .find(|p| p.role == role)
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
fn hinge_parts_closed_and_manifold() {
    let parts = generate(&[]);
    assert_eq!(parts.parts.len(), 2);
    assert_closed_solid(leaf(&parts, "leaf_a"), "leaf_a");
    assert_closed_solid(leaf(&parts, "leaf_b"), "leaf_b");
    assert_eq!(
        parts
            .parts
            .iter()
            .find(|p| p.role == "leaf_a")
            .unwrap()
            .side,
        Side::A
    );
    assert_eq!(
        parts
            .parts
            .iter()
            .find(|p| p.role == "leaf_b")
            .unwrap()
            .side,
        Side::B
    );
}

#[test]
fn hinge_leaves_never_touch() {
    let parts = generate(&[("gap", "0.4")]);
    let ctx = RunContext::none();
    let a = leaf(&parts, "leaf_a");
    let b = leaf(&parts, "leaf_b");
    let a_bvh = Bvh::build(a, &ctx).unwrap();
    let d = minimum_distance(a, &a_bvh, b, &DMat4::IDENTITY, &ctx)
        .unwrap()
        .unwrap();
    // The minimum distance between the two leaves is at least the gap
    // minus a small chord tolerance (the cylinders are faceted, not
    // exact circles).
    let chord_tolerance = 0.05;
    assert!(
        d >= 0.4 - chord_tolerance,
        "leaves should be at least gap apart: {d}"
    );
}

#[test]
fn pin_radial_gap_measured_on_meshes() {
    // leaf_a carries the pin; leaf_b's bore must clear it by `gap`,
    // measured on the meshes (not assumed from the parameters).
    let parts = generate(&[("gap", "0.3"), ("pin_diameter", "3.0")]);
    let ctx = RunContext::none();
    let a = leaf(&parts, "leaf_a"); // pin + odd knuckles
    let b = leaf(&parts, "leaf_b"); // bored even knuckles
    let a_bvh = Bvh::build(a, &ctx).unwrap();
    let d = minimum_distance(a, &a_bvh, b, &DMat4::IDENTITY, &ctx)
        .unwrap()
        .unwrap();
    let chord_tolerance = 0.05;
    assert!(d >= 0.3 - chord_tolerance, "{d}");
}

#[test]
fn hinge_rotates_90_degrees_without_collision() {
    let parts = generate(&[("gap", "0.4")]);
    let ctx = RunContext::none();
    let a = leaf(&parts, "leaf_a");
    let b = leaf(&parts, "leaf_b");
    let a_bvh = Bvh::build(a, &ctx).unwrap();
    for deg in (0..=90).step_by(5) {
        let rot = DMat4::from_rotation_x((deg as f64).to_radians());
        let intersections = mf_geometry::intersections_between(
            a,
            &a_bvh,
            b,
            &Bvh::build(b, &ctx).unwrap(),
            &rot,
            &ctx,
        )
        .unwrap();
        assert_eq!(
            intersections.pair_count, 0,
            "{deg} degrees: leaves should not intersect while the hinge opens"
        );
    }
}
