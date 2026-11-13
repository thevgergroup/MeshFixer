#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Multi-object 3MF import: names, transforms and units reach the document,
//! and a log that imports one replays exactly (MF-DOC-004).

use std::collections::BTreeMap;

use mf_command::{Invocation, Session, Verdict, replay};
use mf_fixtures::{cube, merge, tetrahedron, translated};
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};

fn three_part(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("parts.3mf");
    let shifted = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 20.0, 0.0, 0.0];
    let model = Model {
        unit: Unit::Inch,
        objects: vec![
            ModelObject {
                name: "base".into(),
                mesh: merge(&[cube(1.0), translated(&tetrahedron(), [5.0, 0.0, 0.0])]),
                transform: IDENTITY,
            },
            ModelObject {
                name: "lid".into(),
                mesh: cube(0.5),
                transform: shifted,
            },
            ModelObject {
                name: "pin".into(),
                mesh: tetrahedron(),
                transform: IDENTITY,
            },
        ],
    };
    write_model_path(&model, &path).unwrap();
    path
}

#[test]
fn import_model_keeps_names_transforms_units() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    let (ids, warnings) = s.import_model(read_model(&three_part(dir.path())).unwrap());
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(ids.len(), 3);
    let doc = s.document();
    assert_eq!(doc.units(), Unit::Inch);
    let names: Vec<&str> = ids
        .iter()
        .map(|id| doc.object(*id).unwrap().name.as_str())
        .collect();
    assert_eq!(names, ["base", "lid", "pin"]);
    assert_eq!(doc.object(ids[1]).unwrap().transform[9], 20.0);
}

#[test]
fn log_importing_a_multi_object_3mf_replays_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    let (ids, _) = s.import_model(read_model(&three_part(dir.path())).unwrap());
    s.execute(Invocation {
        op_id: "mesh.remove_small_components".into(),
        target: ids[0],
        selection: None,
        raw_params: BTreeMap::from([
            ("max_triangles".to_string(), "5".to_string()),
            ("max_relative_size".to_string(), "100".to_string()),
        ]),
    })
    .unwrap();
    let log = dir.path().join("s.mfx.log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert_eq!(report.results.len(), 1);
    assert!(
        matches!(report.results[0].1, Verdict::Exact),
        "{:?}",
        report.results
    );
    assert_eq!(report.session.document().objects().len(), 3);
    assert_eq!(report.session.document().units(), Unit::Inch);
}
