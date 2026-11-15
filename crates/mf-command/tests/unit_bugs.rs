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
use mf_operations::shapes::{GeneratedMember, ShapeToAdd, generated_param, shapes_param};

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

/// `split.place_connectors` version 1 recorded a `mm` param
/// (units-audit.md's "fake `mm` length param": a scale factor disguised
/// as a length). C12 drops the param (version 2 reads
/// [`mf_operations::DocContext::mm_to_doc`] instead) but keeps
/// `upgrade_params(1, _) -> true`, so a log an older build wrote — `mm`
/// sitting in its recorded params, `op_version: 1` — must still replay
/// Exact: `upgrade_params` accepts it, the op never reads the leftover
/// `mm` key back, and the geometry it produces is identical to what the
/// current build's own connectors placement makes.
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
        ],
    );
    place.operands = vec![("side_b".into(), top, None)];
    s.execute(place).unwrap();

    // The current build records version 2, with no `mm` param.
    let current_version = s.records_to_current().last().unwrap().op_version;
    assert_eq!(current_version, 2);
    assert!(
        s.records_to_current()
            .last()
            .unwrap()
            .params
            .get_float("mm")
            .is_err(),
        "version 2 no longer records a `mm` param"
    );

    let log = dir.path().join("log");
    s.save_log(&log).unwrap();

    // Hand-edit the saved log's `split.place_connectors` event to look
    // like a version 1 build wrote it: `op_version: 1`, and the `mm`
    // param it used to carry (document units per millimetre; 1.0 here,
    // a millimetre document).
    let commands = log.join("commands.jsonl");
    let text = std::fs::read_to_string(&commands).unwrap();
    let mut lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let place_event = lines
        .iter_mut()
        .find(|e| e["kind"] == "command" && e["op_id"] == "split.place_connectors")
        .unwrap();
    place_event["op_version"] = serde_json::json!(1);
    place_event["params"]["mm"] = serde_json::json!(1.0);
    let changed: Vec<String> = lines.iter().map(|e| e.to_string()).collect();
    std::fs::write(&commands, changed.join("\n") + "\n").unwrap();

    let report = replay(&log).unwrap();
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "an old log carrying split.place_connectors' version-1 mm param must still replay \
         Exact: {:?}",
        report.results
    );
}

/// A document in `unit`, with two 40 mm-equivalent cubes ("a" below the
/// z = 0 plane, "b" above it, both centred on x = y = 0), stacked so a
/// `joint.*` connector's own frame at the origin (z into A at negative
/// z, into B at positive z, D16's local-frame convention) sits right on
/// their shared face.
fn two_scans(dir: &std::path::Path, unit: Unit) -> (Session, ObjectId, ObjectId) {
    let edge = 40.0 / unit.mm_per();
    let mut ta = IDENTITY;
    (ta[9], ta[10], ta[11]) = (-edge / 2.0, -edge / 2.0, -edge);
    let mut tb = IDENTITY;
    (tb[9], tb[10], tb[11]) = (-edge / 2.0, -edge / 2.0, 0.0);
    let model = Model {
        metadata: Default::default(),
        unit,
        objects: vec![
            ModelObject {
                metadata: Default::default(),
                name: "a".into(),
                mesh: mf_fixtures::cube(edge),
                transform: ta,
            },
            ModelObject {
                metadata: Default::default(),
                name: "b".into(),
                mesh: mf_fixtures::cube(edge),
                transform: tb,
            },
        ],
    };
    let path = dir.join(format!("two-{}.3mf", unit.symbol().replace('µ', "u")));
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, warnings) = s.import_model(read_model(&path).unwrap());
    assert!(warnings.is_empty(), "{warnings:?}");
    (s, ids[0], ids[1])
}

