#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `document.reinterpret_units` and `document.scale_units` (consolidation
//! plan C07, import-units-brief.md "Fix it afterwards"): the Inspect
//! "Units" control's two actions. Both are recorded, undoable commands,
//! and both round-trip through undo and replay as `Verdict::Exact`.

use std::collections::BTreeMap;

use mf_command::{CommitOutcome, Invocation, Session, Verdict, replay};
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::write_model_path;

/// A one-object mm document (a 10 mm cube) at a fresh path, imported
/// into a new session.
fn session_with_cube(dir: &std::path::Path) -> (Session, mf_command::ObjectId) {
    let path = dir.join("doc.3mf");
    let model = Model {
        metadata: Default::default(),
        unit: Unit::Millimeter,
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: "part".into(),
            mesh: mf_fixtures::cube(10.0),
            transform: IDENTITY,
        }],
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, warnings) = s.import_model(mf_io::read_model(&path).unwrap());
    assert!(warnings.is_empty(), "{warnings:?}");
    (s, ids[0])
}

fn reinterpret(target: mf_command::ObjectId, unit: &str) -> Invocation {
    Invocation {
        op_id: "document.reinterpret_units".into(),
        target,
        selection: None,
        operands: Vec::new(),
        raw_params: BTreeMap::from([("unit".to_owned(), unit.to_owned())]),
    }
}

fn scale(target: mf_command::ObjectId, unit: &str) -> Invocation {
    Invocation {
        op_id: "document.scale_units".into(),
        target,
        selection: None,
        operands: Vec::new(),
        raw_params: BTreeMap::from([("unit".to_owned(), unit.to_owned())]),
    }
}

/// Reinterpret: the document's unit changes, but the object's own
/// coordinates and transform stay bit-for-bit what they were. Undo puts
/// the document's unit back exactly. Replays Exact.
#[test]
fn reinterpret_round_trips_with_undo_and_replays_exact() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, part) = session_with_cube(dir.path());
    assert_eq!(s.document().units(), Unit::Millimeter);
    let before_mesh = s.document().object(part).unwrap().geometry.content_hash();
    let before_transform = s.document().object(part).unwrap().transform;

    let outcome = s.execute(reinterpret(part, "m")).unwrap();
    assert!(
        matches!(outcome, CommitOutcome::NewRevision { .. }),
        "{outcome:?}"
    );
    assert_eq!(s.document().units(), Unit::Meter, "relabelled");
    // Same numbers, new label: the object's own data is untouched.
    assert_eq!(
        s.document().object(part).unwrap().geometry.content_hash(),
        before_mesh,
        "reinterpret must not touch the mesh"
    );
    assert_eq!(
        s.document().object(part).unwrap().transform,
        before_transform,
        "reinterpret must not touch the transform"
    );
    let bbox = s
        .document()
        .object(part)
        .unwrap()
        .mesh()
        .unwrap()
        .bounding_box()
        .unwrap();
    assert_eq!(
        bbox.max[0] - bbox.min[0],
        10.0,
        "the raw numbers are unchanged, only their meaning"
    );

    assert!(s.undo());
    assert_eq!(s.document().units(), Unit::Millimeter, "undo relabels back");
    assert_eq!(
        s.document().object(part).unwrap().geometry.content_hash(),
        before_mesh
    );

    assert!(s.redo());
    assert_eq!(s.document().units(), Unit::Meter);

    let log = dir.path().join("log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
    assert_eq!(report.session.document().units(), Unit::Meter);
}

/// Scale: the document keeps its unit, but every target's physical size
/// (its transform) changes by the unit ratio. Undo restores the original
/// transform exactly. Replays Exact.
#[test]
fn scale_round_trips_with_undo_and_replays_exact() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, part) = session_with_cube(dir.path());
    assert_eq!(s.document().units(), Unit::Millimeter);
    let before_transform = s.document().object(part).unwrap().transform;
    let before_mesh = s.document().object(part).unwrap().geometry.content_hash();

    // "Scale": the numbers were actually metres, so a 10 mm cube becomes
    // a 10 m cube (10,000x bigger), read in the document's own mm.
    let outcome = s.execute(scale(part, "m")).unwrap();
    assert!(
        matches!(outcome, CommitOutcome::NewRevision { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        s.document().units(),
        Unit::Millimeter,
        "scale never relabels the document"
    );
    assert_eq!(
        s.document().object(part).unwrap().geometry.content_hash(),
        before_mesh,
        "scale changes placement, never geometry"
    );
    let after = s.document().object(part).unwrap().transform;
    // The ratio of a metre to a millimetre is 1000.
    for k in 0..12 {
        assert!(
            (after[k] - before_transform[k] * 1000.0).abs() < 1e-9,
            "index {k}: {after:?} vs {before_transform:?} * 1000"
        );
    }

    assert!(s.undo());
    assert_eq!(
        s.document().object(part).unwrap().transform,
        before_transform,
        "undo restores the original transform exactly"
    );

    assert!(s.redo());
    assert_eq!(s.document().object(part).unwrap().transform, after);

    let log = dir.path().join("log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
    assert_eq!(
        report.session.document().object(part).unwrap().transform,
        after
    );
}
