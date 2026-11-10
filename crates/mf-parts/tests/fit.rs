#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Print fit profiles (Phase 1 Task 12a, D20): the built-in values,
//! filling spec defaults from the active profile, recording the values
//! used, replay never reading a profile, and named errors for bad files.

use std::collections::BTreeMap;
use std::path::Path;

use mf_command::{Invocation, ObjectId, Session, Verdict, replay};
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};
use mf_parts::{BUILTIN_NAME, FIT_PROFILES_FILE, FitError, FitProfile, FitProfiles};
use mf_spec::ParamValue;

fn tight() -> FitProfile {
    FitProfile {
        name: "Tight PETG".into(),
        peg_clearance: 0.3,
        length_allowance: 0.5,
        hinge_gap: 0.45,
        clasp_interference: 0.25,
    }
}

fn split_spec() -> &'static mf_spec::OperationSpec {
    mf_spec::builtin::get("mesh.split_plane").unwrap()
}

fn session_with(dir: &Path) -> (Session, ObjectId) {
    let path = dir.join("doc.3mf");
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: "block".into(),
            mesh: mf_fixtures::cube(10.0),
            transform: IDENTITY,
        }],
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, _) = s.import_model(read_model(&path).unwrap());
    (s, ids[0])
}

/// A split invocation with the profile's defaults filled in, as a
/// frontend makes it.
fn split_filled(target: ObjectId, fit: &FitProfile) -> Invocation {
    let mut raw = BTreeMap::from([("plane_point".to_string(), "0,0,5".to_string())]);
    split_spec().fill_defaults(&mut raw, fit).unwrap();
    Invocation {
        op_id: "mesh.split_plane".into(),
        target,
        selection: None,
        operands: Vec::new(),
        raw_params: raw,
    }
}

#[test]
fn builtin_profile_has_the_documented_defaults() {
    let b = FitProfile::builtin();
    assert_eq!(b.name, "Default FDM, 0.4 mm nozzle");
    assert_eq!(
        (
            b.peg_clearance,
            b.length_allowance,
            b.hinge_gap,
            b.clasp_interference
        ),
        (0.2, 0.4, 0.4, 0.3)
    );
    // Every built-in spec's `default_from` names a profile field, and its
    // written default is the built-in value (what CLI help shows).
    for spec in mf_spec::builtin::specs() {
        for p in &spec.parameters {
            let Some(from) = &p.default_from else {
                continue;
            };
            let field = from.strip_prefix("fit.").unwrap();
            let value = mf_spec::FitDefaults::fit_value(&b, field)
                .unwrap_or_else(|| panic!("{}.{}: no field {field}", spec.id, p.name));
            assert_eq!(
                p.kind.default_value(),
                ParamValue::Float(value),
                "{}.{}",
                spec.id,
                p.name
            );
        }
    }
    // No file yet: only the built-in, active.
    let dir = tempfile::tempdir().unwrap();
    let profiles = FitProfiles::load(dir.path()).unwrap();
    assert_eq!(profiles.active_profile(), &b);
}

#[test]
fn explicit_param_beats_profile_default() {
    let mut raw = BTreeMap::from([("peg_clearance".to_string(), "0.1".to_string())]);
    split_spec().fill_defaults(&mut raw, &tight()).unwrap();
    assert_eq!(raw["peg_clearance"], "0.1");
    assert_eq!(raw["length_allowance"], "0.5");
    let params = split_spec().validate(&raw).unwrap();
    assert_eq!(params.get_float("peg_clearance").unwrap(), 0.1);
}

#[test]
fn recorded_params_hold_profile_values() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, id) = session_with(dir.path());
    s.execute(split_filled(id, &tight())).unwrap();
    let record = s.node(s.current()).unwrap().record.clone().unwrap();
    assert_eq!(record.params.get_float("peg_clearance").unwrap(), 0.3);
    assert_eq!(record.params.get_float("length_allowance").unwrap(), 0.5);
}

