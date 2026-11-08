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
        op_id: "mesh.remove_small_components".into(),
        target: id,
        raw_params: BTreeMap::from([("max_triangles".into(), "5".into())]),
    })
    .unwrap();
    s
}

#[test]
fn save_load_replay_exact() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(dir.path());
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    assert!(log.join("manifest.json").exists() && log.join("commands.jsonl").exists());
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
