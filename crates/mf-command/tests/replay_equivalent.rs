#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The `Equivalent` replay verdict end to end (MF-ISSUE-046, MF-CMD-005).
//!
//! `tests/data/equivalent-replay/` holds a command log recorded by a build
//! whose computed positions legitimately differ from this one's: it was
//! written with the test-only `test-position-jitter` feature and
//! `MF_TEST_POSITION_JITTER=1e-9`, which scales every coordinate of every
//! vertex an operation creates by 1 ± 1e-9: a relative error, as another
//! platform's floating point might make. Replaying it in a normal build must give
//! `Equivalent` (not `Exact`: the bits differ), and the same log with one
//! changed index must still be a `Mismatch`.
//!
//! Regenerate the fixture (only if the source mesh or the fill changes):
//!
//! ```text
//! MF_TEST_POSITION_JITTER=1e-9 cargo test -p mf-command \
//!     --features test-position-jitter --test replay_equivalent \
//!     -- --ignored record_the_jittered_fixture
//! ```

use std::path::{Path, PathBuf};

use mf_command::{CommandId, GeometrySummary, Verdict, replay};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/equivalent-replay")
}

const LOG: &str = "session.mfx.log";
const SOURCE: &str = "two-holes.stl";

/// Copy the fixture (source and log, keeping their relative layout) to a
/// fresh directory, so a test can change the log.
fn copy_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(fixture().join(SOURCE), dir.path().join(SOURCE)).unwrap();
    std::fs::create_dir(dir.path().join(LOG)).unwrap();
    for f in ["manifest.json", "commands.jsonl"] {
        std::fs::copy(fixture().join(LOG).join(f), dir.path().join(LOG).join(f)).unwrap();
    }
    dir
}

#[test]
fn a_log_with_legitimately_different_positions_replays_as_equivalent() {
    let report = replay(&fixture().join(LOG)).unwrap();
    assert_eq!(report.results.len(), 2, "{:?}", report.results);
    let mesh = report
        .session
        .document()
        .objects()
        .first()
        .and_then(|o| o.mesh())
        .unwrap()
        .clone();
    let tolerance = 1e-9 * GeometrySummary::of(&mesh).diagonal().max(1.0);
    for (id, verdict) in &report.results {
        match verdict {
            Verdict::Equivalent { max_deviation } => {
                assert!(*max_deviation <= tolerance, "command {}: {verdict:?}", id.0)
            }
            other => panic!("command {}: expected Equivalent, got {other:?}", id.0),
        }
    }
    assert!(report.all_verified());
}

#[test]
fn the_same_log_with_one_changed_index_is_a_mismatch() {
    let dir = copy_fixture();
    let commands = dir.path().join(LOG).join("commands.jsonl");
    let text = std::fs::read_to_string(&commands).unwrap();
    // The first fill's selected loop: point it at the other hole.
    let mut lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let first_fill = lines.iter_mut().find(|e| e["kind"] == "command").unwrap();
    let ids = first_fill["selection"]["ids"].as_array_mut().unwrap();
    assert_eq!(ids.len(), 1);
    let other = 1 - ids[0].as_u64().unwrap();
    ids[0] = other.into();
    let changed: Vec<String> = lines.iter().map(|e| e.to_string()).collect();
    std::fs::write(&commands, changed.join("\n") + "\n").unwrap();

    let report = replay(&dir.path().join(LOG)).unwrap();
    match report.results.first() {
        Some((CommandId(_), Verdict::Mismatch { .. })) => {}
        other => panic!("expected a Mismatch first, got {other:?}"),
    }
    assert!(!report.all_verified());
}

/// Replay the fixture after `change` edits the first command's recorded
/// output summary (structure untouched); the first verdict.
fn first_verdict_after(change: impl Fn(&mut serde_json::Value)) -> Verdict {
    let dir = copy_fixture();
    let commands = dir.path().join(LOG).join("commands.jsonl");
    let text = std::fs::read_to_string(&commands).unwrap();
    let mut lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let first = lines.iter_mut().find(|e| e["kind"] == "command").unwrap();
    change(&mut first["output_summary"]);
    let changed: Vec<String> = lines.iter().map(|e| e.to_string()).collect();
    std::fs::write(&commands, changed.join("\n") + "\n").unwrap();
    let report = replay(&dir.path().join(LOG)).unwrap();
    report.results.first().unwrap().1.clone()
}

