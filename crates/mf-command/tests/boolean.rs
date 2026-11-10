#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `mesh.boolean` through the command layer (Phase 1 Task 8): a run
//! cancelled at any stage changes no document state (MF-CMD-006, Review
//! Focus 3), undo restores the consumed tool, and replay is Exact at any
//! thread count.

use std::collections::BTreeMap;
use std::path::Path;

use mf_command::{Invocation, ObjectId, Session, Verdict, replay};
use mf_fixtures::{cube, scaled, uv_sphere};
use mf_geometry::Mesh;
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};
use mf_runtime::{JobPool, RunContext};

fn session_with(dir: &Path, objects: Vec<(&str, Mesh, [f64; 12])>) -> (Session, Vec<ObjectId>) {
    let path = dir.join("doc.3mf");
    let model = Model {
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

fn boolean(target: ObjectId, tool: ObjectId, op: &str) -> Invocation {
    Invocation {
        op_id: "mesh.boolean".into(),
        target,
        selection: None,
        operands: vec![("tool".into(), tool, None)],
        raw_params: BTreeMap::from([("operation".to_string(), op.to_string())]),
    }
}

fn scene(dir: &Path) -> (Session, ObjectId, ObjectId) {
    let mut at = IDENTITY;
    at[9..].copy_from_slice(&[9.5, -0.5, -0.5]);
    let (s, ids) = session_with(
        dir,
        vec![
            ("scan", scaled(&uv_sphere(32, 64), 10.0), IDENTITY),
            ("part", cube(1.0), at),
        ],
    );
    (s, ids[0], ids[1])
}

#[test]
fn boolean_cancel_at_every_stage_leaves_document_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, target, tool) = scene(dir.path());
    let revision = s.revision();
    let hashes: Vec<_> = s
        .document()
        .objects()
        .iter()
        .map(|o| {
            (
                o.id,
                o.geometry.content_hash(),
                o.transform.map(f64::to_bits),
            )
        })
        .collect();
    for stage in mf_boolean::STAGES.into_iter().chain(["boolean: manifold"]) {
        let prepared = s.prepare(boolean(target, tool, "union")).unwrap();
        let r = prepared.run(&RunContext::cancelling_at(stage));
        assert!(
            matches!(r, Err(mf_command::CommandError::Cancelled)),
            "{stage}"
        );
        assert_eq!(s.revision(), revision, "{stage}");
        let now: Vec<_> = s
            .document()
            .objects()
            .iter()
            .map(|o| {
                (
                    o.id,
                    o.geometry.content_hash(),
                    o.transform.map(f64::to_bits),
                )
            })
            .collect();
        assert_eq!(now, hashes, "{stage}");
    }
    // Uncancelled, it commits: and undo restores the tool with its id.
    s.execute(boolean(target, tool, "union")).unwrap();
    assert!(s.document().object(tool).is_none());
    assert!(s.undo());
    assert!(s.document().object(tool).is_some());
}

#[test]
fn boolean_replays_exact_at_1_and_n_threads() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, target, tool) = scene(dir.path());
    s.execute(boolean(target, tool, "subtract")).unwrap();
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    for threads in [1, 4] {
        let (pool, _) = JobPool::new(Some(threads));
        let report = pool.install(|| replay(&log).unwrap());
        assert!(
            report.results.iter().all(|(_, v)| *v == Verdict::Exact),
            "{threads} threads: {:?}",
            report.results
        );
    }
}
