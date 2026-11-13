#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Command log save/load and verified replay (MF-CMD-004).

use std::collections::BTreeMap;
use std::path::Path;

use mf_command::{Invocation, LogError, Session, Verdict, replay};
use mf_fixtures::{cube, merge, tetrahedron, translated};
use mf_io::{Format, read_path, write_path};

fn setup(dir: &Path) -> Session {
    let src = dir.join("scan.stl");
    let mesh = merge(&[
        cube(1.0),
        translated(&tetrahedron(), [5.0, 0.0, 0.0]),
        translated(&cube(0.1), [9.0, 0.0, 0.0]),
    ]);
    write_path(
        &mesh,
        &src,
        Format::StlBinary,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();
    let mut s = Session::new();
    let id = s.import(read_path(&src).unwrap(), "scan".into());
    s.execute(Invocation {
        operands: Vec::new(),
        selection: None,
        op_id: "mesh.remove_small_components".into(),
        target: id,
        // Every piece but the largest under 5 triangles, however big the
        // largest is.
        raw_params: BTreeMap::from([
            ("max_triangles".into(), "5".into()),
            ("max_relative_size".into(), "100".into()),
        ]),
    })
    .unwrap();
    s
}

/// A record made by version 1 of remove_small_components (before
/// `max_relative_size` existed) replays exactly: it runs with no relative
/// limit, as it did then (MF-ISSUE-052).
#[test]
fn version_1_records_replay_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(dir.path());
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    let path = log.join("commands.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"op_version\":2"), "{text}");
    assert!(text.contains("\"max_relative_size\":100.0,"), "{text}");
    let v1 = text
        .replace("\"op_version\":2", "\"op_version\":1")
        .replace("\"max_relative_size\":100.0,", "");
    std::fs::write(&path, v1).unwrap();
    let report = replay(&log).unwrap();
    assert!(
        matches!(report.results[0].1, Verdict::Exact),
        "{:?}",
        report.results
    );
}

/// A `fill_holes` record from version 1 (before fill styles) replays
/// exactly: it means `fill_style: flat`, which is what it did then, not the
/// new default (MF-OP-002).
#[test]
fn v1_records_replay_as_flat_exactly() {
    let dir = tempfile::tempdir().unwrap();
    // A round hole in a sphere: smooth and flat patches differ there.
    let base = mf_fixtures::uv_sphere(24, 32);
    let mut e = base.edit();
    for (id, t) in base.faces() {
        if t.iter().all(|&v| base.corner_position(v)[2] > 0.8) {
            e.remove_face(id).unwrap();
        }
    }
    let src = dir.path().join("sphere.stl");
    write_path(
        &e.finish().unwrap().0,
        &src,
        Format::StlBinary,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();
    let fill = |style: &str| {
        let mut s = Session::new();
        let id = s.import(read_path(&src).unwrap(), "sphere".into());
        s.execute(Invocation {
            operands: Vec::new(),
            selection: None,
            op_id: "mesh.fill_holes".into(),
            target: id,
            raw_params: BTreeMap::from([("fill_style".into(), style.into())]),
        })
        .unwrap();
        s
    };
    let hash = |s: &Session| s.document().objects()[0].geometry.content_hash();
    let s = fill("flat");
    assert_ne!(hash(&s), hash(&fill("smooth")), "the styles differ here");
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    let path = log.join("commands.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"op_version\":2"), "{text}");
    assert!(text.contains("\"fill_style\":\"flat\","), "{text}");
    let v1 = text
        .replace("\"op_version\":2", "\"op_version\":1")
        .replace("\"fill_style\":\"flat\",", "");
    std::fs::write(&path, v1).unwrap();
    let report = replay(&log).unwrap();
    assert!(
        matches!(report.results[0].1, Verdict::Exact),
        "{:?}",
        report.results
    );
    assert_eq!(
        hash(&report.session),
        hash(&s),
        "replayed as flat, not as the new default"
    );
}

