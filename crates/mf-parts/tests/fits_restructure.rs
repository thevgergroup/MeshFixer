#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The D20 fits restructuring (owner requirements 2026-11-11): Machine
//! profiles (tolerance) and Fit classes (clearance) are separate, a
//! generated feature's gap resolves from both, and the exact fit check
//! measures the real gap on the meshes.

use glam::DMat4;
use mf_fixtures::{cube, translated};
use mf_geometry::Bvh;
use mf_parts::{
    Compensation, FitClass, FitClasses, FitExpectation, FitProfile, MachineProfile,
    MachineProfiles, MatingSurfaces, UNCALIBRATED_NAME, check_fit, resolve,
};
use mf_runtime::RunContext;

#[test]
fn uncalibrated_machine_has_zero_compensation() {
    let m = MachineProfile::uncalibrated();
    assert_eq!(m.name, UNCALIBRATED_NAME);
    assert!(!m.calibrated);
    assert_eq!((m.outer_xy, m.hole_undersize, m.z), (0.0, 0.0, 0.0));
}

#[test]
fn builtin_fit_classes_are_named_and_increasing() {
    let classes = FitClass::builtins();
    let names: Vec<&str> = classes.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Press", "Snug", "Sliding", "Free running"]);
    for i in 1..classes.len() {
        assert!(
            classes[i].clearance > classes[i - 1].clearance,
            "fit classes should be in increasing clearance order"
        );
    }
}

#[test]
fn resolve_combines_clearance_and_the_relevant_tolerance() {
    let snug = FitClass {
        name: "Snug".into(),
        clearance: 0.0,
    };
    let calibrated = MachineProfile {
        name: "Prusa MK4".into(),
        calibrated: true,
        outer_xy: 0.05,
        hole_undersize: 0.15,
        z: -0.02,
    };
    assert_eq!(resolve(&snug, &calibrated, Compensation::Hole), 0.15);
    assert_eq!(resolve(&snug, &calibrated, Compensation::OuterXy), 0.05);
    assert_eq!(resolve(&snug, &calibrated, Compensation::Z), -0.02);
    assert_eq!(resolve(&snug, &calibrated, Compensation::None), 0.0);

    let uncalibrated = MachineProfile::uncalibrated();
    assert_eq!(
        resolve(&snug, &uncalibrated, Compensation::Hole),
        0.0,
        "no compensation until the user calibrates"
    );
}

#[test]
fn machine_profiles_round_trip_and_the_builtin_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut profiles = MachineProfiles::load(dir.path()).unwrap();
    assert_eq!(profiles.active, UNCALIBRATED_NAME);
    profiles.duplicate(UNCALIBRATED_NAME, "Prusa MK4").unwrap();
    profiles
        .update(MachineProfile {
            name: "Prusa MK4".into(),
            calibrated: true,
            outer_xy: 0.1,
            hole_undersize: 0.15,
            z: 0.05,
        })
        .unwrap();
    profiles.set_active("Prusa MK4").unwrap();
    profiles.save(dir.path()).unwrap();

    let reloaded = MachineProfiles::load(dir.path()).unwrap();
    assert_eq!(reloaded.active, "Prusa MK4");
    let p = reloaded.get("Prusa MK4").unwrap();
    assert!(p.calibrated);
    assert_eq!(p.hole_undersize, 0.15);

    let err = reloaded
        .clone()
        .update(MachineProfile {
            name: UNCALIBRATED_NAME.into(),
            calibrated: true,
            outer_xy: 1.0,
            hole_undersize: 1.0,
            z: 1.0,
        })
        .unwrap_err();
    assert!(matches!(err, mf_parts::MachineError::BuiltinIsReadOnly));
}

#[test]
fn fit_classes_round_trip_with_a_user_addition() {
    let dir = tempfile::tempdir().unwrap();
    let mut classes = FitClasses::load(dir.path()).unwrap();
    assert_eq!(classes.classes.len(), 4, "the four built-ins");
    classes.classes.push(FitClass {
        name: "Very loose".into(),
        clearance: 0.5,
    });
    classes.save(dir.path()).unwrap();
    let reloaded = FitClasses::load(dir.path()).unwrap();
    assert_eq!(reloaded.classes.len(), 5);
    assert_eq!(reloaded.get("Very loose").unwrap().clearance, 0.5);
}

/// The old D20 `FitProfile` (peg_clearance, length_allowance, hinge_gap,
/// clasp_interference) is untouched by the restructuring: existing specs
/// and logs that use `default_from: fit.<field>` keep working.
#[test]
fn the_legacy_fit_profile_is_unchanged() {
    let b = FitProfile::builtin();
    assert_eq!(
        (
            b.peg_clearance,
            b.length_allowance,
            b.hinge_gap,
            b.clasp_interference
        ),
        (0.2, 0.4, 0.4, 0.3)
    );
}

#[test]
fn fit_check_reports_a_gap_against_a_fit_class_in_plain_language() {
    let ctx = RunContext::none();
    let a = cube(10.0);
    let a_bvh = Bvh::build(&a, &ctx).unwrap();
    // b sits 0.15 mm clear of a's +x face (a snug fit on an uncalibrated
    // machine expects 0 mm, so this 0.15 mm gap is not "too tight").
    let b = translated(&cube(10.0), [10.15, 0.0, 0.0]);
    let snug = FitClass {
        name: "Snug".into(),
        clearance: 0.0,
    };
    let machine = MachineProfile::uncalibrated();
    let check = check_fit(
        "Hinge pin to knuckle",
        MatingSurfaces {
            a: &a,
            a_bvh: &a_bvh,
            b: &b,
            b_to_a: &DMat4::IDENTITY,
        },
        FitExpectation {
            fit_class: &snug,
            machine: &machine,
            compensation: Compensation::None,
        },
        &ctx,
    )
    .unwrap()
    .unwrap();
    assert!((check.measured_gap - 0.15).abs() < 1e-9);
    assert_eq!(check.expected_gap, 0.0);
    assert!(
        !check.may_fuse,
        "0.15 mm clear of a 0 mm expectation is fine"
    );
    assert!(check.message().contains("Hinge pin to knuckle"));
    assert!(check.message().contains("0.15"));
}

#[test]
fn fit_check_warns_when_the_measured_gap_is_well_under_what_the_fit_class_expects() {
    let ctx = RunContext::none();
    let a = cube(10.0);
    let a_bvh = Bvh::build(&a, &ctx).unwrap();
    // Only 0.05 mm clear, but a Sliding fit on this machine expects
    // 0.15 + 0.1 (hole undersize) = 0.25 mm: well over half, so it may fuse.
    let b = translated(&cube(10.0), [10.05, 0.0, 0.0]);
    let sliding = FitClass {
        name: "Sliding".into(),
        clearance: 0.15,
    };
    let machine = MachineProfile {
        name: "Prusa MK4".into(),
        calibrated: true,
        outer_xy: 0.0,
        hole_undersize: 0.1,
        z: 0.0,
    };
    let check = check_fit(
        "Hinge pin to knuckle",
        MatingSurfaces {
            a: &a,
            a_bvh: &a_bvh,
            b: &b,
            b_to_a: &DMat4::IDENTITY,
        },
        FitExpectation {
            fit_class: &sliding,
            machine: &machine,
            compensation: Compensation::Hole,
        },
        &ctx,
    )
    .unwrap()
    .unwrap();
    assert_eq!(check.expected_gap, 0.25);
    assert!(check.may_fuse, "{check:?}");
    assert!(check.message().contains("may fuse"));
}