/// A generator's mesh, placed by whichever op (consolidation plan C12:
/// `object.add_shape`, `object.attach`'s `generated` field and
/// `split.place_connectors`), must have the same physical size in every
/// document unit — the generator itself always builds in mm (D16), and
/// the placing op is the one place that scales it, by `DocContext`.
/// Covers a plain shape (`shape.box`, 10 mm on a side), a connector
/// built in place on two targets (`joint.dowel`, a 5 mm hole and an
/// 11.6 mm loose dowel) and a split connector (the same `joint.dowel`,
/// placed across a cut by `split.place_connectors`).
#[test]
fn generated_shape_has_same_physical_size_in_mm_m_in() {
    let dir = tempfile::tempdir().unwrap();
    for unit in [Unit::Millimeter, Unit::Meter, Unit::Inch] {
        let mm = unit.mm_per();

        // A plain shape: `shape.box`, 10 mm on a side (object.add_shape).
        let (mut s, a, _b) = two_scans(dir.path(), unit);
        let box_gen = mf_parts::find_generator("shape.box").unwrap();
        let record = mf_parts::new_record(box_gen, &BTreeMap::new()).unwrap();
        let shapes = [ShapeToAdd {
            generator: record,
            name: "Box".into(),
            hole: false,
            attach_to: None,
            transform: None,
            group: None,
        }];
        let before: Vec<ObjectId> = s.document().objects().iter().map(|o| o.id).collect();
        s.execute(Invocation {
            op_id: "object.add_shape".into(),
            target: a,
            selection: None,
            operands: Vec::new(),
            raw_params: BTreeMap::from([
                ("shapes".to_owned(), shapes_param(&shapes)),
                (
                    "transform".to_owned(),
                    IDENTITY.map(|v| v.to_string()).join(","),
                ),
                ("group".to_owned(), String::new()),
            ]),
        })
        .unwrap();
        let placed = s
            .document()
            .objects()
            .iter()
            .find(|o| !before.contains(&o.id))
            .unwrap_or_else(|| panic!("{unit:?}: the box wasn't placed"));
        let edge_mm = (placed.mesh().unwrap().bounding_box().unwrap().max[0]
            - placed.mesh().unwrap().bounding_box().unwrap().min[0])
            * mm;
        assert!(
            (edge_mm - 10.0).abs() < 1e-6,
            "{unit:?}: shape.box should measure 10 mm across, not {edge_mm} mm \
             (object.add_shape placed the mm-internal mesh unscaled)"
        );

        // A connector built in place across two targets: `joint.dowel`
        // (object.attach's `generated` field, as `connector.join` and
        // the parametric-shapes brief's "connectors become shape
        // groups" both build it).
        let (mut s, a, b) = two_scans(dir.path(), unit);
        let dowel_gen = mf_parts::find_generator("joint.dowel").unwrap();
        let record = mf_parts::new_record(dowel_gen, &BTreeMap::new()).unwrap();
        let generated = [GeneratedMember {
            generator: record,
            transform: IDENTITY,
        }];
        let before: Vec<ObjectId> = s.document().objects().iter().map(|o| o.id).collect();
        s.execute(Invocation {
            op_id: "object.attach".into(),
            target: a,
            selection: None,
            operands: vec![("side_b".to_owned(), b, None)],
            raw_params: BTreeMap::from([
                ("members".to_owned(), "[]".to_owned()),
                ("generated".to_owned(), generated_param(&generated)),
                ("gap".to_owned(), "0".to_owned()),
            ]),
        })
        .unwrap();
        let dowel = s
            .document()
            .objects()
            .iter()
            .find(|o| !before.contains(&o.id))
            .unwrap_or_else(|| panic!("{unit:?}: the loose dowel wasn't placed"));
        let bb = dowel.mesh().unwrap().bounding_box().unwrap();
        let dowel_len_mm = (bb.max[2] - bb.min[2]) * mm;
        // depth_on + depth_other + gap - length_allowance = 6+6+0-0.4.
        assert!(
            (dowel_len_mm - 11.6).abs() < 1e-6,
            "{unit:?}: joint.dowel's loose dowel should be 11.6 mm long, not \
             {dowel_len_mm} mm (object.attach placed the generated mm mesh unscaled)"
        );

        // A split connector: the same joint.dowel, placed across a real
        // cut by split.place_connectors (the fake `mm` param C12 drops).
        // A sphere of "radius" `edge`, raised so its centre sits at
        // world z = edge (world z range [0, 2*edge]), cut just above
        // its equator — the same recipe as split_workflow.rs's
        // `split_horizontally` (ratio 31.3/30), scaled to any unit.
        let edge = 40.0 / mm;
        // `mesh.split_plane`'s `plane_point` is declared `unit: mm`
        // (prepare converts it to document units), so the text given
        // here is millimetres; `split.place_connectors`' own
        // `plane_point` is `unit: document` (it reads the joint's own
        // recorded point back), so that call uses the doc-unit value
        // `mesh.split_plane`'s report already gives.
        let plane_z_mm = edge * mm * (31.3 / 30.0);
        let scan_path = dir
            .path()
            .join(format!("scan-{}.3mf", unit.symbol().replace('µ', "u")));
        write_model_path(
            &Model {
                metadata: Default::default(),
                unit,
                objects: vec![ModelObject {
                    metadata: Default::default(),
                    name: "scan".into(),
                    mesh: mf_fixtures::scaled(&uv_sphere(24, 48), edge),
                    transform: rotation_translation(2, 0.0, [0.0, 0.0, edge]),
                }],
            },
            &scan_path,
        )
        .unwrap();
        let mut s2 = Session::new();
        let (ids, warnings) = s2.import_model(read_model(&scan_path).unwrap());
        assert!(warnings.is_empty(), "{warnings:?}");
        let id = ids[0];
        let report = report_of(
            s2.execute(inv(
                "mesh.split_plane",
                id,
                &[
                    ("plane_point", &format!("0,0,{plane_z_mm}")),
                    ("plane_normal", "0,0,1"),
                ],
            ))
            .unwrap(),
        );
        let joint = report["joints"][0].clone();
        let bottom = created(&s2)[0];
        let top = id;
        let connectors = serde_json::json!([
            { "connector": "joint.dowel", "on": "a", "at": [0.0, 0.0], "params": {} },
        ]);
        let point = joint["point"].as_array().unwrap();
        let normal = joint["normal"].as_array().unwrap();
        let point_csv = point
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let normal_csv = normal
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut place = inv(
            "split.place_connectors",
            bottom,
            &[
                ("plane_point", &point_csv),
                ("plane_normal", &normal_csv),
                ("plane_u", "1,0,0"),
                ("gap", &joint["gap"].to_string()),
                ("connectors", &connectors.to_string()),
            ],
        );
        place.operands = vec![("side_b".into(), top, None)];
        s2.execute(place).unwrap();
        let split_dowel_id = created(&s2)[0];
        let split_dowel = s2.document().object(split_dowel_id).unwrap();
        let bb = split_dowel.mesh().unwrap().bounding_box().unwrap();
        let split_len_mm = (bb.max[2] - bb.min[2]) * mm;
        assert!(
            (split_len_mm - 11.6).abs() < 1e-6,
            "{unit:?}: split.place_connectors' loose dowel should be 11.6 mm long, \
             not {split_len_mm} mm"
        );
    }
}
