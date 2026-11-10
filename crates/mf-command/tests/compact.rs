#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `mesh.compact` reclaims tombstoned slots (ADR-0001, MF-ISSUE-020): every
//! live face and vertex is renumbered, recorded as a slot remap in the
//! revision's provenance, so selections translate through it; and, like any
//! command, it replays exactly (MF-CMD-004).

use std::collections::BTreeMap;
use std::path::Path;

use mf_command::{CommitOutcome, Invocation, ObjectId, Session, Verdict, replay};
use mf_fixtures::{cube_with_missing_face, merge, tetrahedron, translated};
use mf_geometry::{FaceId, Mesh};
use mf_io::{Format, read_path, write_path};
use mf_selection::{Selection, SelectionKind, Translation};

/// A tetrahedron island (faces 0..4, vertices 0..4) before an open cube
/// (faces 4..14, vertices 4..12), so removing the island leaves tombstones
/// below every kept slot: compacting renumbers all of them.
fn island_first() -> Mesh {
    merge(&[
        translated(&tetrahedron(), [5.0, 0.0, 0.0]),
        cube_with_missing_face(),
    ])
}

fn invocation(target: ObjectId, op: &str, params: &[(&str, &str)]) -> Invocation {
    Invocation {
        selection: None,
        op_id: op.into(),
        target,
        raw_params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn remove_island(s: &mut Session, id: ObjectId) {
    s.execute(invocation(
        id,
        "mesh.remove_small_components",
        &[("max_triangles", "5"), ("max_relative_size", "100")],
    ))
    .unwrap();
}

fn mesh(s: &Session, id: ObjectId) -> Mesh {
    s.document().object(id).unwrap().mesh().unwrap().clone()
}

#[test]
fn compact_remaps_and_selections_translate() {
    let mut s = Session::new();
    let id = s.add_geometry(
        mf_command::Geometry::Mesh(island_first()),
        "part".into(),
        None,
    );
    remove_island(&mut s, id);
    let holey = mesh(&s, id);
    assert_eq!((holey.face_slots(), holey.face_count()), (14, 10));
    let before = s.current();
    let selected = Selection {
        object: id,
        revision: before,
        kind: SelectionKind::Faces,
        ids: vec![4, 9, 13],
    };

    let outcome = s.execute(invocation(id, "mesh.compact", &[])).unwrap();
    let CommitOutcome::NewRevision { report } = outcome else {
        panic!("compacting tombstones makes a revision: {outcome:?}");
    };
    assert_eq!(report["reclaimed_faces"], 4);
    assert_eq!(report["reclaimed_vertices"], 4);
    let after = s.current();
    let compact = mesh(&s, id);
    assert!(compact.is_compact());
    assert_eq!((compact.face_slots(), compact.vertex_slots()), (10, 8));
    // Same geometry, densely numbered.
    assert_eq!(compact.to_arrays(), holey.to_arrays());
    assert_eq!(
        compact.face(FaceId(0)),
        Some(holey.face(FaceId(4)).unwrap().map(|v| v - 4))
    );

    // The remap is in the revision's provenance, and nothing else is.
    let prov = &s.node(after).unwrap().provenance[&id];
    let remap = prov
        .remap
        .as_ref()
        .expect("compaction records a slot remap");
    assert_eq!(remap.faces[..5], [None, None, None, None, Some(FaceId(0))]);
    assert_eq!(remap.faces[13], Some(FaceId(9)));
    assert!(prov.removed_faces.is_empty() && prov.created_faces.is_empty());

    // Forward: the selection follows the faces to their new numbers.
    match s.translate(&selected, after) {
        Translation::Mapped(m) => {
            assert_eq!(m.ids, vec![0, 5, 9]);
            assert_eq!(m.revision, after);
        }
        other => panic!("{other:?}"),
    }
    // Backward (undo): back to the old numbers.
    let back = Selection {
        revision: after,
        ids: vec![0, 5, 9],
        ..selected.clone()
    };
    match s.translate(&back, before) {
        Translation::Mapped(m) => assert_eq!(m.ids, vec![4, 9, 13]),
        other => panic!("{other:?}"),
    }
    // Components keep their meaning across the renumbering.
    let piece = Selection {
        kind: SelectionKind::Component,
        ids: vec![0],
        ..selected.clone()
    };
    match s.translate(&piece, after) {
        Translation::Mapped(m) => assert_eq!(m.ids, vec![0]),
        other => panic!("{other:?}"),
    }

    // Undo restores the tombstoned revision; compacting a compact mesh
    // changes nothing.
    assert!(s.undo());
    assert!(Mesh::ptr_eq(&mesh(&s, id), &holey));
    assert!(s.redo());
    let again = s.execute(invocation(id, "mesh.compact", &[])).unwrap();
    assert!(
        matches!(&again, CommitOutcome::NoChange { report } if report["reclaimed_faces"] == 0),
        "{again:?}"
    );
}

fn logged_session(dir: &Path) -> Session {
    let src = dir.join("scan.stl");
    write_path(
        &island_first(),
        &src,
        Format::StlBinary,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();
    let mut s = Session::new();
    let id = s.import(read_path(&src).unwrap(), "scan".into());
    remove_island(&mut s, id);
    s.execute(invocation(id, "mesh.compact", &[])).unwrap();
    // A command after the compaction runs on the renumbered mesh.
    s.execute(invocation(id, "mesh.fill_holes", &[])).unwrap();
    s
}

#[test]
fn compact_is_replayable_exact() {
    let dir = tempfile::tempdir().unwrap();
    let s = logged_session(dir.path());
    let ops: Vec<String> = s
        .records_to_current()
        .iter()
        .map(|r| r.op_id.clone())
        .collect();
    assert_eq!(
        ops,
        [
            "mesh.remove_small_components",
            "mesh.compact",
            "mesh.fill_holes"
        ]
    );
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert_eq!(report.results.len(), 3);
    for (id, verdict) in &report.results {
        assert!(matches!(verdict, Verdict::Exact), "{id:?}: {verdict:?}");
    }
    let original = s.document().objects()[0].geometry.content_hash();
    let replayed = report.session.document().objects()[0]
        .geometry
        .content_hash();
    assert_eq!(original, replayed);
}

/// Boundary-loop selections translate across a compaction both ways (M4
/// Task 9 review P2): a loop is followed through its vertices, which the
/// compaction renumbers.
#[test]
fn compact_translates_boundary_loop_selections() {
    // An island first, then a 6×6 grid with two one-cell holes: three
    // loops (the rim and two holes), every vertex renumbered by compaction.
    let n = 6;
    let mut e = mf_fixtures::grid(n).edit();
    e.remove_face(FaceId(2 * (n + 1))).unwrap();
    e.remove_face(FaceId(2 * (4 * n + 4))).unwrap();
    let holed = e.finish().unwrap().0;
    let part = merge(&[translated(&tetrahedron(), [50.0, 0.0, 0.0]), holed]);
    let mut s = Session::new();
    let id = s.add_geometry(mf_command::Geometry::Mesh(part), "part".into(), None);
    remove_island(&mut s, id);
    let before = s.current();
    let holey = mesh(&s, id);
    s.execute(invocation(id, "mesh.compact", &[])).unwrap();
    let after = s.current();
    let compact = mesh(&s, id);
    let remap = s.node(after).unwrap().provenance[&id]
        .remap
        .clone()
        .unwrap();

    let loops_before = holey.topology().boundary_loops.clone();
    let loops_after = compact.topology().boundary_loops.clone();
    assert_eq!(loops_before.len(), 3);
    let vertex_set = |vs: &[u32]| {
        let mut v = vs.to_vec();
        v.sort_unstable();
        v
    };
    for l in &loops_before {
        let sel = Selection {
            object: id,
            revision: before,
            kind: SelectionKind::BoundaryLoop,
            ids: vec![l.id],
        };
        // Forward: the loop whose vertices are this loop's, renumbered.
        let Translation::Mapped(m) = s.translate(&sel, after) else {
            panic!("loop {}: {:?}", l.id, s.translate(&sel, after));
        };
        assert_eq!(m.ids.len(), 1);
        let renumbered: Vec<u32> = l
            .vertices
            .iter()
            .map(|&v| remap.vertices[v as usize].unwrap().0)
            .collect();
        assert_eq!(
            vertex_set(&loops_after[m.ids[0] as usize].vertices),
            vertex_set(&renumbered),
            "loop {}",
            l.id
        );
        // Backward: the same loop again.
        let back = Selection {
            revision: after,
            ids: m.ids.clone(),
            ..sel.clone()
        };
        match s.translate(&back, before) {
            Translation::Mapped(b) => assert_eq!(b.ids, vec![l.id]),
            other => panic!("loop {} back: {other:?}", l.id),
        }
    }
}
