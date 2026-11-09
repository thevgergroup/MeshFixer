#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Intentional openings (M4 Task 12): a hole marked as meant to be open is
//! stored on its object as boundary-loop ids, by an undoable document
//! command that changes no geometry, and follows the history like any
//! selection: a command that changes the mesh translates the marks to the
//! new revision's loop ids.

use std::collections::BTreeMap;

use mf_command::{Geometry, Invocation, ObjectId, Session};
use mf_geometry::{FaceId, Mesh};
use mf_selection::{Selection, SelectionKind};

/// A 6×6 grid with three one-cell holes (and its open rim).
fn three_holes() -> Mesh {
    let n = 6;
    let mut e = mf_fixtures::grid(n).edit();
    for (i, j) in [(1, 1), (4, 1), (2, 4)] {
        e.remove_face(FaceId(2 * (j * n + i))).unwrap();
        e.remove_face(FaceId(2 * (j * n + i) + 1)).unwrap();
    }
    e.finish().unwrap().0
}

fn mesh(s: &Session, id: ObjectId) -> Mesh {
    s.document().object(id).unwrap().mesh().unwrap().clone()
}

/// The loop with the most edges is the rim; the others are the holes, by
/// their smallest vertex (stable across revisions).
fn holes_by_vertex(m: &Mesh) -> Vec<(u32, u32)> {
    let topo = m.topology();
    let mut out: Vec<(u32, u32)> = topo
        .boundary_loops
        .iter()
        .filter(|l| l.edge_count == 4)
        .map(|l| (*l.vertices.iter().min().unwrap(), l.id))
        .collect();
    out.sort_unstable();
    out
}

#[test]
fn marks_are_an_undoable_command_and_follow_edits() {
    let mut s = Session::new();
    let id = s.add_geometry(Geometry::Mesh(three_holes()), "plate".into(), None);
    let holes = holes_by_vertex(&mesh(&s, id));
    assert_eq!(holes.len(), 3);
    let (keep_vertex, keep_loop) = holes[2];
    let before = s.current();
    let geometry = mesh(&s, id);

    assert!(s.set_openings(id, vec![keep_loop], "Mark intentional opening".into()));
    assert_ne!(s.current(), before, "a new revision");
    assert!(Mesh::ptr_eq(&mesh(&s, id), &geometry), "no geometry change");
    assert_eq!(s.document().object(id).unwrap().openings, vec![keep_loop]);
    assert_eq!(
        s.node(s.current()).unwrap().label,
        "Mark intentional opening"
    );

    // Fill another hole: the mark follows to the new revision's loop id.
    let (_, fill_loop) = holes[0];
    s.execute(Invocation {
        op_id: "mesh.fill_holes".into(),
        target: id,
        selection: Some(Selection {
            object: id,
            revision: s.current(),
            kind: SelectionKind::BoundaryLoop,
            ids: vec![fill_loop],
        }),
        raw_params: BTreeMap::new(),
    })
    .unwrap();
    let after = holes_by_vertex(&mesh(&s, id));
    assert_eq!(after.len(), 2);
    let now = after.iter().find(|(v, _)| *v == keep_vertex).unwrap().1;
    assert_eq!(s.document().object(id).unwrap().openings, vec![now]);

    // Undo twice: back to no marks; redo brings the mark back.
    s.undo();
    assert_eq!(s.document().object(id).unwrap().openings, vec![keep_loop]);
    s.undo();
    assert!(s.document().object(id).unwrap().openings.is_empty());
    s.redo();
    assert_eq!(s.document().object(id).unwrap().openings, vec![keep_loop]);
    // An unknown object is refused.
    assert!(!s.set_openings(ObjectId(99), vec![], "x".into()));
}

/// Filling the marked hole itself drops its mark (it's no longer a hole).
#[test]
fn filling_a_marked_hole_drops_its_mark() {
    let mut s = Session::new();
    let id = s.add_geometry(Geometry::Mesh(three_holes()), "plate".into(), None);
    let (_, l) = holes_by_vertex(&mesh(&s, id))[1];
    s.set_openings(id, vec![l], "Mark".into());
    s.execute(Invocation {
        op_id: "mesh.fill_holes".into(),
        target: id,
        selection: Some(Selection {
            object: id,
            revision: s.current(),
            kind: SelectionKind::BoundaryLoop,
            ids: vec![l],
        }),
        raw_params: BTreeMap::new(),
    })
    .unwrap();
    assert!(s.document().object(id).unwrap().openings.is_empty());
}
