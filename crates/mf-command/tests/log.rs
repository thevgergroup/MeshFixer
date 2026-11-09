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
    write_path(&mesh, &src, Format::StlBinary).unwrap();
    let mut s = Session::new();
    let id = s.import(read_path(&src).unwrap(), "scan".into());
    s.execute(Invocation {
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
    write_path(&cube(3.0), &dir.path().join("scan.stl"), Format::StlBinary).unwrap();
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