#[test]
fn save_load_replay_exact() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(dir.path());
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    assert!(log.join("manifest.json").exists() && log.join("commands.jsonl").exists());
    let manifest = std::fs::read_to_string(log.join("manifest.json")).unwrap();
    assert!(
        manifest.contains("\"path\": \"../scan.stl\""),
        "portable relative path: {manifest}"
    );
    let report = replay(&log).unwrap();
    assert_eq!(report.results.len(), 1);
    assert!(
        matches!(report.results[0].1, Verdict::Exact),
        "{:?}",
        report.results
    );
    let original = s.document().objects()[0].geometry.content_hash();
    let replayed = report.session.document().objects()[0]
        .geometry
        .content_hash();
    assert_eq!(original, replayed);
}

#[test]
fn tampered_log_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(dir.path());
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    let path = log.join("commands.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"max_triangles\":5"));
    std::fs::write(
        &path,
        text.replace("\"max_triangles\":5", "\"max_triangles\":13"),
    )
    .unwrap();
    let report = replay(&log).unwrap();
    assert!(
        matches!(report.results[0].1, Verdict::Mismatch { .. }),
        "{:?}",
        report.results
    );
    assert!(!report.all_verified());
}

#[test]
fn replay_rejects_changed_source() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(dir.path());
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    write_path(
        &cube(3.0),
        &dir.path().join("scan.stl"),
        Format::StlBinary,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();
    match replay(&log) {
        Err(LogError::SourceChanged { path, .. }) => assert!(path.ends_with("scan.stl")),
        other => panic!("expected SourceChanged, got {:?}", other.map(|r| r.results)),
    }
}

#[test]
fn log_survives_moving_the_whole_folder() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(dir.path());
    s.save_log(&dir.path().join("session.mfx.log")).unwrap();
    let moved = tempfile::tempdir().unwrap();
    std::fs::copy(dir.path().join("scan.stl"), moved.path().join("scan.stl")).unwrap();
    let log = moved.path().join("session.mfx.log");
    std::fs::create_dir(&log).unwrap();
    for f in ["manifest.json", "commands.jsonl"] {
        std::fs::copy(dir.path().join("session.mfx.log").join(f), log.join(f)).unwrap();
    }
    std::fs::remove_file(dir.path().join("scan.stl")).unwrap();
    assert!(
        replay(&log).unwrap().all_verified(),
        "sources are found relative to the log"
    );
}

#[test]
fn save_rejects_objects_without_a_source_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::new();
    s.add_geometry(
        mf_command::Geometry::Mesh(cube(1.0)),
        "made in memory".into(),
        None,
    );
    assert!(matches!(
        s.save_log(&dir.path().join("x.mfx.log")),
        Err(LogError::NoSource { .. })
    ));
}

fn tamper(log: &Path, field: &str) {
    let path = log.join("commands.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
    v[field] = serde_json::Value::String("0".repeat(64));
    let first = text.lines().next().unwrap();
    std::fs::write(
        &path,
        format!("{first}\n{}\n", serde_json::to_string(&v).unwrap()),
    )
    .unwrap();
}

#[test]
fn equivalent_when_only_positions_differ() {
    // Same structure, different bits: what another platform's floating point
    // would produce. Simulated by changing only the recorded output hash.
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("s.mfx.log");
    setup(dir.path()).save_log(&log).unwrap();
    tamper(&log, "output_hash");
    let report = replay(&log).unwrap();
    assert!(
        matches!(report.results[0].1, Verdict::Equivalent { .. }),
        "{:?}",
        report.results
    );
}

#[test]
fn mismatch_when_structure_differs() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("s.mfx.log");
    setup(dir.path()).save_log(&log).unwrap();
    tamper(&log, "output_hash");
    tamper(&log, "output_structure_hash");
    let report = replay(&log).unwrap();
    assert!(
        matches!(report.results[0].1, Verdict::Mismatch { .. }),
        "{:?}",
        report.results
    );
}

#[test]
fn input_mismatch_is_detected_before_running() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("s.mfx.log");
    setup(dir.path()).save_log(&log).unwrap();
    tamper(&log, "input_hash");
    tamper(&log, "input_structure_hash");
    let report = replay(&log).unwrap();
    assert!(
        matches!(report.results[0].1, Verdict::InputMismatch { .. }),
        "{:?}",
        report.results
    );
}

#[test]
fn commit_does_not_force_topology_on_the_new_revision() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(dir.path());
    let mesh = s.document().objects()[0].mesh().unwrap();
    assert!(
        !mesh.has_cached_topology(),
        "summary and analysis are computed lazily"
    );
}

