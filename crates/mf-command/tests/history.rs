#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Revision DAG, command records and selection translation through the
//! session (MF-HIST-001, MF-CMD-002, MF-SEL-003).

use std::collections::BTreeMap;

use mf_command::{CommandRecord, Invocation, ObjectId, Session};
use mf_fixtures::{cube, merge, tetrahedron, translated};
use mf_geometry::{ContentHash, Mesh};
use mf_io::{Format, Imported, SourceInfo, SourcePrecision};
use mf_selection::{Selection, SelectionKind, TranslateError, Translation};

fn imported(mesh: Mesh) -> Imported {
    Imported {
        mesh,
        source: SourceInfo {
            path: "part.stl".into(),
            content_hash: ContentHash([7; 32]),
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

#[test]
fn dag_retains_abandoned_branch() {
    let mut s = Session::new();
    let id = s.import(imported(damaged()), "part".into());
    let a = s.current();
    s.execute(remove(id, "5")).unwrap();
    let b = s.current();
    assert!(s.undo());
    s.execute(remove(id, "6")).unwrap();
    let c = s.current();
    assert_eq!(s.node(a).unwrap().children, vec![b, c]);
    assert_eq!(s.node(b).unwrap().parent, Some(a));
    assert_eq!(s.node(c).unwrap().parent, Some(a));
    assert!(s.checkout(b), "abandoned branch is still reachable");
    assert_eq!(s.current(), b);
    assert!(!s.checkout(mf_document::RevisionId(999)));
}

#[test]
fn redo_follows_most_recent_child() {
    let mut s = Session::new();
    let id = s.import(imported(damaged()), "part".into());
    s.execute(remove(id, "5")).unwrap();
    s.undo();
    s.execute(remove(id, "6")).unwrap();
    let c = s.current();
    s.undo();
    s.redo();
    assert_eq!(s.current(), c);
}

#[test]
fn record_is_complete_and_roundtrips_json() {
    let mut s = Session::new();
    let id = s.import(imported(damaged()), "part".into());
    let input_rev = s.current();
    s.execute(remove(id, "5")).unwrap();
    let records = s.records_to_current();
    assert_eq!(records.len(), 1);
    let r: &CommandRecord = &records[0];
    assert_eq!(r.op_id, "mesh.remove_small_components");
    assert_eq!(r.op_version, 2);
    assert_eq!(r.target, id);
    assert_eq!(r.input_revision, input_rev);
    assert_eq!(r.output_revision, s.current());
    assert_ne!(r.input_hash, r.output_hash);
    assert!(r.depends_on.is_empty(), "the import produced the input");
    assert_eq!(r.affected.removed, 4);
    assert_eq!(r.affected.created, 0);
    assert_eq!(r.affected.kept, 12);
    assert_eq!(r.affected_objects, vec![id]);
    assert_eq!(
        r.platform,
        format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
    );
    let back: CommandRecord = serde_json::from_str(&serde_json::to_string(r).unwrap()).unwrap();
    assert_eq!(&back, r);
}

#[test]
fn second_command_depends_on_first() {
    let mut s = Session::new();
    let id = s.import(
        imported(merge(&[
            cube(1.0),
            translated(&tetrahedron(), [5.0, 0.0, 0.0]),
            translated(&cube(0.1), [9.0, 0.0, 0.0]),
        ])),
        "part".into(),
    );
    s.execute(remove(id, "5")).unwrap(); // removes the tetrahedron
    s.execute(remove(id, "13")).unwrap(); // removes the small cube
    let records = s.records_to_current();
    assert_eq!(records[1].depends_on, vec![records[0].id]);
}

#[test]
fn face_selection_maps_across_removal() {
    let mut s = Session::new();
    let id = s.import(imported(damaged()), "part".into());
    let before = s.current();
    s.execute(remove(id, "5")).unwrap();
    let sel = Selection {
        object: id,
        revision: before,
        kind: SelectionKind::Faces,
        ids: vec![0, 1, 14],
    };
    match s.translate(&sel, s.current()) {
        Translation::Partial { mapped, lost } => {
            assert_eq!(mapped.ids, vec![0, 1]);
            assert_eq!(lost, vec![14]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn translate_across_branches() {
    let mut s = Session::new();
    let id = s.import(imported(damaged()), "part".into());
    s.execute(remove(id, "5")).unwrap();
    let b = s.current();
    s.undo();
    s.execute(remove(id, "6")).unwrap();
    let c = s.current();
    // From one branch to its sibling, through their common ancestor.
    let sel = Selection {
        object: id,
        revision: b,
        kind: SelectionKind::Faces,
        ids: vec![0, 3],
    };
    assert!(
        matches!(s.translate(&sel, c), Translation::Mapped(m) if m.ids == vec![0, 3] && m.revision == c)
    );
}

#[test]
fn translate_before_import_reports_missing_object() {
    let mut s = Session::new();
    let first = s.import(imported(cube(1.0)), "a".into());
    let before_second = s.current();
    let second = s.import(imported(cube(2.0)), "b".into());
    let sel = Selection {
        object: second,
        revision: s.current(),
        kind: SelectionKind::Object,
        ids: vec![],
    };
    assert_eq!(
        s.translate(&sel, before_second),
        Translation::Failed(TranslateError::ObjectMissing)
    );
    let _ = first;
}

#[test]
fn record_floats_parse_back_exactly() {
    // 0.171417 parsed back as 0.17141699999999999 without serde_json's
    // `float_roundtrip` feature (seen as a flaky record round-trip).
    let back: f64 = serde_json::from_str(&serde_json::to_string(&0.171417_f64).unwrap()).unwrap();
    assert_eq!(back.to_bits(), 0.171417_f64.to_bits());
}

#[test]
fn ten_revisions_cost_under_one_and_a_half_times_one() {
    // MF-DOC-002: ten successive revisions use < 1.5× the memory of one.
    // A body plus ten islands of distinct sizes; each command removes one more.
    let mut parts = vec![mf_fixtures::uv_sphere(100, 200)];
    for i in 0..10u32 {
        parts.push(translated(
            &mf_fixtures::uv_sphere(2 + i, 3 + i),
            [10.0 + 5.0 * i as f64, 0.0, 0.0],
        ));
    }
    let mut s = Session::new();
    let id = s.import(imported(merge(&parts)), "part".into());
    let sizes: Vec<usize> = (0..10u32)
        .map(|i| 2 * (3 + i as usize) * (1 + i as usize))
        .collect();
    for size in sizes {
        let out = s.execute(remove(id, &(size + 1).to_string())).unwrap();
        assert!(matches!(out, mf_command::CommitOutcome::NewRevision { .. }));
    }
    assert_eq!(s.records_to_current().len(), 10);
    let current = s.document().object(id).unwrap().mesh().unwrap().clone();
    let _ = current.topology(); // the current revision is analysed (as the UI does)
    let one = current.heap_bytes() + current.topology().heap_bytes();
    let all = s.history_bytes();
    eprintln!(
        "history: {all} B for 10 revisions vs {one} B for one ({:.2}×)",
        all as f64 / one as f64
    );
    assert!(
        (all as f64) < 1.5 * one as f64,
        "history {all} B vs one revision {one} B ({:.2}×)",
        all as f64 / one as f64
    );
}
