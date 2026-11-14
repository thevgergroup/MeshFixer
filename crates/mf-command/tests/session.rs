#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::collections::BTreeMap;

use mf_command::{CommandError, CommitOutcome, Invocation, ObjectId, Session};
use mf_fixtures::{cube, merge, tetrahedron, translated};
use mf_geometry::{ContentHash, Mesh};
use mf_io::{Format, Imported, SourceInfo, SourcePrecision};

fn imported(mesh: Mesh) -> Imported {
    Imported {
        mesh,
        source: SourceInfo {
            path: "test.stl".into(),
            content_hash: ContentHash([0; 32]),
            format: Format::StlBinary,
            precision: SourcePrecision::F32,
            exporter_hint: None,
        },
        warnings: vec![],
    }
}

fn damaged() -> Mesh {
    merge(&[cube(1.0), translated(&tetrahedron(), [5.0, 0.0, 0.0])])
}

fn remove(target: ObjectId, max: &str) -> Invocation {
    Invocation {
        operands: Vec::new(),
        selection: None,
        op_id: "mesh.remove_small_components".into(),
        target,
        raw_params: BTreeMap::from([
            ("max_triangles".to_string(), max.to_string()),
            ("max_relative_size".to_string(), "100".to_string()),
        ]),
    }
}

fn hash_of(s: &Session, id: ObjectId) -> ContentHash {
    s.document().object(id).unwrap().geometry.content_hash()
}

#[test]
fn execute_undo_redo() {
    let mut s = Session::new();
    let id = s.import(imported(damaged()), "part".into());
    let h0 = hash_of(&s, id);
    assert!(matches!(
        s.execute(remove(id, "5")).unwrap(),
        CommitOutcome::NewRevision { .. }
    ));
    let h1 = hash_of(&s, id);
    assert_ne!(h0, h1);
    assert!(s.undo());
    assert_eq!(hash_of(&s, id), h0);
    assert!(s.redo());
    assert_eq!(hash_of(&s, id), h1);
    assert!(!s.redo());
    assert_eq!(s.history_labels().len(), 3); // new, import, remove
}

#[test]
fn undo_import_removes_object() {
    let mut s = Session::new();
    let id = s.import(imported(cube(1.0)), "part".into());
    assert!(s.undo());
    assert!(s.document().object(id).is_none());
    assert!(!s.undo());
}

#[test]
fn new_command_after_undo_truncates_redo() {
    let mut s = Session::new();
    let id = s.import(imported(damaged()), "part".into());
    s.execute(remove(id, "5")).unwrap();
    s.undo();
    // A different command that changes something (max 6 also removes the
    // 4-face island; max 3 would remove nothing and create no revision).
    assert!(matches!(
        s.execute(remove(id, "6")).unwrap(),
        CommitOutcome::NewRevision { .. }
    ));
    assert!(!s.can_redo());
}

#[test]
fn failed_op_leaves_document() {
    let mut s = Session::new();
    let id = s.import(imported(damaged()), "part".into());
    let rev = s.revision();
    let h = hash_of(&s, id);
    assert!(matches!(
        s.execute(remove(id, "0")),
        Err(CommandError::Spec(_))
    ));
    assert!(matches!(
        s.execute(remove(ObjectId(999), "5")),
        Err(CommandError::UnknownObject(_))
    ));
    let mut bad = remove(id, "5");
    bad.op_id = "mesh.nope".into();
    assert!(matches!(
        s.execute(bad),
        Err(CommandError::UnknownOperation(_))
    ));
    assert_eq!(s.revision(), rev);
    assert_eq!(hash_of(&s, id), h);
}

#[test]
fn stale_commit_rejected() {
    let mut s = Session::new();
    let id = s.import(imported(damaged()), "part".into());
    let prepared = s.prepare(remove(id, "5")).unwrap();
    let completed = std::thread::spawn(move || prepared.run(&mf_runtime::RunContext::none()))
        .join()
        .unwrap()
        .unwrap();
    s.undo();
    let rev = s.revision();
    assert!(matches!(
        s.commit(completed),
        Err(CommandError::StaleInput { .. })
    ));
    assert_eq!(s.revision(), rev);
}

#[test]
fn stale_after_undo_then_new_command() {
    // Same history depth but different content must still be stale.
    let mut s = Session::new();
    let id = s.import(imported(damaged()), "part".into());
    s.execute(remove(id, "5")).unwrap();
    let prepared = s.prepare(remove(id, "5")).unwrap();
    s.undo();
    s.execute(remove(id, "3")).unwrap();
    let completed = prepared.run(&mf_runtime::RunContext::none()).unwrap();
    assert!(matches!(
        s.commit(completed),
        Err(CommandError::StaleInput { .. })
    ));
}

#[test]
fn read_only_does_not_create_revision() {
    let mut s = Session::new();
    let id = s.import(imported(cube(1.0)), "part".into());
    let rev = s.revision();
    let out = s
        .execute(Invocation {
            operands: Vec::new(),
            selection: None,
            op_id: "mesh.inspect".into(),
            target: id,
            raw_params: BTreeMap::new(),
        })
        .unwrap();
    match out {
        CommitOutcome::Report(r) => assert_eq!(r["components"], 1),
        _ => panic!("inspect must not create a revision"),
    }
    assert_eq!(s.revision(), rev);
}

#[test]
fn object_ids_never_reused() {
    let mut s = Session::new();
    let a = s.import(imported(cube(1.0)), "a".into());
    s.undo();
    let b = s.import(imported(cube(1.0)), "b".into());
    assert_ne!(a, b);
}

#[test]
fn noop_operation_creates_no_revision() {
    let mut s = Session::new();
    let id = s.import(imported(cube(1.0)), "part".into());
    let rev = s.revision();
    let out = s.execute(remove(id, "5")).unwrap();
    assert!(matches!(out, CommitOutcome::NoChange { .. }), "{out:?}");
    assert_eq!(s.revision(), rev);
    assert_eq!(s.history_labels().len(), 2);
}
