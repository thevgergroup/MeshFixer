#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Parametric shapes as first-class objects (parametric-shapes brief):
//! a placed shape keeps its recipe; editing a param regenerates its mesh
//! as one undo step that replays Exact; any other mesh operation bakes
//! it into a plain mesh.

use std::collections::BTreeMap;
use std::path::Path;

use mf_command::{GENERATOR, GROUP, Invocation, ObjectId, Session, Verdict, replay};
use mf_fixtures::cube;
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};
use mf_operations::shapes::{ShapeToAdd, generators_param, shapes_param};
use mf_parts::GeneratorRecord;

fn session(dir: &Path) -> (Session, ObjectId) {
    let path = dir.join("doc.3mf");
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: "base".into(),
            mesh: mf_fixtures::scaled(&cube(1.0), 40.0),
            transform: IDENTITY,
        }],
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, _) = s.import_model(read_model(&path).unwrap());
    (s, ids[0])
}

fn record(id: &str, raw: &[(&str, &str)]) -> GeneratorRecord {
    let shape = mf_parts::find_shape(id).unwrap();
    let raw: BTreeMap<String, String> = raw
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    mf_parts::new_record(shape, &raw).unwrap()
}

fn at(x: f64, y: f64, z: f64) -> String {
    let mut t = IDENTITY;
    t[9] = x;
    t[10] = y;
    t[11] = z;
    t.map(|v| v.to_string()).join(",")
}

fn add(s: &mut Session, target: ObjectId, shapes: &[ShapeToAdd], group: &str) -> Vec<ObjectId> {
    let before: Vec<ObjectId> = s.document().objects().iter().map(|o| o.id).collect();
    s.execute(Invocation {
        op_id: "object.add_shape".into(),
        target,
        selection: None,
        operands: Vec::new(),
        raw_params: BTreeMap::from([
            ("shapes".to_owned(), shapes_param(shapes)),
            ("transform".to_owned(), at(10.0, 10.0, 40.0)),
            ("group".to_owned(), group.to_owned()),
        ]),
    })
    .unwrap();
    s.document()
        .objects()
        .iter()
        .map(|o| o.id)
        .filter(|id| !before.contains(id))
        .collect()
}

fn set_params(s: &mut Session, ids: &[ObjectId], records: &[GeneratorRecord]) {
    s.execute(Invocation {
        op_id: "object.set_shape_params".into(),
        target: ids[0],
        selection: None,
        operands: ids[1..]
            .iter()
            .map(|&id| ("target".to_owned(), id, None))
            .collect(),
        raw_params: BTreeMap::from([("generators".to_owned(), generators_param(records))]),
    })
    .unwrap();
}

fn width(s: &Session, id: ObjectId) -> f64 {
    let bb = s
        .document()
        .object(id)
        .unwrap()
        .mesh()
        .unwrap()
        .bounding_box()
        .unwrap();
    bb.max[0] - bb.min[0]
}

fn recipe(s: &Session, id: ObjectId) -> Option<GeneratorRecord> {
    let text = s.document().object(id)?.properties.get(GENERATOR)?.clone();
    Some(GeneratorRecord::from_json(&text).unwrap())
}

/// Edit a param, then regenerate, then undo and redo, then replay Exact.
#[test]
fn editing_a_param_regenerates_undoes_redoes_and_replays_exact() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, base) = session(dir.path());
    let boxed = record("shape.box", &[("width", "10")]);
    let ids = add(
        &mut s,
        base,
        &[ShapeToAdd {
            generator: boxed.clone(),
            name: "Box".into(),
            hole: false,
        }],
        "",
    );
    assert_eq!(ids.len(), 1);
    let id = ids[0];
    assert!(s.document().object(id).unwrap().is_parametric());
    assert_eq!(recipe(&s, id).unwrap(), boxed);
    assert!((width(&s, id) - 10.0).abs() < 1e-9);
    let transform = s.document().object(id).unwrap().transform;

    let wider = record("shape.box", &[("width", "25")]);
    let steps = s.history_labels().len();
    set_params(&mut s, &[id], std::slice::from_ref(&wider));
    assert_eq!(s.history_labels().len(), steps + 1, "one undo step");
    assert!((width(&s, id) - 25.0).abs() < 1e-9);
    assert_eq!(recipe(&s, id).unwrap(), wider);
    let o = s.document().object(id).unwrap();
    assert_eq!(o.transform, transform, "the transform is kept");
    assert_eq!(o.name, "Box", "the name is kept");

    assert!(s.undo());
    assert!((width(&s, id) - 10.0).abs() < 1e-9);
    assert_eq!(recipe(&s, id).unwrap(), boxed);
    assert!(s.redo());
    assert!((width(&s, id) - 25.0).abs() < 1e-9);
    assert_eq!(recipe(&s, id).unwrap(), wider);

    let log = dir.path().join("shapes.mfx.log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
    assert_eq!(recipe(&report.session, id).unwrap(), wider);
}

