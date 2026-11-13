#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `mesh.combine` through the command layer: solids joined and holes cut as
//! one command (variadic operands), checked by volume; one undo restores
//! every object with its id; replay is Exact; a refusal names its object.

use std::collections::BTreeMap;
use std::path::Path;

use mf_command::{Invocation, ObjectId, Session, Verdict, replay};
use mf_fixtures::cube;
use mf_geometry::{Mesh, signed_volume};
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};

fn session_with(dir: &Path, objects: Vec<(&str, Mesh, [f64; 12])>) -> (Session, Vec<ObjectId>) {
    let path = dir.join("doc.3mf");
    let model = Model {
        metadata: Default::default(),
        unit: Unit::Millimeter,
        objects: objects
            .into_iter()
            .map(|(name, mesh, transform)| ModelObject {
                metadata: Default::default(),
                name: name.into(),
                mesh,
                transform,
            })
            .collect(),
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, _) = s.import_model(read_model(&path).unwrap());
    (s, ids)
}

fn at(x: f64, y: f64, z: f64) -> [f64; 12] {
    let mut t = IDENTITY;
    t[9..].copy_from_slice(&[x, y, z]);
    t
}

fn combine(target: ObjectId, solids: &[ObjectId], holes: &[ObjectId]) -> Invocation {
    Invocation {
        op_id: "mesh.combine".into(),
        target,
        selection: None,
        operands: solids
            .iter()
            .map(|&o| ("solids".to_string(), o, None))
            .chain(holes.iter().map(|&o| ("holes".to_string(), o, None)))
            .collect(),
        raw_params: BTreeMap::new(),
    }
}

/// Two overlapping 2 mm cubes and a 1 mm hole poking out of the second.
fn scene(dir: &Path) -> (Session, Vec<ObjectId>) {
    session_with(
        dir,
        vec![
            ("a", cube(2.0), IDENTITY),
            ("b", cube(2.0), at(1.0, 0.5, 0.5)),
            ("hole", cube(1.0), at(2.2, 1.1, 1.1)),
        ],
    )
}

fn state(s: &Session) -> Vec<(ObjectId, String, String)> {
    s.document()
        .objects()
        .iter()
        .map(|o| (o.id, o.name.clone(), o.geometry.content_hash().to_hex()))
        .collect()
}

#[test]
fn solid_solid_hole_combines_by_volume_and_undoes_as_one_step() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = scene(dir.path());
    let before = state(&s);
    let revision = s.revision();
    s.execute(combine(ids[0], &[ids[1]], &[ids[2]])).unwrap();
    // a ∪ b = 8 + 8 − 1·1.5·1.5; the hole cuts 0.8·1·1 out of b.
    let result = s.document().object(ids[0]).unwrap();
    assert_eq!(result.name, "a", "the first solid keeps its name");
    let v = signed_volume(result.mesh().unwrap()) / 6.0;
    assert!((v - (16.0 - 2.25 - 0.8)).abs() < 1e-9, "volume {v}");
    assert_eq!(
        s.document().objects().len(),
        1,
        "b and the hole are consumed"
    );
    let record = s
        .node(s.current())
        .unwrap()
        .record
        .as_ref()
        .unwrap()
        .clone();
    let slot = |n: &str| -> Vec<ObjectId> { record.operand_slot(n).map(|o| o.object).collect() };
    assert_eq!(slot("target"), vec![ids[0]]);
    assert_eq!(slot("solids"), vec![ids[1]]);
    assert_eq!(slot("holes"), vec![ids[2]]);
    // One undo brings every object back with its id, name and geometry.
    assert!(s.undo());
    assert_eq!(s.revision(), revision);
    assert_eq!(state(&s), before);
}

#[test]
fn keep_holes_keeps_the_hole_objects() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = scene(dir.path());
    let mut inv = combine(ids[0], &[ids[1]], &[ids[2]]);
    inv.raw_params.insert("keep_holes".into(), "true".into());
    s.execute(inv).unwrap();
    assert!(s.document().object(ids[1]).is_none());
    assert!(s.document().object(ids[2]).is_some());
}

#[test]
fn combine_replays_exact() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = scene(dir.path());
    s.execute(combine(ids[0], &[ids[1]], &[ids[2]])).unwrap();
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
    assert_eq!(state(&report.session), state(&s));
}

#[test]
fn a_refused_step_names_its_object_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = session_with(
        dir.path(),
        vec![
            ("a", cube(2.0), IDENTITY),
            ("b", cube(2.0), at(1.0, 0.5, 0.5)),
            ("far hole", cube(1.0), at(20.0, 0.0, 0.0)),
        ],
    );
    let before = state(&s);
    let err = s
        .execute(combine(ids[0], &[ids[1]], &[ids[2]]))
        .unwrap_err()
        .to_string();
    assert!(err.contains("'far hole'"), "{err}");
    assert_eq!(state(&s), before);
    // Nothing but the target is nothing to combine.
    let err = s
        .execute(combine(ids[0], &[], &[]))
        .unwrap_err()
        .to_string();
    assert!(err.contains("nothing to join"), "{err}");
}
