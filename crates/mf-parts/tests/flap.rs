#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Flap hinge (connector `hinge.flap`): two leaves plus a pin, printed
//! in place (captive) or separately.

use std::collections::BTreeMap;

use glam::DMat4;
use mf_geometry::{Bvh, FaceId, intersections_between, minimum_distance};
use mf_parts::connectors::flap::FlapHinge;
use mf_parts::{Generator, JoinKind, Side};
use mf_runtime::RunContext;

fn params(extra: &[(&str, &str)]) -> mf_spec::Params {
    let spec = FlapHinge.spec();
    let mut raw: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in extra {
        raw.insert(k.to_string(), v.to_string());
    }
    spec.fill_defaults(&mut raw, &mf_parts::FitProfile::builtin())
        .unwrap();
    spec.validate(&raw).unwrap()
}

fn generate(extra: &[(&str, &str)]) -> Vec<mf_parts::ShapePart> {
    FlapHinge
        .generate(
            &params(extra),
            &mf_parts::GenContext::new(&RunContext::none()),
        )
        .unwrap()
}

fn leaf<'a>(parts: &'a [mf_parts::ShapePart], role: &str) -> &'a mf_geometry::Mesh {
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
fn captive_mode_makes_exactly_two_parts_with_the_pin_fused_into_leaf_a() {
    let parts = generate(&[("pin_mode", "captive")]);
    assert_eq!(parts.len(), 2, "no separate pin part in captive mode");
    assert_closed_solid(leaf(&parts, "leaf_a"), "leaf_a");
    assert_closed_solid(leaf(&parts, "leaf_b"), "leaf_b");
    assert_eq!(
        parts.iter().find(|p| p.name == "leaf_a").unwrap().join(),
        JoinKind::Union
    );
}

#[test]
fn separate_mode_makes_a_third_loose_pin_part() {
    let parts = generate(&[("pin_mode", "separate"), ("gap", "0.3")]);
    assert_eq!(parts.len(), 3, "leaf_a, leaf_b and a loose pin");
    let pin = parts.iter().find(|p| p.name == "pin").unwrap();
    assert_eq!(pin.attach_to, Side::Neither);
    assert_eq!(pin.join(), JoinKind::Separate);
    assert_closed_solid(&pin.mesh, "pin");
    assert_closed_solid(leaf(&parts, "leaf_a"), "leaf_a");
    assert_closed_solid(leaf(&parts, "leaf_b"), "leaf_b");

    // In separate mode both leaves' knuckles clear the pin by `gap`,
    // measured on the meshes (not assumed from the parameters): leaf_a
    // no longer has the pin fused in, so the pin must sit clear of it
    // too, exactly as it does of leaf_b in the pin hinge.
    let ctx = RunContext::none();
    let a = leaf(&parts, "leaf_a");
    let a_bvh = Bvh::build(a, &ctx).unwrap();
    let d = minimum_distance(a, &a_bvh, &pin.mesh, &DMat4::IDENTITY, &ctx)
        .unwrap()
        .unwrap();
    let chord_tolerance = 0.05;
    assert!(d >= 0.3 - chord_tolerance, "leaf_a to pin: {d}");
}

/// The two leaves never touch in the closed (flat) print pose, measured
/// exactly with the BVH minimum distance.
#[test]
fn flap_leaves_never_touch_in_print_pose() {
    let parts = generate(&[("gap", "0.4")]);
    let ctx = RunContext::none();
    let a = leaf(&parts, "leaf_a");
    let b = leaf(&parts, "leaf_b");
    let a_bvh = Bvh::build(a, &ctx).unwrap();
    let d = minimum_distance(a, &a_bvh, b, &DMat4::IDENTITY, &ctx)
        .unwrap()
        .unwrap();
    let chord_tolerance = 0.05;
    assert!(d >= 0.4 - chord_tolerance, "leaves should be apart: {d}");
}

/// Each leaf's wing is a flat plate on that leaf alone (not the other
/// leaf's), reaching `leaf_width` out from the knuckles: measured on
/// the mesh's own bounding box.
#[test]
fn each_leaf_has_its_own_wing_reaching_leaf_width_out() {
    let parts = generate(&[
        ("knuckle_diameter", "6"),
        ("leaf_width", "8"),
        ("leaf_thickness", "2"),
    ]);
    for role in ["leaf_a", "leaf_b"] {
        let bb = leaf(&parts, role).bounding_box().unwrap();
        // Knuckle radius 3, wing out to 3 + 8 = 11 in y.
        assert!((bb.max[1] - 11.0).abs() < 1e-9, "{role}: {bb:?}");
        // The knuckle itself is a round cylinder of radius 3 (its own
        // cross-section, not the thinner wing tab), so the leaf's
        // overall z-extent is +-r_knuckle, not +-leaf_thickness/2: the
        // wing only narrows the solid near z = 0, it doesn't shrink the
        // knuckle's own round part.
        assert!((bb.max[2] - 3.0).abs() < 0.05, "{role}: {bb:?}");
        assert!((bb.min[2] + 3.0).abs() < 0.05, "{role}: {bb:?}");
    }
}

/// The flap opens through 90 degrees (leaf_b rotating about the hinge
/// axis) without the leaves colliding, checked with the exact
/// intersection test at several angles, not merely the gap at rest.
#[test]
fn flap_opens_90_degrees_without_collision() {
    let parts = generate(&[("gap", "0.4")]);
    let ctx = RunContext::none();
    let a = leaf(&parts, "leaf_a");
    let b = leaf(&parts, "leaf_b");
    let a_bvh = Bvh::build(a, &ctx).unwrap();
    for deg in (0..=90).step_by(10) {
        let rot = DMat4::from_rotation_x((deg as f64).to_radians());
        let intersections =
            intersections_between(a, &a_bvh, b, &Bvh::build(b, &ctx).unwrap(), &rot, &ctx).unwrap();
        assert_eq!(
            intersections.pair_count, 0,
            "{deg} degrees: leaves should not intersect while the flap opens"
        );
    }
}

#[test]
fn knuckles_must_be_odd_is_still_enforced() {
    match FlapHinge.generate(
        &params(&[("knuckles", "4")]),
        &mf_parts::GenContext::new(&RunContext::none()),
    ) {
        Err(mf_parts::ConnectorError::Failed { code, .. }) => {
            assert_eq!(code, "knuckles_must_be_odd")
        }
        Ok(_) => panic!("expected a refusal"),
    }
}