#[test]
fn replay_names_a_missing_target_object() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("s.mfx.log");
    setup(dir.path()).save_log(&log).unwrap();
    let path = log.join("commands.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
    v["target"] = serde_json::json!(99);
    let first = text.lines().next().unwrap();
    std::fs::write(
        &path,
        format!("{first}\n{}\n", serde_json::to_string(&v).unwrap()),
    )
    .unwrap();
    let report = replay(&log).unwrap();
    match &report.results[0].1 {
        Verdict::Failed { error } => {
            assert!(
                error.contains("#99") && error.contains("does not exist"),
                "{error}"
            )
        }
        other => panic!("{other:?}"),
    }
}

/// A 12×12 plate with three square holes (and its rim), saved as STL and
/// imported: (session, object, source path).
fn holed_plate(dir: &Path) -> (Session, mf_command::ObjectId) {
    let n = 12;
    let mut e = mf_fixtures::grid(n).edit();
    for (i, j) in [(1u32, 1u32), (5, 1), (2, 6)] {
        let f = 2 * (j * n + i);
        e.remove_face(mf_geometry::FaceId(f)).unwrap();
        e.remove_face(mf_geometry::FaceId(f + 1)).unwrap();
    }
    let mesh = e.finish().unwrap().0;
    let src = dir.join("plate.stl");
    write_path(
        &mesh,
        &src,
        Format::StlBinary,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();
    let mut s = Session::new();
    let id = s.import(read_path(&src).unwrap(), "plate".into());
    (s, id)
}

/// The 4-edge holes of `id`'s current mesh, as loop ids by lowest vertex.
fn small_holes(s: &Session, id: mf_command::ObjectId) -> Vec<u32> {
    let m = s.document().object(id).unwrap().mesh().unwrap().clone();
    let topo = m.topology();
    let mut h: Vec<(u32, u32)> = topo
        .boundary_loops
        .iter()
        .filter(|l| l.edge_count == 4)
        .map(|l| (*l.vertices.iter().min().unwrap(), l.id))
        .collect();
    h.sort_unstable();
    h.into_iter().map(|(_, id)| id).collect()
}

fn fill(s: &mut Session, id: mf_command::ObjectId, hole: u32) {
    s.execute(Invocation {
        operands: Vec::new(),
        selection: Some(mf_selection::Selection {
            object: id,
            revision: s.current(),
            kind: mf_selection::SelectionKind::BoundaryLoop,
            ids: vec![hole],
        }),
        op_id: "mesh.fill_holes".into(),
        target: id,
        raw_params: BTreeMap::new(),
    })
    .unwrap();
}

/// Task 12 fix round 1, C1: marking an intentional opening is in the log,
/// so a hole fill recorded after it replays (its selection isn't bound to
/// a revision number the replay doesn't reach), and the marks come back.
#[test]
fn a_log_with_a_marked_opening_replays_exactly_with_its_marks() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, id) = holed_plate(dir.path());
    let holes = small_holes(&s, id);
    assert!(s.set_openings(id, vec![holes[0]], "Mark intentional opening".into()));
    fill(&mut s, id, holes[1]);
    let log = dir.path().join("s.mfx.log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert_eq!(report.results.len(), 1);
    assert!(
        matches!(report.results[0].1, Verdict::Exact),
        "{:?}",
        report.results
    );
    let marks = &report.session.document().object(id).unwrap().openings;
    assert_eq!(marks, &s.document().object(id).unwrap().openings);
    assert_eq!(marks.len(), 1);
}

/// A log saved on a branch made after an undo replays: replay numbers its
/// revisions afresh, so recorded selections are re-bound once the input
/// is verified.
#[test]
fn a_log_saved_after_undo_and_branch_replays_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, id) = holed_plate(dir.path());
    let holes = small_holes(&s, id);
    fill(&mut s, id, holes[0]);
    s.undo();
    fill(&mut s, id, holes[1]);
    // Bound to the branch's revision (r3), which replay calls r2.
    let left = small_holes(&s, id);
    fill(&mut s, id, *left.last().unwrap());
    let log = dir.path().join("s.mfx.log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert!(
        matches!(
            report.results[..],
            [(_, Verdict::Exact), (_, Verdict::Exact)]
        ),
        "{:?}",
        report.results
    );
}
