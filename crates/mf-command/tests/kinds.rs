#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The command and history layers handle non-mesh geometry, while mesh
//! operations reject it cleanly (MF-DOC-005).

use std::collections::BTreeMap;

use mf_command::{CommandError, Invocation, Session};
use mf_document::Geometry;

#[test]
fn dummy_kind_rejected_by_mesh_ops() {
    let mut s = Session::new();
    let id = s.add_geometry(Geometry::Dummy, "not a mesh".into(), None);
    assert_eq!(
        s.document().object(id).unwrap().geometry.kind().as_str(),
        "dummy"
    );
    let err = s
        .execute(Invocation {
            op_id: "mesh.inspect".into(),
            target: id,
            raw_params: BTreeMap::new(),
        })
        .unwrap_err();
    assert!(
        matches!(err, CommandError::UnsupportedGeometry { ref kind, .. } if kind == "dummy"),
        "{err:?}"
    );
    assert!(s.undo(), "history handles the non-mesh object");
    assert!(s.document().object(id).is_none());
    assert!(s.redo());
}
