#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Phase 1 Task 3 (MF-P1-CMD-001, MF-P1-SEL-001, MF-OBJ-001): operands and
//! object changes through the command layer, and the object operations
//! `object.set_transform`, `object.drop_to_plate` and `object.delete`.
//!
//! `test.consume_tool` (`test-kinds` feature) stands in for a boolean: it
//! removes its `tool` operand and creates a new object from both
//! operands' faces, so undo and replay of a multi-object command can be
//! checked before the real boolean exists (Review Focus 5).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use mf_command::{
    CommitOutcome, Geometry, Invocation, LOG_FORMAT, ObjectId, Session, Verdict, replay,
};
use mf_fixtures::{cube, rotation_translation, tetrahedron, translated, uv_sphere};
use mf_geometry::{FaceId, Mesh};
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};
use mf_selection::{Selection, SelectionKind, TranslateError, Translation};

/// The objects written to `dir/doc.3mf` and imported into a fresh session
/// (so `save_log` has a source and replay can re-import it).
fn session_with(dir: &Path, objects: Vec<(&str, Mesh, [f64; 12])>) -> (Session, Vec<ObjectId>) {
    let path = dir.join("doc.3mf");
    let model = Model {
        unit: Unit::Millimeter,
        objects: objects
            .into_iter()
            .map(|(name, mesh, transform)| ModelObject {
                name: name.into(),
                mesh,
                transform,
            })
            .collect(),
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, warnings) = s.import_model(read_model(&path).unwrap());
    assert!(warnings.is_empty(), "{warnings:?}");
    (s, ids)
}

fn csv(m: &[f64; 12]) -> String {
    m.iter().map(f64::to_string).collect::<Vec<_>>().join(",")
}

fn set_transform(target: ObjectId, m: &[f64; 12]) -> Invocation {
    Invocation {
        op_id: "object.set_transform".into(),
        target,
        selection: None,
        operands: Vec::new(),
        raw_params: BTreeMap::from([("transform".to_string(), csv(m))]),
    }
}

fn plain(op: &str, target: ObjectId) -> Invocation {
    Invocation {
        op_id: op.into(),
        target,
        selection: None,
        operands: Vec::new(),
        raw_params: BTreeMap::new(),
    }
}

fn consume_tool(target: ObjectId, tool: ObjectId) -> Invocation {
    Invocation {
        op_id: "test.consume_tool".into(),
        target,
        selection: None,
        operands: vec![("tool".into(), tool, None)],
        raw_params: BTreeMap::new(),
    }
}

fn bits(m: &[f64; 12]) -> [u64; 12] {
    m.map(f64::to_bits)
}

fn transform_of(s: &Session, id: ObjectId) -> [f64; 12] {
    s.document().object(id).unwrap().transform
}

/// World-space z of every live vertex of `id` (3MF row-vector convention:
/// `z' = m02 x + m12 y + m22 z + m32`).
fn world_min_z(s: &Session, id: ObjectId) -> f64 {
    let o = s.document().object(id).unwrap();
    let t = o.transform;
    o.mesh()
        .unwrap()
        .vertices()
        .map(|(_, p)| t[2] * p[0] + t[5] * p[1] + t[8] * p[2] + t[11])
        .fold(f64::INFINITY, f64::min)
}

fn save_and_replay(s: &Session, dir: &Path) -> mf_command::ReplayReport {
    let log = dir.join("session.mfx.log");
    s.save_log(&log).unwrap();
    let manifest = std::fs::read_to_string(log.join("manifest.json")).unwrap();
    assert!(manifest.contains(LOG_FORMAT), "{manifest}");
    replay(&log).unwrap()
}

#[test]
fn log_format_is_version_2() {
    assert_eq!(LOG_FORMAT, "mfx-log/2");
}

