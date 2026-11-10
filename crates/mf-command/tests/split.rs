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
    session_in(dir, name, mesh, transform, Unit::Millimeter)
}

fn session_in(
    dir: &Path,
    name: &str,
    mesh: Mesh,
    transform: [f64; 12],
    unit: Unit,
) -> (Session, ObjectId) {
    let path = dir.join("doc.3mf");
    let model = Model {
        unit,
        objects: vec![ModelObject {
            metadata: Default::default(),
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

/// Review Focus 4: in a metre document the default 0.2 mm clearance is
/// 0.0002 document units, and that is what is recorded.
#[test]
fn default_clearance_is_0_2_mm_in_a_metre_document() {
    let dir = tempfile::tempdir().unwrap();
    // A 60 mm sphere in metres.
    let sphere = mf_fixtures::scaled(&uv_sphere(48, 96), 0.03);
    let (s, id) = session_in(
        dir.path(),
        "scan",
        sphere,
        mf_io::threemf::IDENTITY,
        Unit::Meter,
    );
    let prepared = s
        .prepare(split(
            id,
            &[("pegs", "dowels"), ("plane_point", "0,0,0.37")],
        ))
        .unwrap();
    let p = prepared.params();
    assert!((p.get_float("peg_clearance").unwrap() - 0.0002).abs() < 1e-15);
    assert!((p.get_float("length_allowance").unwrap() - 0.0004).abs() < 1e-15);
    assert!((p.get_float("hole_diameter").unwrap() - 0.005).abs() < 1e-15);
}

#[test]
fn pegs_replay_exact() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, id) = session_with(
        dir.path(),
        "scan",
        mf_fixtures::scaled(&uv_sphere(48, 96), 30.0),
        mf_io::threemf::IDENTITY,
    );
    s.execute(split(
        id,
        &[
            ("pegs", "dowels"),
            ("plane_point", "0,0,0.37"),
            ("hole_depth_above", "5"),
            ("hole_depth_below", "8"),
        ],
    ))
    .unwrap();
    assert_eq!(
        names(&s),
        [
            "scan (top)",
            "scan (bottom)",
            "scan dowel 1",
            "scan dowel 2"
        ]
    );
    let after = state(&s);
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
    assert_eq!(state(&report.session), after);
}

/// Several targets in one command: every object the plane crosses gets
/// "(top)" and "(bottom)" parts, one the plane misses is listed in the
/// report, one undo restores everything, and replay is Exact.
#[test]
fn split_several_targets_in_one_command() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc.3mf");
    let placed = |x: f64, z: f64| rotation_translation(2, 0.0, [x, 0.0, z]);
    let model = Model {
        unit: Unit::Millimeter,
        objects: [("a", 0.0, 0.0), ("b", 5.0, 0.0), ("high", 10.0, 20.0)]
            .into_iter()
            .map(|(name, x, z)| ModelObject {
                metadata: Default::default(),
                name: name.into(),
                mesh: uv_sphere(11, 24),
                transform: placed(x, z),
            })
            .collect(),
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, _) = s.import_model(read_model(&path).unwrap());
    let before = state(&s);
    let mut inv = split(ids[0], &[]);
    inv.operands = vec![
        ("target".into(), ids[1], None),
        ("target".into(), ids[2], None),
    ];
    let report = match s.execute(inv).unwrap() {
        mf_command::CommitOutcome::NewRevision { report } => report,
        other => panic!("{other:?}"),
    };
    let mut got = names(&s);
    got.sort();
    assert_eq!(
        got,
        ["a (bottom)", "a (top)", "b (bottom)", "b (top)", "high"]
    );
    assert_eq!(report["missed"], serde_json::json!(["high"]), "{report}");
    assert_eq!(report["objects"].as_array().unwrap().len(), 2);
    assert!(s.undo());
    assert_eq!(state(&s), before, "one undo restores every object");
    assert!(s.redo());
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    let replayed = replay(&log).unwrap();
    assert!(
        replayed.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        replayed.results
    );
    assert_eq!(state(&replayed.session), state(&s));
    // A plane that misses every target is refused.
    let mut miss = split(ids[0], &[("plane_point", "0,0,100")]);
    miss.operands = vec![("target".into(), ids[1], None)];
    let err = s.execute(miss).unwrap_err().to_string();
    assert!(err.contains("any of the selected objects"), "{err}");
}