/// Recorded with a 0.3 mm profile active, the log replays Exact while a
/// different profile is active: replay reads only the recorded values.
#[test]
fn replay_ignores_the_active_profile() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, id) = session_with(dir.path());
    s.execute(split_filled(id, &tight())).unwrap();
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();

    // Another active profile on disk; replay has no way to see it, and
    // must not need one.
    let config = dir.path().join("config");
    let mut profiles = FitProfiles::default();
    profiles.duplicate(BUILTIN_NAME, "Loose").unwrap();
    let mut loose = profiles.get("Loose").unwrap().clone();
    loose.peg_clearance = 0.5;
    profiles.update(loose).unwrap();
    profiles.set_active("Loose").unwrap();
    profiles.save(&config).unwrap();

    let report = replay(&log).unwrap();
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
    let record = report
        .session
        .node(report.session.current())
        .unwrap()
        .record
        .clone()
        .unwrap();
    assert_eq!(record.params.get_float("peg_clearance").unwrap(), 0.3);
}

#[test]
fn malformed_profile_file_is_a_named_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(FIT_PROFILES_FILE);
    for (text, why) in [
        ("active: [not, a, name\n", "bad YAML"),
        ("active: Nope\nprofiles: []\n", "unknown active profile"),
        (
            "active: A\nprofiles:\n  - name: A\n    peg_clearance: -0.1\n    length_allowance: 0.4\n    hinge_gap: 0.4\n    clasp_interference: 0.3\n",
            "negative clearance",
        ),
        (
            "active: A\nprofiles:\n  - name: A\n    peg_clearance: 0.1\n",
            "missing fields",
        ),
        (
            "active: X\nprofiles:\n  - name: Default FDM, 0.4 mm nozzle\n    peg_clearance: 0.1\n    length_allowance: 0.4\n    hinge_gap: 0.4\n    clasp_interference: 0.3\n",
            "shadows the built-in",
        ),
    ] {
        std::fs::write(&path, text).unwrap();
        match FitProfiles::load(dir.path()) {
            Err(FitError::Malformed { path: p, .. }) => assert_eq!(p, path, "{why}"),
            other => panic!("{why}: {other:?}"),
        }
    }
}

#[test]
fn profiles_round_trip_and_the_builtin_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let mut profiles = FitProfiles::default();
    profiles.duplicate(BUILTIN_NAME, "Mine").unwrap();
    profiles.rename("Mine", "Tight PETG").unwrap();
    profiles.update(tight()).unwrap();
    profiles.set_active("Tight PETG").unwrap();
    profiles.save(dir.path()).unwrap();
    let loaded = FitProfiles::load(dir.path()).unwrap();
    assert_eq!(loaded, profiles);
    assert_eq!(loaded.active_profile(), &tight());
    assert_eq!(loaded.profiles[0], FitProfile::builtin());

    let mut p = loaded.clone();
    assert_eq!(
        p.rename(BUILTIN_NAME, "x"),
        Err(FitError::BuiltinIsReadOnly)
    );
    assert_eq!(p.delete(BUILTIN_NAME), Err(FitError::BuiltinIsReadOnly));
    let mut b = FitProfile::builtin();
    b.peg_clearance = 1.0;
    assert_eq!(p.update(b), Err(FitError::BuiltinIsReadOnly));
    p.delete("Tight PETG").unwrap();
    assert_eq!(p.active, BUILTIN_NAME);
}

#[test]
fn default_in_any_case_is_the_built_in_profile() {
    let dir = tempfile::tempdir().unwrap();
    let profiles = mf_parts::FitProfiles::load(dir.path()).unwrap();
    for name in ["Default", "default", "DEFAULT"] {
        assert_eq!(
            profiles.lookup(name),
            Some(&mf_parts::FitProfile::builtin()),
            "{name}"
        );
    }
    assert_eq!(profiles.lookup("Nope"), None);
}
