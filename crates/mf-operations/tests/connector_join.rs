#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `connector.join` (Phase 1 Task 28): fold a connector's parts into the
//! two objects it joins, in one command, and remove the connector
//! object.

use std::collections::BTreeMap;

use mf_document::ObjectId;
use mf_fixtures::{cube, translated};
use mf_geometry::{Bvh, inspect, minimum_distance};
use mf_io::threemf::IDENTITY;
use mf_operations::{
    DocContext, DocOutput, ObjectChange, OpError, OperandInput, RunContext, Target, find,
};
use mf_parts::Generator;
use mf_parts::connectors::clasp::SnapClasp;
use mf_spec::Params;

/// Two big, separate cubes standing in for the body (`catch`'s side)
/// and the lid (`hook`'s side) a `clasp.snap` connector joins. With the
/// connector's own defaults (arm_length 10, arm_thickness 1.6, lip_depth
/// 0.3, gap 0.4): `catch` sits at z in [0, 3.2] (the ridge height), and
/// `hook` at z in [3.6, 5.2] (gap clear above it) — a real z split with
/// a known gap, unlike `hinge.pin`'s leaves (interleaved along x at the
/// same y/z, which isn't usable as two separate "sides" this simply).
fn sides() -> (mf_geometry::Mesh, mf_geometry::Mesh) {
    let body = translated(&cube(40.0), [-5.0, -5.0, -40.0]); // top face at z = 0
    let lid = translated(&cube(40.0), [-5.0, -5.0, 5.2]); // bottom face at z = 5.2
    (body, lid)
}

/// `clasp.snap`'s own validated params, as the JSON object
/// `connector.join`'s `connector_params` records.
fn clasp_params_json(extra: &[(&str, &str)]) -> String {
    let spec = SnapClasp.spec();
    let mut raw: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in extra {
        raw.insert(k.to_string(), v.to_string());
    }
    if !raw.contains_key("gap") {
        raw.insert("gap".into(), "0.4".into());
    }
    spec.fill_defaults(&mut raw, &mf_parts::FitProfile::builtin())
        .unwrap();
    let params = spec.validate(&raw).unwrap();
    serde_json::to_string(&params).unwrap()
}

