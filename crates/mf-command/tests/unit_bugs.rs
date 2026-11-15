#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Owner-reported unit bug (consolidation plan C01, units-audit.md
//! mf-command/src/session.rs:811): importing a second file in another
//! unit only warns "coordinates are not converted" — it never actually
//! converts them. A 10 mm part imported into an existing metres document
//! lands at its raw mm coordinates (10 doc units, 1000x too big), not at
//! its correct physical size (0.01 doc units). Fixed by C07 ("DocContext
//! and cross-unit import in mf-command").

use std::collections::BTreeMap;

use mf_command::{CommitOutcome, Invocation, ObjectId, Session, Verdict, replay};
use mf_fixtures::{rotation_translation, uv_sphere};
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};

fn model_at(unit: Unit, name: &str, mesh: mf_geometry::Mesh) -> Model {
    Model {
        metadata: Default::default(),
        unit,
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: name.into(),
            mesh,
            transform: IDENTITY,
        }],
    }
}

/// Open a metres document (a 1 m cube), then import a second file that
/// is itself in millimetres (a 10 mm cube): the second object's physical
/// size, once it's in the document, must be the same 10 mm — 0.01 doc
/// units — not the raw mm number (10.0) taken as if it were already in
/// doc units.
#[test]
fn a_second_import_in_another_unit_converts_to_the_document_unit() {
    let dir = tempfile::tempdir().unwrap();

    let base_path = dir.path().join("base.3mf");
    write_model_path(
        &model_at(Unit::Meter, "base", mf_fixtures::cube(1.0)),
        &base_path,
    )
    .unwrap();

    let part_path = dir.path().join("part.3mf");
    // A 10 mm cube, saved in a millimetre file.
    write_model_path(
        &model_at(Unit::Millimeter, "part", mf_fixtures::cube(10.0)),
        &part_path,
    )
    .unwrap();

    let mut s = Session::new();
    s.import_model(read_model(&base_path).unwrap());
    assert_eq!(s.document().units(), Unit::Meter, "set by the first import");

    let (ids, warnings) = s.import_model(read_model(&part_path).unwrap());
    let part = s.document().object(ids[0]).unwrap();
    let mesh = part.mesh().unwrap();
    let bbox = mesh.bounding_box().unwrap();
    let edge = bbox.max[0] - bbox.min[0];
    // Physically correct: 10 mm == 0.01 m == 0.01 doc units.
    assert!(
        (edge - 0.01).abs() < 1e-6,
        "a 10 mm part imported into a metres document should measure \
         0.01 doc units across, but measured {edge} (bbox {bbox:?}): \
         the cross-unit import left the raw mm coordinates unconverted"
    );
    // The warning says the import was converted, not just that units
    // mismatched (consolidation plan C07's contract).
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("converted from mm to m")),
        "expected a warning saying the import was converted from mm to m, got {warnings:?}"
    );
}

fn inv(op: &str, target: ObjectId, params: &[(&str, &str)]) -> Invocation {
    Invocation {
        op_id: op.into(),
        target,
        selection: None,
        operands: Vec::new(),
        raw_params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn report_of(out: CommitOutcome) -> serde_json::Value {
    match out {
        CommitOutcome::NewRevision { report } | CommitOutcome::NoChange { report } => report,
        other => panic!("{other:?}"),
    }
}

fn created(s: &Session) -> Vec<ObjectId> {
    s.records_to_current()
        .last()
        .unwrap()
        .outputs
        .iter()
        .filter(|o| o.created)
        .map(|o| o.object)
        .collect()
}

/// `split.place_connectors`' `mm` param (units-audit.md's "fake `mm`
/// length param") is still declared `unit: mm` and still recorded in
/// every log, even though `execute`'s new `DocContext` parameter
/// (consolidation plan C07) now carries the document's unit alongside
/// it. A log recorded before C07 — any log today, since C12 is the task
/// that removes the `mm` param — must keep replaying exactly: `prepare`
/// still converts it once at record time, replay still never reconverts
/// it, and the op's own `/ mm` round-trips inside `split/joints.rs` are
/// untouched by the new parameter threading through `execute_scene`.
#[test]
fn old_logs_with_mm_param_replay_identically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc.3mf");
    let model = Model {
        metadata: Default::default(),
        unit: Unit::Millimeter,
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: "scan".into(),
            mesh: mf_fixtures::scaled(&uv_sphere(24, 48), 30.0),
            transform: rotation_translation(2, 0.0, [0.0, 0.0, 30.0]),
        }],
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, warnings) = s.import_model(read_model(&path).unwrap());
    assert!(warnings.is_empty(), "{warnings:?}");
    let id = ids[0];

    let report = report_of(
        s.execute(inv(
            "mesh.split_plane",
            id,
            &[("plane_point", "0,0,31.3"), ("plane_normal", "0,0,1")],
        ))
        .unwrap(),
    );
    let joint = report["joints"][0].clone();
    let bottom = created(&s)[0];
    let top = id;

    let connectors = serde_json::json!([
        { "connector": "joint.dowel", "on": "a", "at": [-9.0, 0.0], "params": {} },
    ]);
    let mut place = inv(
        "split.place_connectors",
        bottom,
        &[
            ("plane_point", "0,0,31.3"),
            ("plane_normal", "0,0,1"),
            ("plane_u", "1,0,0"),
            ("gap", &joint["gap"].to_string()),
            ("connectors", &connectors.to_string()),
            // Not given: `mm` keeps its spec default (1.0 mm), exactly as
            // every log recorded before C07 either omitted it or carried
            // this same default.
        ],
    );
    place.operands = vec![("side_b".into(), top, None)];
    s.execute(place).unwrap();

    let recorded_mm = s
        .records_to_current()
        .last()
        .unwrap()
        .params
        .get_float("mm")
        .unwrap();
    // `to_document_units` converts the mm-unit param to the document's
    // own unit (here also mm), so 1.0 mm stays 1.0.
    assert_eq!(recorded_mm, 1.0);

    let log = dir.path().join("log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "an old log carrying split.place_connectors' mm param must still replay Exact: {:?}",
        report.results
    );
    let replayed_mm = report
        .session
        .records_to_current()
        .last()
        .unwrap()
        .params
        .get_float("mm")
        .unwrap();
    assert_eq!(
        replayed_mm, recorded_mm,
        "replay must use the recorded mm value unchanged, never reconvert it"
    );
}
