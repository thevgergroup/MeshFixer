#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `fit_role`: generic filling of a connector's Fit-class/Machine-
//! profile-derived parameters (parts-architecture-review.md), replacing
//! a per-connector `if id == ...` match in `connector_tool.rs`'s own
//! `connector_params`. This file tests the pure filler
//! (`mf_parts::fill_fit_role_params`) directly; the desktop's own use of
//! it at placement time is covered by `connectors.rs`'s and
//! `core_workflows.rs`'s existing connector-placement tests, which never
//! changed behaviour across this refactor.

use std::collections::BTreeMap;

use mf_parts::connectors::dovetail_slide::DovetailSlide;
use mf_parts::{Connector, FitClass, MachineProfile, fill_fit_role_params};

fn class(clearance: f64) -> FitClass {
    FitClass {
        name: "Test class".into(),
        clearance,
    }
}

fn machine() -> MachineProfile {
    MachineProfile {
        name: "Test printer".into(),
        calibrated: true,
        outer_xy: 0.11,
        hole_undersize: 0.22,
        z: 0.33,
        print_volume_mm: [200.0, 200.0, 200.0],
    }
}

#[test]
fn fills_every_fit_role_param_the_caller_did_not_set() {
    let spec = DovetailSlide.spec();
    let c = class(0.15);
    let m = machine();
    let mut raw = BTreeMap::new();
    fill_fit_role_params(spec, &mut raw, &c, &m);
    // `fit_clearance` is "clearance" (plain, uncompensated).
    assert_eq!(raw.get("fit_clearance").unwrap(), "0.15");
    // `machine_tolerance` is the raw, uncompensated machine field.
    assert_eq!(raw.get("machine_tolerance").unwrap(), "0.22");
    assert_eq!(raw.get("fit_class").unwrap(), "Test class");
    assert_eq!(raw.get("machine_profile").unwrap(), "Test printer");
}

#[test]
fn a_param_the_caller_already_set_is_left_alone() {
    let spec = DovetailSlide.spec();
    let c = class(0.15);
    let m = machine();
    let mut raw = BTreeMap::from([("fit_clearance".to_owned(), "9.99".to_owned())]);
    fill_fit_role_params(spec, &mut raw, &c, &m);
    assert_eq!(raw.get("fit_clearance").unwrap(), "9.99", "not overwritten");
    // Everything else with a fit_role still fills in.
    assert_eq!(raw.get("machine_tolerance").unwrap(), "0.22");
}

#[test]
fn params_with_no_fit_role_are_never_touched() {
    let spec = DovetailSlide.spec();
    let mut raw = BTreeMap::new();
    fill_fit_role_params(spec, &mut raw, &class(0.15), &machine());
    assert!(
        !raw.contains_key("width") && !raw.contains_key("length") && !raw.contains_key("angle"),
        "{raw:?}"
    );
}

#[test]
fn an_outer_xy_role_adds_the_machine_profiles_outer_xy_deviation() {
    // Exercised through the hinge's own spec (`hinge.leaf`'s `gap` is
    // `fit_role: outer_xy`), proving the compensation arms beyond
    // "clearance"/"hole" (which dovetail already covers) also resolve
    // correctly.
    let spec = mf_parts::connectors::leaf_hinge::LeafHinge.spec();
    let mut raw = BTreeMap::new();
    fill_fit_role_params(spec, &mut raw, &class(0.2), &machine());
    let gap: f64 = raw.get("gap").unwrap().parse().unwrap();
    let pin_clearance: f64 = raw.get("pin_clearance").unwrap().parse().unwrap();
    assert!((gap - 0.31).abs() < 1e-9, "0.2 clearance + 0.11 outer_xy: {gap}");
    assert!(
        (pin_clearance - 0.42).abs() < 1e-9,
        "0.2 clearance + 0.22 hole_undersize: {pin_clearance}"
    );
}