/// A mesh operation (Combine) bakes the shape: its result is a plain
/// mesh, with no recipe left on it.
#[test]
fn combine_bakes_a_shape_into_a_mesh() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, base) = session(dir.path());
    let ids = add(
        &mut s,
        base,
        &[
            ShapeToAdd {
                generator: record("shape.box", &[]),
                name: "Box".into(),
                hole: false,
            },
            ShapeToAdd {
                generator: record("shape.cylinder", &[("diameter", "6"), ("height", "40")]),
                name: "Cylinder".into(),
                hole: true,
            },
        ],
        "",
    );
    let (solid, hole) = (ids[0], ids[1]);
    assert!(s.document().object(hole).unwrap().is_hole());
    // The cylinder sticks out of the box on both ends so the cut is clean.
    let mut t = s.document().object(hole).unwrap().transform;
    t[11] -= 10.0;
    s.execute(Invocation {
        op_id: "object.set_transform".into(),
        target: hole,
        selection: None,
        operands: Vec::new(),
        raw_params: BTreeMap::from([("transform".to_owned(), t.map(|v| v.to_string()).join(","))]),
    })
    .unwrap();
    assert!(
        s.document().object(hole).unwrap().is_parametric(),
        "moving a shape keeps it editable"
    );
    s.execute(Invocation {
        op_id: "mesh.combine".into(),
        target: solid,
        selection: None,
        operands: vec![("holes".to_owned(), hole, None)],
        raw_params: BTreeMap::new(),
    })
    .unwrap();
    let o = s.document().object(solid).unwrap();
    assert!(!o.is_parametric(), "Combine bakes the shape");
    assert!(!o.properties.contains_key(GROUP));
    assert!(s.document().object(hole).is_none());
    assert!(s.undo());
    assert!(s.document().object(solid).unwrap().is_parametric());
}

/// Every member of a linked group gets the group id, and one edit
/// regenerates them all together.
#[test]
fn a_group_is_regenerated_by_one_edit() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, base) = session(dir.path());
    let peg = mf_parts::connectors()
        .into_iter()
        .find(|c| c.id() == "joint.peg")
        .unwrap();
    let params = peg.spec().validate(&BTreeMap::new()).unwrap();
    let member = |part: &str, params: &mf_spec::Params| GeneratorRecord {
        id: "joint.peg".into(),
        version: peg.version(),
        params: params.clone(),
        part: Some(part.into()),
    };
    let ids = add(
        &mut s,
        base,
        &[
            ShapeToAdd {
                generator: member("peg", &params),
                name: "Peg".into(),
                hole: false,
            },
            ShapeToAdd {
                generator: member("counterpart hole", &params),
                name: "Peg hole".into(),
                hole: true,
            },
        ],
        "g1",
    );
    for &id in &ids {
        assert_eq!(s.document().object(id).unwrap().properties[GROUP], "g1");
    }
    let before: Vec<f64> = ids.iter().map(|&id| width(&s, id)).collect();
    let mut wider = params.clone();
    wider
        .0
        .insert("diameter".into(), mf_spec::ParamValue::Float(9.0));
    set_params(
        &mut s,
        &ids,
        &[member("peg", &wider), member("counterpart hole", &wider)],
    );
    for (k, &id) in ids.iter().enumerate() {
        assert!(width(&s, id) > before[k] + 3.0, "member {k} regenerated");
        assert_eq!(
            recipe(&s, id)
                .unwrap()
                .params
                .get_float("diameter")
                .unwrap(),
            9.0
        );
    }
}