#[test]
fn set_transform_is_one_undoable_command_and_replays_exact() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = session_with(dir.path(), vec![("part", cube(1.0), IDENTITY)]);
    let id = ids[0];
    let before = s.current();
    // Awkward floats (from trigonometry), so exactness is meaningful.
    let m = rotation_translation(2, 37.5, [1.25, -3.0, 0.1]);
    let outcome = s.execute(set_transform(id, &m)).unwrap();
    assert!(matches!(outcome, CommitOutcome::NewRevision { .. }));
    assert_eq!(
        s.node(s.current()).unwrap().parent,
        Some(before),
        "one command, one revision"
    );
    assert_eq!(bits(&transform_of(&s, id)), bits(&m));

    let record = s.node(s.current()).unwrap().record.clone().unwrap();
    assert_eq!(record.op_id, "object.set_transform");
    assert_eq!(record.operands.len(), 1, "{:?}", record.operands);
    assert_eq!(record.operands[0].object, id);
    assert_eq!(bits(&record.operands[0].input_transform), bits(&IDENTITY));
    assert_eq!(record.outputs.len(), 1, "{:?}", record.outputs);
    let out = &record.outputs[0];
    assert_eq!(out.object, id);
    assert!(!out.created && !out.removed);
    assert_eq!(bits(&out.transform), bits(&m));
    assert_eq!(out.hash, record.input_hash, "geometry is unchanged");

    assert!(s.undo());
    assert_eq!(bits(&transform_of(&s, id)), bits(&IDENTITY));
    assert!(s.redo());
    assert_eq!(bits(&transform_of(&s, id)), bits(&m));

    let report = save_and_replay(&s, dir.path());
    assert_eq!(report.results.len(), 1, "{:?}", report.results);
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
    assert_eq!(bits(&transform_of(&report.session, id)), bits(&m));
}

/// A transform that differs by one bit from the recorded one is a
/// `Mismatch`, never `Equivalent` (D7: replay runs no trigonometry).
#[test]
fn a_transform_one_bit_off_is_a_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = session_with(dir.path(), vec![("part", cube(1.0), IDENTITY)]);
    let m = rotation_translation(0, 30.0, [0.0, 0.0, 2.0]);
    s.execute(set_transform(ids[0], &m)).unwrap();
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    let path = log.join("commands.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let cmd = lines.iter_mut().find(|e| e["kind"] == "command").unwrap();
    let recorded = cmd["outputs"][0]["transform"][11].as_f64().unwrap();
    cmd["outputs"][0]["transform"][11] = serde_json::json!(f64::from_bits(recorded.to_bits() + 1));
    let edited: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    std::fs::write(&path, edited.join("\n") + "\n").unwrap();
    let report = replay(&log).unwrap();
    assert!(
        matches!(report.results[0].1, Verdict::Mismatch { .. }),
        "{:?}",
        report.results
    );
}

#[test]
fn drop_to_plate_puts_world_min_z_at_zero() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = session_with(
        dir.path(),
        vec![(
            "part",
            translated(&cube(2.0), [0.5, -0.25, 3.0]),
            rotation_translation(0, 30.0, [4.0, 5.0, 7.5]),
        )],
    );
    let id = ids[0];
    let before = transform_of(&s, id);
    let geometry_before = s.document().object(id).unwrap().geometry.clone();
    let outcome = s.execute(plain("object.drop_to_plate", id)).unwrap();
    assert!(matches!(outcome, CommitOutcome::NewRevision { .. }));
    let after = transform_of(&s, id);
    assert!(
        world_min_z(&s, id).abs() <= 1e-12,
        "{}",
        world_min_z(&s, id)
    );
    // Only the z translation moves.
    assert_eq!(bits(&before)[..11], bits(&after)[..11]);
    assert_ne!(before[11].to_bits(), after[11].to_bits());
    assert!(Geometry::ptr_eq(
        &geometry_before,
        &s.document().object(id).unwrap().geometry
    ));
    // Dropping again changes nothing.
    let again = s.execute(plain("object.drop_to_plate", id)).unwrap();
    assert!(matches!(again, CommitOutcome::NoChange { .. }), "{again:?}");
    let report = save_and_replay(&s, dir.path());
    assert!(report.all_verified(), "{:?}", report.results);
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
}

