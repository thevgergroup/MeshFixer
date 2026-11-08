#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Selection translation over provenance steps (MF-SEL-003).

use mf_document::{ObjectId, RevisionId};
use mf_fixtures::{cube, cube_with_missing_face, merge, tetrahedron, translated};
use mf_geometry::{FaceId, Mesh, Sources};
use mf_selection::{Selection, SelectionKind, Step, TranslateError, Translation, resolve};

const OBJ: ObjectId = ObjectId(1);

fn sel(kind: SelectionKind, ids: Vec<u32>) -> Selection {
    Selection {
        object: OBJ,
        revision: RevisionId(1),
        kind,
        ids,
    }
}

fn remove_island() -> (Mesh, Mesh, mf_geometry::Provenance) {
    let before = merge(&[cube(1.0), translated(&tetrahedron(), [5.0, 0.0, 0.0])]);
    let mut e = before.edit();
    for f in 12..16 {
        e.remove_face(FaceId(f)).unwrap();
    }
    let (after, prov) = e.finish().unwrap();
    (before, after, prov)
}

#[test]
fn faces_map_forward_with_removed_faces_lost() {
    let (before, after, prov) = remove_island();
    let s = sel(SelectionKind::Faces, vec![0, 5, 13]);
    match resolve(&s, &before, &after, &[Step::Forward(&prov)], RevisionId(2)) {
        Translation::Partial { mapped, lost } => {
            assert_eq!(mapped.ids, vec![0, 5]);
            assert_eq!(mapped.revision, RevisionId(2));
            assert_eq!(lost, vec![13]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn faces_map_fully_when_untouched() {
    let (before, after, prov) = remove_island();
    let s = sel(SelectionKind::Faces, vec![0, 1, 2]);
    assert!(matches!(
        resolve(&s, &before, &after, &[Step::Forward(&prov)], RevisionId(2)),
        Translation::Mapped(m) if m.ids == vec![0, 1, 2]
    ));
}

#[test]
fn backward_step_loses_created_faces() {
    let before = cube_with_missing_face();
    let mut e = before.edit();
    let f = e.add_face([4, 5, 6], Sources::default()).unwrap();
    let (after, prov) = e.finish().unwrap();
    let s = Selection {
        revision: RevisionId(2),
        ..sel(SelectionKind::Faces, vec![0, f.0])
    };
    match resolve(&s, &after, &before, &[Step::Backward(&prov)], RevisionId(1)) {
        Translation::Partial { mapped, lost } => {
            assert_eq!(mapped.ids, vec![0]);
            assert_eq!(lost, vec![f.0]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn all_lost_is_failure_not_empty_selection() {
    let (before, after, prov) = remove_island();
    let s = sel(SelectionKind::Faces, vec![12, 13]);
    assert!(matches!(
        resolve(&s, &before, &after, &[Step::Forward(&prov)], RevisionId(2)),
        Translation::Failed(TranslateError::AllLost { .. })
    ));
}

#[test]
fn component_selection_reresolves_to_destination_index() {
    // Island first (component 1 by size), then cube.
    let before = merge(&[cube(1.0), translated(&tetrahedron(), [5.0, 0.0, 0.0])]);
    let cube_component = before
        .topology()
        .components
        .iter()
        .position(|c| c.len() == 12)
        .unwrap() as u32;
    let (_, after, prov) = remove_island();
    let s = sel(SelectionKind::Component, vec![cube_component]);
    assert!(matches!(
        resolve(&s, &before, &after, &[Step::Forward(&prov)], RevisionId(2)),
        Translation::Mapped(m) if m.kind == SelectionKind::Component && m.ids == vec![0]
    ));
    let island = sel(SelectionKind::Component, vec![1]);
    assert!(matches!(
        resolve(
            &island,
            &before,
            &after,
            &[Step::Forward(&prov)],
            RevisionId(2)
        ),
        Translation::Failed(TranslateError::AllLost { .. })
    ));
}

#[test]
fn loop_selection_maps_while_open_and_fails_when_closed() {
    let open = cube_with_missing_face();
    let loop_id = open.topology().boundary_loops[0].id;
    let s = sel(SelectionKind::BoundaryLoop, vec![loop_id]);

    // An unrelated edit keeps the loop.
    let mut e = open.edit();
    e.move_vertex(mf_geometry::VertexId(0), [-0.1, 0.0, 0.0])
        .unwrap();
    let (moved, prov) = e.finish().unwrap();
    assert!(matches!(
        resolve(&s, &open, &moved, &[Step::Forward(&prov)], RevisionId(2)),
        Translation::Mapped(m) if m.kind == SelectionKind::BoundaryLoop
    ));

    // Closing it (two faces over the hole) reports which faces closed it.
    let mut e = open.edit();
    let a = e
        .add_face(
            [4, 5, 6],
            Sources {
                faces: vec![],
                loops: vec![loop_id],
            },
        )
        .unwrap();
    let b = e
        .add_face(
            [4, 6, 7],
            Sources {
                faces: vec![],
                loops: vec![loop_id],
            },
        )
        .unwrap();
    let (closed, prov) = e.finish().unwrap();
    match resolve(&s, &open, &closed, &[Step::Forward(&prov)], RevisionId(2)) {
        Translation::Failed(TranslateError::LoopClosed { faces }) => assert_eq!(faces, vec![a, b]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn selection_serializes() {
    let s = sel(SelectionKind::Faces, vec![1, 2]);
    let back: Selection = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
    assert_eq!(back, s);
}

#[test]
fn faces_follow_a_compaction_remap_both_ways() {
    let (_, tomb, _) = remove_island(); // faces 12..16 tombstoned
    let (compact, map) = tomb.compacted();
    let prov = mf_geometry::Provenance::from_compaction(&map);
    // Faces 0..12 keep their positions, so a selection of face 3 stays face 3;
    // build a case with a real renumbering instead: tombstone face 0 first.
    let mut e = tomb.edit();
    e.remove_face(FaceId(0)).unwrap();
    let (holey, _) = e.finish().unwrap();
    let (compact2, map2) = holey.compacted();
    let prov2 = mf_geometry::Provenance::from_compaction(&map2);
    let s = Selection {
        revision: RevisionId(1),
        ..sel(SelectionKind::Faces, vec![1, 5])
    };
    match resolve(
        &s,
        &holey,
        &compact2,
        &[Step::Forward(&prov2)],
        RevisionId(2),
    ) {
        Translation::Mapped(m) => assert_eq!(m.ids, vec![0, 4]),
        other => panic!("{other:?}"),
    }
    let back = Selection {
        revision: RevisionId(2),
        ..sel(SelectionKind::Faces, vec![0, 4])
    };
    match resolve(
        &back,
        &compact2,
        &holey,
        &[Step::Backward(&prov2)],
        RevisionId(1),
    ) {
        Translation::Mapped(m) => assert_eq!(m.ids, vec![1, 5]),
        other => panic!("{other:?}"),
    }
    let _ = (compact, prov);
}
