#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Object properties (the Solid/Hole role): an undoable history step, kept
//! by geometry commands, logged (`mfx-log/2.1`) so replay reaches the same
//! roles, and read from 3MF object metadata on import.

use std::path::Path;

use mf_command::{ObjectId, Session, Verdict, replay};
use mf_document::{HOLE, ROLE};
use mf_fixtures::cube;
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};

fn session(dir: &Path, hole_in_file: bool) -> (Session, Vec<ObjectId>) {
    let path = dir.join("doc.3mf");
    let object = |name: &str, hole: bool| ModelObject {
        metadata: if hole {
            [("meshfixer:role".to_string(), HOLE.to_string())].into()
        } else {
            Default::default()
        },
        name: name.into(),
        mesh: cube(1.0),
        transform: IDENTITY,
    };
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![object("a", false), object("b", hole_in_file)],
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, _) = s.import_model(read_model(&path).unwrap());
    (s, ids)
}

fn is_hole(s: &Session, id: ObjectId) -> bool {
    s.document().object(id).unwrap().is_hole()
}

#[test]
fn role_is_an_undoable_step_and_replays() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = session(dir.path(), false);
    assert!(!is_hole(&s, ids[1]));
    assert!(s.set_property(ids[1], ROLE, Some(HOLE.into()), "Make b a hole".into()));
    assert!(is_hole(&s, ids[1]));
    // Setting the same value again is no step.
    assert!(!s.set_property(ids[1], ROLE, Some(HOLE.into()), "again".into()));
    assert!(s.undo());
    assert!(!is_hole(&s, ids[1]));
    assert!(s.redo());
    // A geometry command keeps the role.
    let mut moved = IDENTITY;
    moved[9] = 3.0;
    s.execute(mf_command::Invocation {
        op_id: "object.set_transform".into(),
        target: ids[1],
        selection: None,
        operands: Vec::new(),
        raw_params: [(
            "transform".to_string(),
            moved.map(|v| v.to_string()).join(","),
        )]
        .into(),
    })
    .unwrap();
    assert!(is_hole(&s, ids[1]));
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
    assert!(report.session.document().object(ids[1]).unwrap().is_hole());
    // An mfx-log/2 log (no property events) still replays.
    let manifest = log.join("manifest.json");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, text.replace("mfx-log/2.1", "mfx-log/2")).unwrap();
    let commands = log.join("commands.jsonl");
    let kept: Vec<String> = std::fs::read_to_string(&commands)
        .unwrap()
        .lines()
        .filter(|l| !l.contains("\"set_property\""))
        .map(str::to_owned)
        .collect();
    std::fs::write(&commands, kept.join("\n") + "\n").unwrap();
    let report = replay(&log).unwrap();
    assert!(!report.session.document().object(ids[1]).unwrap().is_hole());
}

#[test]
fn role_is_read_from_3mf_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let (s, ids) = session(dir.path(), true);
    assert!(!is_hole(&s, ids[0]));
    assert!(is_hole(&s, ids[1]));
    let metadata = s.document().object(ids[1]).unwrap().metadata();
    assert_eq!(
        metadata.get("meshfixer:role").map(String::as_str),
        Some(HOLE)
    );
}
