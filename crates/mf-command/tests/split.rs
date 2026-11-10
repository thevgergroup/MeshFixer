#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `mesh.split_plane` through the command layer (Phase 1 Task 12): one
//! undoable command that changes, renames and creates objects; Exact
//! replay with the created part's recorded id; and cancellation at every
//! checkpoint (Review Focus 3) changing nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use mf_command::{CommandError, Invocation, ObjectId, Session, Verdict, replay};
use mf_fixtures::{rotation_translation, uv_sphere};
use mf_geometry::Mesh;
use mf_io::threemf::{Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};
use mf_runtime::RunContext;

fn session_with(dir: &Path, name: &str, mesh: Mesh, transform: [f64; 12]) -> (Session, ObjectId) {
    let path = dir.join("doc.3mf");
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![ModelObject {
            name: name.into(),
            mesh,
            transform,
        }],
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, warnings) = s.import_model(read_model(&path).unwrap());
    assert!(warnings.is_empty(), "{warnings:?}");
    (s, ids[0])
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

fn names(s: &Session) -> Vec<String> {
    s.document()
        .objects()
        .iter()
        .map(|o| o.name.clone())
        .collect()
}

/// Object ids, names, transforms and geometry hashes: the document's
/// whole state.
fn state(s: &Session) -> Vec<(ObjectId, String, [u64; 12], String)> {
    s.document()
        .objects()
        .iter()
        .map(|o| {
            (
                o.id,
                o.name.clone(),
                o.transform.map(f64::to_bits),
                o.mesh().unwrap().content_hash().to_hex(),
            )
        })
        .collect()
}

#[test]
fn split_replays_exact() {
    let dir = tempfile::tempdir().unwrap();
    let placement = rotation_translation(0, 30.0, [2.0, 1.0, 3.0]);
    let (mut s, id) = session_with(dir.path(), "scan", uv_sphere(24, 48), placement);
    let before = state(&s);
    s.execute(split(
        id,
        &[
            ("plane_point", "2.1,1.05,3.2"),
            ("plane_normal", "0.3,0.2,1"),
            ("cut_gap", "0.25"),
        ],
    ))
    .unwrap();
    assert_eq!(names(&s), ["scan (top)", "scan (bottom)"]);
    let created = s.document().objects()[1].id;
    assert_ne!(created, id);
    assert_eq!(s.document().objects()[1].transform, placement);
    let after = state(&s);

    // One command, one undo.
    assert!(s.undo());
    assert_eq!(state(&s), before);
    assert!(s.redo());
    assert_eq!(state(&s), after);

    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert_eq!(report.results.len(), 1);
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
    assert_eq!(state(&report.session), after);
}

/// Cancelled at each of its checkpoints in turn, a split returns
/// `Cancelled` and the document is unchanged, with no new revision; the
/// cancellations land in every stage.
#[test]
fn cancel_at_every_stage_leaves_document_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let (s, id) = session_with(
        dir.path(),
        "scan",
        uv_sphere(200, 400),
        mf_io::threemf::IDENTITY,
    );
    let params = [("plane_point", "0,0,0.1"), ("plane_normal", "0.1,0,1")];
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    // A first run caches the target's topology and hashes, as the app's
    // analysis would have; then count the checkpoints of a warm run (one
    // thread: deterministic).
    pool.install(|| {
        s.prepare(split(id, &params))
            .unwrap()
            .run(&RunContext::none())
    })
    .unwrap();
    let (ctx, probe) = RunContext::instrumented();
    pool.install(|| s.prepare(split(id, &params)).unwrap().run(&ctx))
        .unwrap();
    let total = probe.checkpoints();
    assert!(total > 20, "{total} checkpoints");

    let before = state(&s);
    let revision = s.current();
    let mut stages = BTreeSet::new();
    assert!(total < 2000, "{total} checkpoints: too many to try each");
    for n in 0..total {
        let ctx = RunContext::cancelled_after(n);
        let result = pool.install(|| s.prepare(split(id, &params)).unwrap().run(&ctx));
        assert!(
            matches!(result, Err(CommandError::Cancelled)),
            "cancelled after {n} of {total} checkpoints: finished anyway ({})",
            result.is_ok()
        );
        stages.insert(ctx.progress.snapshot().0);
        assert_eq!(state(&s), before);
        assert_eq!(s.current(), revision);
    }
    for stage in mf_operations::SPLIT_STAGES {
        assert!(
            stages.contains(stage),
            "never cancelled in {stage}: {stages:?}"
        );
    }
}
