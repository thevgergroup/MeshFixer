#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `mesh.hollow` through the command layer (MF-ISSUE-112): one undo puts
//! the solid back; replay is Exact (drain holes included); a cancelled
//! run, at any stage, changes nothing.

use std::collections::BTreeMap;
use std::path::Path;

use mf_command::{Invocation, ObjectId, Session, Verdict, replay};
use mf_fixtures::{scaled, uv_sphere};
use mf_geometry::Mesh;
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};
use mf_runtime::RunContext;

fn session_with(dir: &Path, mesh: Mesh) -> (Session, ObjectId) {
    let path = dir.join("doc.3mf");
    let model = Model {
        metadata: Default::default(),
        unit: Unit::Millimeter,
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: "ball".into(),
            mesh,
            transform: IDENTITY,
        }],
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, _) = s.import_model(read_model(&path).unwrap());
    (s, ids[0])
}

fn hollow(target: ObjectId, params: &[(&str, &str)]) -> Invocation {
    Invocation {
        op_id: "mesh.hollow".into(),
        target,
        selection: None,
        operands: Vec::new(),
        raw_params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn hash(s: &Session, id: ObjectId) -> String {
    s.document()
        .object(id)
        .unwrap()
        .geometry
        .content_hash()
        .to_hex()
}

const PARAMS: [(&str, &str); 3] = [
    ("wall_thickness", "1.5"),
    ("inner_detail", "0.5"),
    ("drain_holes", "auto"),
];

#[test]
fn hollow_undoes_in_one_step_and_replays_exact() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, id) = session_with(dir.path(), scaled(&uv_sphere(32, 64), 15.0));
    let before = hash(&s, id);
    let revision = s.revision();
    s.execute(hollow(id, &PARAMS)).unwrap();
    let after = hash(&s, id);
    assert_ne!(before, after);
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
    assert_eq!(hash(&report.session, id), after);
    assert!(s.undo());
    assert_eq!(s.revision(), revision);
    assert_eq!(hash(&s, id), before);
}

fn split(target: ObjectId, params: &[(&str, &str)]) -> Invocation {
    Invocation {
        op_id: "mesh.split_plane".into(),
        target,
        selection: None,
        operands: Vec::new(),
        raw_params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
    }
}

/// `open_through` (owner 2026-11-13): a split-sphere cap, picked by its
/// stable input-face seeds, opens into a single undo step that replays
/// Exact — the picked region is recorded as face ids, not re-derived.
#[test]
fn hollow_open_through_undoes_in_one_step_and_replays_exact() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, id) = session_with(dir.path(), scaled(&uv_sphere(48, 96), 20.0));
    let cut = s
        .execute(split(
            id,
            &[
                ("plane_point", "0,0,2.0"),
                ("plane_normal", "0,0,1"),
                ("keep", "above"),
                ("cap", "true"),
            ],
        ))
        .unwrap();
    let mf_command::CommitOutcome::NewRevision { report } = cut else {
        panic!("expected a new revision");
    };
    let cap_faces: Vec<u32> = report["parts"][0]["cap_faces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u32)
        .collect();
    assert!(!cap_faces.is_empty(), "{report}");
    let faces_param = cap_faces
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let before = hash(&s, id);
    let revision = s.revision();
    s.execute(hollow(
        id,
        &[
            ("wall_thickness", "2"),
            ("inner_detail", "0.5"),
            ("open_through", "true"),
            ("open_through_faces", &faces_param),
        ],
    ))
    .unwrap();
    let after = hash(&s, id);
    assert_ne!(before, after);
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
    assert_eq!(hash(&report.session, id), after);
    assert!(s.undo());
    assert_eq!(s.revision(), revision);
    assert_eq!(hash(&s, id), before);
}

#[test]
fn a_cancelled_hollow_changes_nothing_at_any_stage() {
    let dir = tempfile::tempdir().unwrap();
    let (s, id) = session_with(dir.path(), scaled(&uv_sphere(32, 64), 15.0));
    let before = hash(&s, id);
    let revision = s.revision();
    for stage in mf_operations::hollow::STAGES {
        let prepared = s.prepare(hollow(id, &PARAMS)).unwrap();
        let r = prepared.run(&RunContext::cancelling_at(stage));
        assert!(r.is_err(), "{stage}: should be cancelled");
        assert_eq!(s.revision(), revision, "{stage}");
        assert_eq!(hash(&s, id), before, "{stage}");
    }
}
