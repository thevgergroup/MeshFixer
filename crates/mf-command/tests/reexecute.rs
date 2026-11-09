#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Re-executing a recorded command on another revision (MF-HIST-002).

use std::collections::BTreeMap;

use mf_command::{CommandError, Geometry, Invocation, ObjectId, RevisionId, Session};
use mf_fixtures::{cube, merge, tetrahedron, translated, uv_sphere};
use mf_geometry::{FaceId, Mesh};
use mf_selection::{Selection, SelectionKind};

/// A sphere with one hole, plus a 4-triangle and a 12-triangle island.
fn damaged() -> Mesh {
    let mut e = uv_sphere(8, 12).edit();
    e.remove_face(FaceId(40)).unwrap();
    e.remove_face(FaceId(41)).unwrap();
    let sphere = e.finish().unwrap().0;
    merge(&[
        sphere,
        translated(&tetrahedron(), [5.0, 0.0, 0.0]),
        translated(&cube(1.0), [9.0, 0.0, 0.0]),
    ])
}

fn invocation(
    op: &str,
    target: ObjectId,
    sel: Option<Selection>,
    p: &[(&str, &str)],
) -> Invocation {
    Invocation {
        op_id: op.into(),
        target,
        selection: sel,
        raw_params: p
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn mesh(s: &Session, id: ObjectId) -> Mesh {
    s.document().object(id).unwrap().mesh().unwrap().clone()
}

/// The sphere's hole as a selection at the current revision.
fn hole(s: &Session, id: ObjectId) -> Selection {
    let m = mesh(s, id);
    let topo = m.topology();
    assert_eq!(topo.boundary_loops.len(), 1, "only the sphere has a hole");
    Selection {
        object: id,
        revision: s.current(),
        kind: SelectionKind::BoundaryLoop,
        ids: vec![topo.boundary_loops[0].id],
    }
}

/// Import, then remove islands under `max` triangles.
fn upstream(s: &mut Session, id: ObjectId, import: RevisionId, max: &str) -> RevisionId {
    assert!(s.checkout(import));
    s.execute(invocation(
        "mesh.remove_small_components",
        id,
        None,
        &[("max_triangles", max), ("max_relative_size", "100")],
    ))
    .unwrap();
    s.current()
}

#[test]
fn reexecute_fill_on_regenerated_upstream() {
    let mut s = Session::new();
    let id = s.add_geometry(Geometry::Mesh(damaged()), "scan".into(), None);
    let import = s.current();
    // Upstream A removes the tetrahedron only; the fill is recorded on it.
    upstream(&mut s, id, import, "5");
    assert_eq!(mesh(&s, id).topology().components.len(), 2);
    let sel = hole(&s, id);
    s.execute(invocation("mesh.fill_holes", id, Some(sel), &[]))
        .unwrap();
    let record = s.records_to_current().last().unwrap().clone();

    // Upstream B, a regenerated branch: a higher threshold removes the cube too.
    let b = upstream(&mut s, id, import, "13");
    assert_eq!(mesh(&s, id).topology().components.len(), 1);
    s.reexecute(&record, b).unwrap();

    let out = mesh(&s, id);
    let d = mf_geometry::inspect(&out);
    assert!(d.is_closed && d.is_manifold, "{d:?}");
    assert_eq!(d.components, 1);
    let rerun = s.records_to_current().last().unwrap().clone();
    assert_eq!(rerun.input_revision, b);
    assert_eq!(rerun.op_id, record.op_id);
    assert_eq!(rerun.params, record.params);
    assert_eq!(rerun.selection.as_ref().unwrap().revision, b);
    // The same hole was closed the same way: same patch, on B's mesh.
    assert_eq!(rerun.affected.created, record.affected.created);
    assert_eq!(rerun.affected.removed, 0);
    assert_eq!(
        out.face_count(),
        mesh_at(&s, b, id).face_count() + record.affected.created as usize
    );
}

fn mesh_at(s: &Session, rev: RevisionId, id: ObjectId) -> Mesh {
    s.node(rev)
        .unwrap()
        .document
        .object(id)
        .unwrap()
        .mesh()
        .unwrap()
        .clone()
}

#[test]
fn reexecute_fails_by_name_when_target_gone() {
    let mut s = Session::new();
    let id = s.add_geometry(Geometry::Mesh(damaged()), "scan".into(), None);
    let import = s.current();
    let sel = hole(&s, id);
    s.execute(invocation("mesh.fill_holes", id, Some(sel), &[]))
        .unwrap();
    let record = s.records_to_current().last().unwrap().clone();

    // Another branch fills every hole, so the recorded loop no longer exists.
    assert!(s.checkout(import));
    s.execute(invocation("mesh.fill_holes", id, None, &[]))
        .unwrap();
    let closed = s.current();
    let err = s.reexecute(&record, closed).unwrap_err();
    assert!(
        matches!(err, CommandError::Untranslatable { .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("closed"), "{err}");
    assert_eq!(
        s.current(),
        closed,
        "a failed re-execution doesn't move the cursor"
    );
}