#[test]
fn drop_to_plate_is_cancellable() {
    let dir = tempfile::tempdir().unwrap();
    let (s, ids) = session_with(dir.path(), vec![("part", uv_sphere(24, 32), IDENTITY)]);
    let prepared = s.prepare(plain("object.drop_to_plate", ids[0])).unwrap();
    let ctx = mf_runtime::RunContext::none();
    ctx.cancel.cancel();
    assert!(matches!(
        prepared.run(&ctx),
        Err(mf_command::CommandError::Cancelled)
    ));
}

#[test]
fn delete_object_is_undoable_and_keeps_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = session_with(
        dir.path(),
        vec![
            ("body", cube(1.0), IDENTITY),
            (
                "head",
                tetrahedron(),
                rotation_translation(1, 15.0, [0.0, 0.0, 2.0]),
            ),
        ],
    );
    let (body, head) = (ids[0], ids[1]);
    let head_geometry = s.document().object(head).unwrap().geometry.clone();
    let head_transform = transform_of(&s, head);
    let sel = Selection {
        object: head,
        revision: s.current(),
        kind: SelectionKind::Faces,
        ids: vec![0, 1],
    };
    s.execute(plain("object.delete", head)).unwrap();
    assert!(s.document().object(head).is_none());
    assert!(s.document().object(body).is_some());
    let record = s.node(s.current()).unwrap().record.clone().unwrap();
    assert_eq!(record.outputs.len(), 1);
    assert!(record.outputs[0].removed && record.outputs[0].object == head);
    assert_eq!(
        s.translate(&sel, s.current()),
        Translation::Failed(TranslateError::ObjectMissing)
    );

    assert!(s.undo());
    let restored = s.document().object(head).unwrap();
    assert_eq!(restored.id, head);
    assert_eq!(restored.name, "head");
    assert!(Geometry::ptr_eq(&restored.geometry, &head_geometry));
    assert_eq!(bits(&restored.transform), bits(&head_transform));
    assert_eq!(
        s.document()
            .objects()
            .iter()
            .map(|o| o.id)
            .collect::<Vec<_>>(),
        vec![body, head],
        "restored in its place"
    );
    assert!(s.redo());
    assert!(s.document().object(head).is_none());

    let report = save_and_replay(&s, dir.path());
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
    assert!(report.session.document().object(head).is_none());
}

#[test]
fn transform_change_keeps_geometry_shared() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = session_with(dir.path(), vec![("part", uv_sphere(12, 16), IDENTITY)]);
    let id = ids[0];
    let before = s.document().object(id).unwrap().geometry.clone();
    s.execute(set_transform(
        id,
        &rotation_translation(1, 90.0, [1.0, 2.0, 3.0]),
    ))
    .unwrap();
    let after = &s.document().object(id).unwrap().geometry;
    assert!(Geometry::ptr_eq(&before, after));
    // A transform-only revision records no geometry provenance.
    assert!(!s.node(s.current()).unwrap().provenance.contains_key(&id));
}

