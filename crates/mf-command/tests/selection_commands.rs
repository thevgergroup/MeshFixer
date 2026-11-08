#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Selections travel in invocations and records (MF-ISSUE-027, MF-SEL-004).

use std::collections::BTreeMap;

use mf_command::{CommandError, Geometry, Invocation, ObjectId, Session};
use mf_fixtures::{cube, merge, tetrahedron, translated};
use mf_selection::{Selection, SelectionKind};

fn session() -> (Session, ObjectId) {
    let mut s = Session::new();
    let mesh = merge(&[
        cube(1.0),
        translated(&tetrahedron(), [5.0, 0.0, 0.0]),
        translated(&tetrahedron(), [9.0, 0.0, 0.0]),
    ]);
    let id = s.add_geometry(Geometry::Mesh(mesh), "part".into(), None);
    (s, id)
}

fn remove_with(target: ObjectId, selection: Option<Selection>, max: &str) -> Invocation {
    Invocation {
        op_id: "mesh.remove_small_components".into(),
        target,
        selection,
        raw_params: BTreeMap::from([("max_triangles".to_string(), max.to_string())]),
    }
}

fn component(s: &Session, id: ObjectId, c: u32) -> Selection {
    Selection {
        object: id,
        revision: s.current(),
        kind: SelectionKind::Component,
        ids: vec![c],
    }
}

#[test]
fn selection_is_recorded() {
    let (mut s, id) = session();
    let sel = component(&s, id, 1);
    s.execute(remove_with(id, Some(sel.clone()), "5")).unwrap();
    assert_eq!(s.records_to_current()[0].selection, Some(sel));
}

#[test]
fn remove_selected_component_only() {
    let (mut s, id) = session();
    // Components: 0 = cube (12), 1 and 2 = tetrahedra (4 each).
    s.execute(remove_with(id, Some(component(&s, id, 1)), "5"))
        .unwrap();
    let mesh = s.document().object(id).unwrap().mesh().unwrap();
    assert_eq!(
        mesh.topology().components.len(),
        2,
        "only the selected island is removed"
    );
    assert_eq!(mesh.face_count(), 16);
}

#[test]
fn selected_component_above_threshold_fails_by_name() {
    let (mut s, id) = session();
    let err = s
        .execute(remove_with(id, Some(component(&s, id, 0)), "5"))
        .unwrap_err();
    assert!(err.to_string().contains("component 0"), "{err}");
}

#[test]
fn stale_selection_is_rejected() {
    let (mut s, id) = session();
    let old = component(&s, id, 2);
    s.execute(remove_with(id, Some(component(&s, id, 1)), "5"))
        .unwrap();
    let err = s.execute(remove_with(id, Some(old), "5")).unwrap_err();
    assert!(
        matches!(err, CommandError::StaleSelection { .. }),
        "{err:?}"
    );
}

#[test]
fn selection_for_another_object_is_rejected() {
    let (mut s, id) = session();
    let other = s.add_geometry(Geometry::Mesh(cube(1.0)), "other".into(), None);
    let sel = component(&s, other, 0);
    let err = s.execute(remove_with(id, Some(sel), "5")).unwrap_err();
    assert!(
        matches!(err, CommandError::SelectionMismatch { .. }),
        "{err:?}"
    );
}

#[test]
fn kind_not_in_spec_is_rejected() {
    let (mut s, id) = session();
    let sel = Selection {
        object: id,
        revision: s.current(),
        kind: SelectionKind::BoundaryLoop,
        ids: vec![0],
    };
    let err = s.execute(remove_with(id, Some(sel), "5")).unwrap_err();
    assert!(
        matches!(err, CommandError::UnsupportedTarget { .. }),
        "{err:?}"
    );
}