/// The Equivalent tolerance stays tight (1e-9): a recorded surface area
/// off by a relative 1e-6, with the structure unchanged, is a Mismatch.
#[test]
fn an_area_off_by_a_millionth_is_a_mismatch() {
    match first_verdict_after(|summary| {
        let area = summary["surface_area"].as_f64().unwrap();
        summary["surface_area"] = (area * (1.0 + 1e-6)).into();
    }) {
        Verdict::Mismatch { .. } => {}
        other => panic!("expected Mismatch, got {other:?}"),
    }
}

/// Likewise a recorded bounding box off by 1e-6 × its diagonal.
#[test]
fn a_bounding_box_off_by_a_millionth_of_its_diagonal_is_a_mismatch() {
    match first_verdict_after(|summary| {
        let x = summary["bbox"][1][0].as_f64().unwrap();
        // The sphere's diagonal is 2√3.
        summary["bbox"][1][0] = (x + 1e-6 * 2.0 * 3f64.sqrt()).into();
    }) {
        Verdict::Mismatch { .. } => {}
        other => panic!("expected Mismatch, got {other:?}"),
    }
}

/// Writes `tests/data/equivalent-replay/`: a sphere with two holes, each
/// filled (refined, so new vertices are computed) by its own command.
#[cfg(feature = "test-position-jitter")]
#[test]
#[ignore = "regenerates the committed fixture; see the module docs"]
fn record_the_jittered_fixture() {
    use std::collections::BTreeMap;

    use mf_command::{Invocation, Session};
    use mf_geometry::FaceId;
    use mf_selection::{Selection, SelectionKind};

    assert_eq!(
        std::env::var("MF_TEST_POSITION_JITTER").as_deref(),
        Ok("1e-9"),
        "record with MF_TEST_POSITION_JITTER=1e-9"
    );
    let dir = fixture();
    std::fs::create_dir_all(&dir).unwrap();
    // Two holes on opposite sides of the equator, big enough that
    // refinement adds vertices.
    let sphere = mf_fixtures::uv_sphere(24, 36);
    let near = |t: [u32; 3], c: [f64; 3]| {
        let p = t.map(|v| sphere.corner_position(v));
        let m = [0, 1, 2].map(|k| (p[0][k] + p[1][k] + p[2][k]) / 3.0);
        (0..3).map(|k| (m[k] - c[k]).powi(2)).sum::<f64>() < 0.7f64.powi(2)
    };
    let doomed: Vec<FaceId> = sphere
        .faces()
        .filter(|&(_, t)| near(t, [1.0, 0.0, 0.0]) || near(t, [-1.0, 0.0, 0.0]))
        .map(|(f, _)| f)
        .collect();
    let mut e = sphere.edit();
    for f in doomed {
        e.remove_face(f).unwrap();
    }
    let mesh = e.finish().unwrap().0;
    let source = dir.join(SOURCE);
    mf_io::write_path(
        &mesh,
        &source,
        mf_io::Format::StlBinary,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();

    let mut model = mf_io::read_model(&source).unwrap();
    let mut s = Session::new();
    let obj = model.objects.swap_remove(0);
    let id = s.import_object(obj, model.unit, model.source, 0);
    let open_loops = |s: &Session| -> Vec<u32> {
        let doc = s.document();
        let m = doc.object(id).unwrap().mesh().unwrap();
        m.topology().boundary_loops.iter().map(|l| l.id).collect()
    };
    assert_eq!(open_loops(&s), vec![0, 1], "two holes");
    // Fill the first open hole, twice: loop 0, then the other (renumbered 0).
    for _ in 0..2 {
        let inv = Invocation {
            selection: Some(Selection {
                object: id,
                revision: s.current(),
                kind: SelectionKind::BoundaryLoop,
                ids: vec![open_loops(&s)[0]],
            }),
            op_id: "mesh.fill_holes".into(),
            target: id,
            raw_params: BTreeMap::from([("refine".into(), "true".into())]),
        };
        match s.execute(inv).unwrap() {
            mf_command::CommitOutcome::NewRevision { report } => assert!(
                report["created_vertices"].as_u64().unwrap() > 0,
                "refinement added vertices, so there is something to jitter: {report}"
            ),
            other => panic!("{other:?}"),
        }
    }
    assert!(open_loops(&s).is_empty());
    let log = dir.join(LOG);
    if log.exists() {
        std::fs::remove_dir_all(&log).unwrap();
    }
    s.save_log(&log).unwrap();
}