/// Review Focus 5: undoing a command that consumed its tool restores the
/// tool with the same `ObjectId`; a selection on the tool fails with
/// `ObjectMissing` after the command; replay reproduces both objects with
/// Exact verdicts; a 3MF export after the undo contains both.
#[test]
fn undo_boolean_restores_the_tool_with_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = session_with(
        dir.path(),
        vec![
            ("target", cube(1.0), IDENTITY),
            (
                "tool",
                tetrahedron(),
                rotation_translation(2, 45.0, [0.5, 0.5, 0.5]),
            ),
        ],
    );
    let (target, tool) = (ids[0], ids[1]);
    let tool_geometry = s.document().object(tool).unwrap().geometry.clone();
    let tool_sel = Selection {
        object: tool,
        revision: s.current(),
        kind: SelectionKind::Faces,
        ids: vec![0],
    };
    s.execute(consume_tool(target, tool)).unwrap();
    let after = s.current();
    assert!(s.document().object(tool).is_none(), "the tool is consumed");
    let record = s.node(after).unwrap().record.clone().unwrap();
    assert_eq!(record.operands.len(), 2, "{:?}", record.operands);
    assert_eq!(record.operands[1].name, "tool");
    assert_eq!(record.operands[1].object, tool);
    let created: Vec<_> = record.outputs.iter().filter(|o| o.created).collect();
    assert_eq!(created.len(), 1, "{:?}", record.outputs);
    let result = created[0].object;
    assert!(result != target && result != tool);
    assert!(s.document().object(result).is_some());
    assert!(record.outputs.iter().any(|o| o.object == tool && o.removed));
    assert_eq!(
        s.translate(&tool_sel, after),
        Translation::Failed(TranslateError::ObjectMissing)
    );

    assert!(s.undo());
    let restored = s.document().object(tool).unwrap();
    assert_eq!(restored.id, tool);
    assert!(Geometry::ptr_eq(&restored.geometry, &tool_geometry));
    assert!(s.document().object(result).is_none());
    // A 3MF export after the undo contains both objects.
    let model = Model {
        unit: s.document().units(),
        objects: s
            .document()
            .objects()
            .iter()
            .map(|o| ModelObject {
                name: o.name.clone(),
                mesh: o.mesh().unwrap().clone(),
                transform: o.transform,
            })
            .collect(),
    };
    let out = dir.path().join("after-undo.3mf");
    write_model_path(&model, &out).unwrap();
    let back = read_model(&out).unwrap();
    let names: Vec<&str> = back.objects.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, vec!["target", "tool"]);

    // Replay of the command (redo it first: the log is root → current).
    assert!(s.redo());
    let report = save_and_replay(&s, dir.path());
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
    let doc = report.session.document();
    assert!(doc.object(tool).is_none());
    assert!(doc.object(target).is_some());
    assert_eq!(
        doc.object(result).unwrap().geometry.content_hash(),
        s.document().object(result).unwrap().geometry.content_hash()
    );
}

/// An operand that the spec doesn't name, or a missing required one, is
/// refused before anything runs.
#[test]
fn operands_are_validated_against_the_spec() {
    let dir = tempfile::tempdir().unwrap();
    let (s, ids) = session_with(
        dir.path(),
        vec![("a", cube(1.0), IDENTITY), ("b", tetrahedron(), IDENTITY)],
    );
    let mut missing = consume_tool(ids[0], ids[1]);
    missing.operands.clear();
    assert!(s.prepare(missing).is_err());
    let mut unknown = plain("object.drop_to_plate", ids[0]);
    unknown.operands = vec![("tool".into(), ids[1], None)];
    assert!(s.prepare(unknown).is_err());
    // The target can't also be an operand.
    assert!(s.prepare(consume_tool(ids[0], ids[0])).is_err());
    assert!(s.prepare(consume_tool(ids[0], ObjectId(999))).is_err());
}

/// `mfx-log/1` logs still replay: the committed fixture, and a log written
/// now and rewritten to the old format (no `operands`, no `outputs`).
#[test]
fn mfx_log_1_still_replays() {
    let committed =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/equivalent-replay/session.mfx.log");
    let manifest = std::fs::read_to_string(committed.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"mfx-log/1\""), "{manifest}");
    let report = replay(&committed).unwrap();
    assert_eq!(report.results.len(), 2);
    assert!(report.all_verified(), "{:?}", report.results);

    let dir = tempfile::tempdir().unwrap();
    let mut holed = cube(1.0).edit();
    holed.remove_face(FaceId(0)).unwrap();
    let (mut s, ids) = session_with(
        dir.path(),
        vec![("part", holed.finish().unwrap().0, IDENTITY)],
    );
    s.execute(plain("mesh.fill_holes", ids[0])).unwrap();
    let log = dir.path().join("v1.mfx.log");
    s.save_log(&log).unwrap();
    let manifest_path = log.join("manifest.json");
    let manifest = std::fs::read_to_string(&manifest_path)
        .unwrap()
        .replace("mfx-log/2", "mfx-log/1");
    std::fs::write(&manifest_path, manifest).unwrap();
    let path = log.join("commands.jsonl");
    let lines: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|l| {
            let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
            if let Some(o) = v.as_object_mut() {
                o.remove("operands");
                o.remove("outputs");
            }
            v.to_string()
        })
        .collect();
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    let report = replay(&log).unwrap();
    assert_eq!(report.results.len(), 1);
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
}