fn join(
    connector_params: &str,
    gap: &str,
    target_mesh: &mf_geometry::Mesh,
    a: &mf_geometry::Mesh,
    b: &mf_geometry::Mesh,
) -> Result<DocOutput, OpError> {
    let spec = mf_spec::builtin::get("connector.join").unwrap();
    let raw: BTreeMap<String, String> = [
        ("connector_id", "clasp.snap"),
        ("connector_params", connector_params),
        ("gap", gap),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let params: Params = spec.validate(&raw).unwrap();
    find("connector.join").unwrap().execute_doc(
        &[
            OperandInput {
                slot: "target",
                object: ObjectId(1),
                name: "connector",
                mesh: target_mesh,
                transform: IDENTITY,
                target: Target::WholeObject,
            },
            OperandInput {
                slot: "side_a",
                object: ObjectId(2),
                name: "body",
                mesh: a,
                transform: IDENTITY,
                target: Target::WholeObject,
            },
            OperandInput {
                slot: "side_b",
                object: ObjectId(3),
                name: "lid",
                mesh: b,
                transform: IDENTITY,
                target: Target::WholeObject,
            },
        ],
        &params,
        &DocContext::default(),
        &RunContext::none(),
    )
}

struct Out {
    side_a: mf_geometry::Mesh,
    side_b: mf_geometry::Mesh,
    removed_target: bool,
    created: Vec<(String, mf_geometry::Mesh)>,
}

fn parts(out: DocOutput) -> Out {
    let mut side_a = None;
    let mut side_b = None;
    let mut removed_target = false;
    let mut created = Vec::new();
    for c in out.changes {
        match c {
            ObjectChange::Geometry {
                operand: 1, mesh, ..
            } => side_a = Some(mesh),
            ObjectChange::Geometry {
                operand: 2, mesh, ..
            } => side_b = Some(mesh),
            ObjectChange::Remove { operand: 0 } => removed_target = true,
            ObjectChange::Create { name, mesh, .. } => created.push((name, mesh)),
            _ => {}
        }
    }
    Out {
        side_a: side_a.unwrap(),
        side_b: side_b.unwrap(),
        removed_target,
        created,
    }
}

/// `connector.join` unions every `Union` role (`clasp.snap`'s `catch`
/// and `hook`) into its own side, in one command, and removes the
/// connector (target) object.
#[test]
fn join_unions_each_leaf_into_its_side_in_one_command() {
    let (a, b) = sides();
    let empty = mf_geometry::Mesh::empty();
    let out = join(&clasp_params_json(&[]), "0.4", &empty, &a, &b).unwrap();
    let out = parts(out);
    assert!(out.removed_target, "{:?}", out.created);
    assert!(out.created.is_empty(), "no Separate parts in clasp.snap");
    // Each side grew: more faces than the plain cube alone.
    assert!(out.side_a.face_count() > a.face_count());
    assert!(out.side_b.face_count() > b.face_count());
    for m in [&out.side_a, &out.side_b] {
        let d = inspect(m);
        assert!(d.is_closed && d.is_manifold, "{d:?}");
    }
}

/// Afterwards, side_a and side_b don't intersect and their minimum
/// distance at the connector is at least the connector's own gap minus
/// the chord tolerance: measured exactly with the BVH minimum distance,
/// not assumed from the parameters.
#[test]
fn join_leaves_the_sides_clear_by_the_connectors_own_gap() {
    let (a, b) = sides();
    let empty = mf_geometry::Mesh::empty();
    let out = join(&clasp_params_json(&[("gap", "0.4")]), "0.4", &empty, &a, &b).unwrap();
    let out = parts(out);
    let ctx = RunContext::none();
    let a_bvh = Bvh::build(&out.side_a, &ctx).unwrap();
    let d = minimum_distance(
        &out.side_a,
        &a_bvh,
        &out.side_b,
        &glam::DMat4::IDENTITY,
        &ctx,
    )
    .unwrap()
    .unwrap();
    let chord_tolerance = 0.05;
    assert!(d >= 0.4 - chord_tolerance, "{d}");
}

/// A connector whose recorded gap is larger than the mating surfaces
/// can actually deliver (because the sides were moved closer than the
/// connector expects) is refused by name, not silently joined fused.
#[test]
fn fused_moving_parts_are_refused() {
    let (a, b) = sides();
    let empty = mf_geometry::Mesh::empty();
    // Claim a far larger gap than clasp.snap's own geometry (built with
    // gap 0.4) actually delivers between the two sides' bulk material:
    // the connector's own leaves are the only things near each other
    // here (both sides are plain cubes with the connector unioned in),
    // so a wildly inflated claimed gap can never be met.
    match join(&clasp_params_json(&[("gap", "0.4")]), "50", &empty, &a, &b) {
        Err(OpError::Failed { code, message }) => {
            assert_eq!(code, "moving_parts_fused");
            assert!(
                message.contains("gap") || message.contains("fused"),
                "{message}"
            );
        }
        other => panic!("expected a refusal, got {:?}", other.map(|o| o.report)),
    }
}

/// `connector.join` replays exact: re-running it from the recorded
/// `connector_id`/`connector_params`/`gap` (never reading a connector's
/// live state) reproduces the identical result.
#[test]
fn connector_join_replays_exact() {
    let (a, b) = sides();
    let empty = mf_geometry::Mesh::empty();
    let params_json = clasp_params_json(&[]);
    let out1 = parts(join(&params_json, "0.4", &empty, &a, &b).unwrap());
    let out2 = parts(join(&params_json, "0.4", &empty, &a, &b).unwrap());
    assert_eq!(out1.side_a.to_arrays(), out2.side_a.to_arrays());
    assert_eq!(out1.side_b.to_arrays(), out2.side_b.to_arrays());
}

/// "Save to library" on a connector object writes `PartManifest.kind:
/// Connector { connector, params }` with no mesh file, and dropping it
/// from the library regenerates the exact same parts (byte for byte)
/// through the connector, from the recorded params alone — not a
/// stored snapshot.
#[test]
fn saved_connector_regenerates_identically() {
    let spec = SnapClasp.spec();
    let mut raw: BTreeMap<String, String> = BTreeMap::new();
    spec.fill_defaults(&mut raw, &mf_parts::FitProfile::builtin())
        .unwrap();
    let params = spec.validate(&raw).unwrap();
    let ctx = RunContext::none();
    let original = SnapClasp
        .generate(&params, &mf_parts::GenContext::new(&ctx))
        .unwrap();
    let mut original_mesh_positions = Vec::new();
    for p in &original {
        original_mesh_positions.push(p.mesh.to_arrays());
    }

    let dir = tempfile::tempdir().unwrap();
    let manifest = mf_parts::PartManifest {
        id: "my-clasp".into(),
        name: "My clasp".into(),
        version: 1,
        licence: "CC0".into(),
        source: "test".into(),
        kind: mf_parts::PartKind::Connector {
            connector: "clasp.snap".into(),
            params: params.clone(),
        },
        attach: mf_parts::Attach {
            point: [0.0, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0],
            up: [1.0, 0.0, 0.0],
        },
        default_scale: 1.0,
        unit: mf_parts::Unit::Millimeter,
        mesh: String::new(),
        category: String::new(),
        description: String::new(),
    };
    mf_parts::save_connector_as_part(&manifest, dir.path()).unwrap();

    let (lib, warnings) = mf_parts::Library::load(Some(dir.path()), &ctx);
    assert!(warnings.is_empty(), "{warnings:?}");
    let saved = lib.get("my-clasp").unwrap();
    assert!(matches!(
        saved.manifest.kind,
        mf_parts::PartKind::Connector { .. }
    ));
    let regenerated = saved.mesh(&ctx).unwrap();

    // The regenerated (merged) mesh matches a fresh merge of the
    // original parts exactly.
    let mut merged_positions = Vec::new();
    let mut merged_triangles = Vec::new();
    for (p, t) in &original_mesh_positions {
        let base = merged_positions.len() as u32;
        merged_positions.extend(p.clone());
        merged_triangles.extend(t.iter().map(|tri: &[u32; 3]| tri.map(|i| i + base)));
    }
    let expected = mf_geometry::Mesh::new(merged_positions, merged_triangles).unwrap();
    assert_eq!(regenerated.to_arrays(), expected.to_arrays());

    // Regenerating again (as a second "drop from the library" would)
    // gives byte-identical geometry too.
    let regenerated_again = saved.mesh(&ctx).unwrap();
    assert_eq!(regenerated.to_arrays(), regenerated_again.to_arrays());
}

/// The one generic apply (parts-architecture-review.md): `object.attach`
/// building the connector in place (`generated`, no member objects) does
/// exactly what the legacy `connector.join` does, byte for byte, since
/// `connector.join` is now that same command's body.
#[test]
fn attach_with_generated_parts_is_connector_join() {
    let (a, b) = sides();
    let empty = mf_geometry::Mesh::empty();
    let params_json = clasp_params_json(&[]);
    let joined = parts(join(&params_json, "0.4", &empty, &a, &b).unwrap());
    let record = mf_parts::GeneratorRecord {
        id: "clasp.snap".into(),
        version: 1,
        params: Params(serde_json::from_str(&params_json).unwrap()),
        part: None,
    };
    let spec = mf_spec::builtin::get("object.attach").unwrap();
    let raw: BTreeMap<String, String> = BTreeMap::from([
        (
            "generated".to_owned(),
            mf_operations::shapes::generated_param(&[mf_operations::shapes::GeneratedMember {
                generator: record,
                transform: IDENTITY,
            }]),
        ),
        ("gap".to_owned(), "0.4".to_owned()),
    ]);
    let out = find("object.attach")
        .unwrap()
        .execute_doc(
            &[
                OperandInput {
                    slot: "target",
                    object: ObjectId(2),
                    name: "body",
                    mesh: &a,
                    transform: IDENTITY,
                    target: Target::WholeObject,
                },
                OperandInput {
                    slot: "side_b",
                    object: ObjectId(3),
                    name: "lid",
                    mesh: &b,
                    transform: IDENTITY,
                    target: Target::WholeObject,
                },
            ],
            &spec.validate(&raw).unwrap(),
            &DocContext::default(),
            &RunContext::none(),
        )
        .unwrap();
    let mut got = [None, None];
    for c in out.changes {
        if let ObjectChange::Geometry { operand, mesh, .. } = c {
            got[operand] = Some(mesh);
        }
    }
    assert_eq!(
        got[0].as_ref().unwrap().to_arrays(),
        joined.side_a.to_arrays()
    );
    assert_eq!(
        got[1].as_ref().unwrap().to_arrays(),
        joined.side_b.to_arrays()
    );
    assert!(out.report["measured_gap"].as_f64().unwrap() >= 0.4 * 0.9);
}
