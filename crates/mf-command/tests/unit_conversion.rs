#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Unit conversion at `prepare` time happens exactly once (D20, Global
//! Constraints: "Printing parameters are in document units... converted
//! at prepare time. The converted value is what's recorded").
//!
//! `test.unit_conversion` (`test-kinds` feature) is a test-only
//! operation with one `unit: mm` parameter (`clearance`), so these
//! tests exercise the real `Session::prepare` wiring without touching a
//! production operation's parameters.

use std::collections::BTreeMap;

use mf_command::{CommitOutcome, Invocation, Session, replay};
use mf_fixtures::tetrahedron;
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};

/// A one-object 3MF at `unit`, imported into a fresh session.
fn session_at(dir: &std::path::Path, unit: Unit) -> (Session, mf_command::ObjectId) {
    let path = dir.join("doc.3mf");
    let model = Model {
        metadata: Default::default(),
        unit,
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: "part".into(),
            mesh: tetrahedron(),
            transform: IDENTITY,
        }],
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, warnings) = s.import_model(read_model(&path).unwrap());
    assert!(warnings.is_empty(), "{warnings:?}");
    (s, ids[0])
}

fn invocation(target: mf_command::ObjectId, clearance: &str) -> Invocation {
    Invocation {
        operands: Vec::new(),
        op_id: "test.unit_conversion".into(),
        target,
        selection: None,
        raw_params: BTreeMap::from([("clearance".to_string(), clearance.to_string())]),
    }
}

/// (a): 0.2 mm in a metre document is recorded as 0.0002 (Review Focus 4's
/// example, applied to the wiring rather than to a real operation).
#[test]
fn clearance_in_mm_is_recorded_in_document_units() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, id) = session_at(dir.path(), Unit::Meter);
    assert_eq!(s.document().units(), Unit::Meter);
    let outcome = s.execute(invocation(id, "0.2")).unwrap();
    // The operation itself only ever sees the converted value (never the
    // raw 0.2 mm, never a profile or a unit to re-read).
    match outcome {
        CommitOutcome::NewRevision { report } => {
            assert_eq!(
                report["clearance_seen_by_op"],
                serde_json::json!(0.0002),
                "{report}"
            );
        }
        other => panic!("{other:?}"),
    }
    let record = s.records_to_current().last().unwrap().clone();
    assert_eq!(
        record.params.get_float("clearance").unwrap(),
        0.0002,
        "{:?}",
        record.params
    );
}

/// A millimetre document (the common case) converts by exactly 1.0.
#[test]
fn clearance_in_mm_is_unchanged_in_a_millimetre_document() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, id) = session_at(dir.path(), Unit::Millimeter);
    s.execute(invocation(id, "0.2")).unwrap();
    let record = s.records_to_current().last().unwrap().clone();
    assert_eq!(record.params.get_float("clearance").unwrap(), 0.2);
}

/// (c): preparing the same invocation twice gives identical recorded
/// params — `prepare`'s conversion is a pure function of the invocation
/// and the document's units, not cumulative state.
#[test]
fn preparing_the_same_invocation_twice_gives_identical_params() {
    let dir = tempfile::tempdir().unwrap();
    let (s, id) = session_at(dir.path(), Unit::Meter);
    let first = s.prepare(invocation(id, "0.2")).unwrap();
    let second = s.prepare(invocation(id, "0.2")).unwrap();
    assert_eq!(first.params(), second.params());
    assert_eq!(first.params().get_float("clearance").unwrap(), 0.0002);
}

/// (b): replaying a log uses the recorded (already-converted) value
/// unchanged. If replay re-converted, a metre-recorded 0.0002 replayed
/// would silently become a different number, and a mismatch (or worse,
/// a silent wrong answer) would follow.
#[test]
fn replay_uses_the_recorded_value_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, id) = session_at(dir.path(), Unit::Meter);
    s.execute(invocation(id, "0.2")).unwrap();
    let recorded = s
        .records_to_current()
        .last()
        .unwrap()
        .params
        .get_float("clearance")
        .unwrap();
    assert_eq!(recorded, 0.0002);

    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert!(report.all_verified(), "{:?}", report.results);

    // The replayed session's document is still the metre document the
    // log describes (replay re-imports the source, it doesn't change
    // units), and the replayed command's recorded clearance is the exact
    // bits that were saved, not re-derived from 0.2 mm a second time.
    assert_eq!(report.session.document().units(), Unit::Meter);
    let replayed = report
        .session
        .records_to_current()
        .last()
        .unwrap()
        .params
        .get_float("clearance")
        .unwrap();
    assert_eq!(replayed, recorded);
}

/// (b), continued: carrying a recorded command onto a *different* unit
/// document (not reachable through `replay()`, which always reconstructs
/// the document the log itself describes, but reachable through
/// `Session::execute_recorded`, the same non-converting path replay and
/// `reexecute` both use) must still use the recorded 0.0002 bit for bit
/// — not reconvert it as if it were fresh millimetre input.
#[test]
fn recorded_params_carried_onto_a_different_unit_document_are_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (mut source, id) = session_at(dir.path(), Unit::Meter);
    source.execute(invocation(id, "0.2")).unwrap();
    let recorded_params = source.records_to_current().last().unwrap().params.clone();
    assert_eq!(recorded_params.get_float("clearance").unwrap(), 0.0002);

    let (mut other_unit_doc, other_id) = session_at(dir.path(), Unit::Inch);
    other_unit_doc
        .execute_recorded("test.unit_conversion", other_id, None, recorded_params)
        .unwrap();
    let carried = other_unit_doc
        .records_to_current()
        .last()
        .unwrap()
        .params
        .get_float("clearance")
        .unwrap();
    assert_eq!(
        carried, 0.0002,
        "a recorded length must survive unchanged, not be reconverted for the new document's unit"
    );
}

/// (b), continued: `reexecute` (MF-HIST-002) is the other path that
/// carries a recorded command forward; it must not re-convert either.
#[test]
fn reexecute_uses_the_recorded_value_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, id) = session_at(dir.path(), Unit::Meter);
    let import = s.current();
    s.execute(invocation(id, "0.2")).unwrap();
    let record = s.records_to_current().last().unwrap().clone();
    assert_eq!(record.params.get_float("clearance").unwrap(), 0.0002);

    // Re-execute on the same (only) upstream revision: a no-op position
    // wise, but it must go through the params-already-typed path, not
    // back through raw-string validation and conversion.
    s.reexecute(&record, import).unwrap();
    let rerun = s.records_to_current().last().unwrap().clone();
    assert_eq!(rerun.params.get_float("clearance").unwrap(), 0.0002);
}