/// Transforms edited by commands round-trip through a 3MF bit for bit.
#[test]
fn edited_transforms_round_trip_through_3mf() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = session_with(
        dir.path(),
        vec![("a", cube(1.0), IDENTITY), ("b", tetrahedron(), IDENTITY)],
    );
    s.execute(set_transform(
        ids[0],
        &rotation_translation(0, 30.0, [0.1, 0.2, 0.3]),
    ))
    .unwrap();
    s.execute(set_transform(
        ids[1],
        &rotation_translation(1, 123.456, [-7.0, 1e-7, 5.5]),
    ))
    .unwrap();
    s.execute(plain("object.drop_to_plate", ids[1])).unwrap();
    let model = Model {
        unit: s.document().units(),
        objects: s
            .document()
            .objects()
            .iter()
            .map(|o| ModelObject {
                name: o.name.clone(),
                mesh: o.mesh().unwrap().clone(),
                transform: o.transform,
            })
            .collect(),
    };
    let out = dir.path().join("edited.3mf");
    write_model_path(&model, &out).unwrap();
    let back = read_model(&out).unwrap();
    assert_eq!(back.objects.len(), 2);
    for (o, b) in s.document().objects().iter().zip(&back.objects) {
        assert_eq!(bits(&o.transform), bits(&b.transform), "{}", o.name);
    }
}

/// A tiny deterministic generator for the property test (no new
/// dependency).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn unit(&mut self) -> f64 {
        self.next() as f64 / (1u64 << 31) as f64
    }
}

fn random_mesh(r: &mut Lcg) -> Mesh {
    match r.below(4) {
        0 => cube(0.5 + r.unit()),
        1 => tetrahedron(),
        2 => uv_sphere(4 + r.below(6) as u32, 6 + r.below(8) as u32),
        _ => {
            // A cube with a face removed, so mesh.fill_holes has work.
            let mut e = cube(1.0).edit();
            e.remove_face(FaceId(r.below(12) as u32)).unwrap();
            e.finish().unwrap().0
        }
    }
}

/// MF-P1-SEL-001: every output face of every changed or created object is
/// accounted for exactly once (kept, or created), every input face of a
/// changed object is kept xor removed, and every face of a consumed
/// operand is a source of a created face (or its object was removed by a
/// command that creates nothing from it, like `object.delete`).
#[test]
fn operand_provenance_is_total() {
    let mut r = Lcg(0x5eed_0bec7);
    let mut committed: BTreeMap<String, usize> = BTreeMap::new();
    for case in 0..40 {
        let dir = tempfile::tempdir().unwrap();
        let n = 2 + r.below(2) as usize;
        let objects: Vec<(String, Mesh, [f64; 12])> = (0..n)
            .map(|k| {
                (
                    format!("o{k}"),
                    random_mesh(&mut r),
                    rotation_translation(
                        r.below(3) as usize,
                        r.unit() * 360.0,
                        [r.unit(), r.unit(), r.unit() * 4.0],
                    ),
                )
            })
            .collect();
        let (mut s, ids) = session_with(
            dir.path(),
            objects
                .iter()
                .map(|(n, m, t)| (n.as_str(), m.clone(), *t))
                .collect(),
        );
        for step in 0..4 {
            let live: Vec<ObjectId> = s.document().objects().iter().map(|o| o.id).collect();
            if live.is_empty() {
                break;
            }
            let target = live[r.below(live.len() as u64) as usize];
            let inv = match r.below(5) {
                0 => set_transform(
                    target,
                    &rotation_translation(0, r.unit() * 90.0, [r.unit(), 0.0, 1.0]),
                ),
                1 => plain("object.drop_to_plate", target),
                2 => plain("object.delete", target),
                3 => plain("mesh.fill_holes", target),
                _ => match live.iter().find(|&&o| o != target) {
                    Some(&tool) => consume_tool(target, tool),
                    None => plain("object.drop_to_plate", target),
                },
            };
            let op = inv.op_id.clone();
            let base = s.current();
            let outcome = s.execute(inv).unwrap();
            if !matches!(outcome, CommitOutcome::NewRevision { .. }) {
                continue;
            }
            *committed.entry(op.clone()).or_default() += 1;
            let ctx = format!("case {case} step {step} {op}");
            let node = s.node(s.current()).unwrap();
            let record = node.record.clone().unwrap();
            let before = &s.node(base).unwrap().document;
            let after = &node.document;
            // Faces named as sources of created faces, per operand index.
            let mut sourced: BTreeSet<(u32, FaceId)> = BTreeSet::new();
            for out in &record.outputs {
                if out.removed {
                    assert!(after.object(out.object).is_none(), "{ctx}");
                    continue;
                }
                let new = after.object(out.object).unwrap();
                let new_mesh = new.mesh().unwrap();
                assert_eq!(
                    new.geometry.content_hash().to_hex(),
                    out.hash,
                    "{ctx}: output hash"
                );
                assert_eq!(bits(&new.transform), bits(&out.transform), "{ctx}");
                let Some(prov) = node.provenance.get(&out.object) else {
                    // Transform-only: the geometry is the same revision.
                    assert!(!out.created, "{ctx}: a created object has provenance");
                    assert!(
                        Geometry::ptr_eq(
                            &before.object(out.object).unwrap().geometry,
                            &new.geometry
                        ),
                        "{ctx}"
                    );
                    continue;
                };
                let created: BTreeSet<FaceId> =
                    prov.created_faces.iter().map(|(f, _)| *f).collect();
                assert_eq!(created.len(), prov.created_faces.len(), "{ctx}: duplicates");
                for (_, src) in &prov.created_faces {
                    sourced.extend(src.operands.iter().copied());
                }
                let removed: BTreeSet<FaceId> = prov.removed_faces.iter().copied().collect();
                let old = before.object(out.object).and_then(|o| o.mesh());
                if out.created {
                    assert!(old.is_none(), "{ctx}");
                    assert!(
                        new_mesh.faces().all(|(f, _)| created.contains(&f)),
                        "{ctx}: every face of a created object is Created"
                    );
                    assert_eq!(created.len(), new_mesh.face_count(), "{ctx}");
                    for (_, src) in &prov.created_faces {
                        assert!(src.faces.is_empty() && !src.operands.is_empty(), "{ctx}");
                    }
                    continue;
                }
                let old = old.unwrap();
                for (f, tri) in new_mesh.faces() {
                    assert!(
                        (old.face(f) == Some(tri)) ^ created.contains(&f),
                        "{ctx}: face {f:?} kept xor created"
                    );
                }
                for (f, _) in old.faces() {
                    assert!(
                        new_mesh.face(f).is_some() ^ removed.contains(&f),
                        "{ctx}: face {f:?} kept xor removed"
                    );
                }
            }
            // Every face of a consumed operand is a source (or removed).
            let spec = mf_spec::builtin::get(&record.op_id).unwrap();
            for (k, operand) in record.operands.iter().enumerate().skip(1) {
                let role = spec
                    .operands
                    .iter()
                    .find(|o| o.name == operand.name)
                    .unwrap()
                    .role;
                if role != mf_spec::OperandRole::Consumed {
                    continue;
                }
                let gone = record
                    .outputs
                    .iter()
                    .any(|o| o.object == operand.object && o.removed);
                let creates = record.outputs.iter().any(|o| o.created);
                let mesh = before.object(operand.object).unwrap().mesh().unwrap();
                for (f, _) in mesh.faces() {
                    assert!(
                        sourced.contains(&(k as u32, f)) || (gone && !creates),
                        "{ctx}: consumed face {f:?} of operand {k} is neither a source nor removed"
                    );
                }
            }
        }
        let _ = ids;
    }
    // Every kind of change was exercised.
    for op in [
        "object.set_transform",
        "object.drop_to_plate",
        "object.delete",
        "mesh.fill_holes",
        "test.consume_tool",
    ] {
        assert!(
            committed.get(op).is_some_and(|&n| n > 0),
            "{op}: {committed:?}"
        );
    }
}
