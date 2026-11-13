#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::collections::{BTreeMap, BTreeSet};

use mf_fixtures::{cube, merge, tetrahedron, translated};
use mf_geometry::{Mesh, inspect};
use mf_operations::{find, registry};
use mf_spec::{Params, builtin};

fn params(op: &str, pairs: &[(&str, &str)]) -> Params {
    let raw: BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    builtin::get(op).unwrap().validate(&raw).unwrap()
}

#[test]
fn parity_specs_and_impls() {
    let specs: BTreeSet<(String, u32)> = builtin::specs()
        .iter()
        .map(|s| (s.id.clone(), s.version))
        .collect();
    let impls: BTreeSet<(String, u32)> = registry()
        .iter()
        .map(|o| (o.id().to_string(), o.version()))
        .collect();
    assert_eq!(specs, impls);
    assert_eq!(
        registry().len(),
        impls.len(),
        "duplicate implementation ids"
    );
}

#[test]
fn removes_island() {
    let c = cube(1.0);
    let input = merge(&[c.clone(), translated(&tetrahedron(), [5.0, 0.0, 0.0])]);
    let op = find("mesh.remove_small_components").unwrap();
    let out = op
        .execute(
            &input,
            &mf_operations::Target::WholeObject,
            &params(
                op.id(),
                &[("max_triangles", "5"), ("max_relative_size", "100")],
            ),
            &mf_operations::RunContext::none(),
        )
        .unwrap();
    let mesh = out.change.unwrap().0;
    assert_eq!(mesh.triangle_count(), 12);
    assert_eq!(inspect(&mesh).components, 1);
    assert_eq!(mesh.to_arrays(), c.to_arrays());
    assert_eq!(out.report["removed_components"], 1);
    assert_eq!(out.report["removed_triangles"], 4);
}

#[test]
fn kept_faces_keep_relative_order() {
    // Island first, then cube: the cube faces must stay in order after compaction.
    let c = translated(&cube(1.0), [5.0, 0.0, 0.0]);
    let input = merge(&[tetrahedron(), c.clone()]);
    let op = find("mesh.remove_small_components").unwrap();
    let mesh = op
        .execute(
            &input,
            &mf_operations::Target::WholeObject,
            &params(
                op.id(),
                &[("max_triangles", "5"), ("max_relative_size", "100")],
            ),
            &mf_operations::RunContext::none(),
        )
        .unwrap()
        .change
        .unwrap()
        .0;
    assert_eq!(mesh.to_arrays(), c.to_arrays());
}

#[test]
fn keep_largest_guard() {
    let input = merge(&[tetrahedron(), translated(&cube(1.0), [5.0, 0.0, 0.0])]);
    let op = find("mesh.remove_small_components").unwrap();
    let kept = op
        .execute(
            &input,
            &mf_operations::Target::WholeObject,
            &params(
                op.id(),
                &[("max_triangles", "50"), ("max_relative_size", "100")],
            ),
            &mf_operations::RunContext::none(),
        )
        .unwrap()
        .change
        .unwrap()
        .0;
    assert_eq!(kept.triangle_count(), 12);
    let none = op
        .execute(
            &input,
            &mf_operations::Target::WholeObject,
            &params(
                op.id(),
                &[
                    ("max_triangles", "50"),
                    ("max_relative_size", "100"),
                    ("keep_largest", "false"),
                ],
            ),
            &mf_operations::RunContext::none(),
        )
        .unwrap()
        .change
        .unwrap()
        .0;
    assert!(none.is_empty());
    assert_eq!(none.vertex_count(), 0);
}

#[test]
fn input_not_mutated() {
    let input = merge(&[cube(1.0), translated(&tetrahedron(), [5.0, 0.0, 0.0])]);
    let before = input.content_hash();
    let op = find("mesh.remove_small_components").unwrap();
    op.execute(
        &input,
        &mf_operations::Target::WholeObject,
        &params(
            op.id(),
            &[("max_triangles", "5"), ("max_relative_size", "100")],
        ),
        &mf_operations::RunContext::none(),
    )
    .unwrap();
    assert_eq!(input.content_hash(), before);
    assert_eq!(input.triangle_count(), 16);
}

#[test]
fn inspect_is_read_only() {
    let op = find("mesh.inspect").unwrap();
    let out = op
        .execute(
            &cube(1.0),
            &mf_operations::Target::WholeObject,
            &params(op.id(), &[]),
            &mf_operations::RunContext::none(),
        )
        .unwrap();
    assert!(out.change.is_none());
    assert_eq!(out.report["components"], 1);
    assert_eq!(out.report["is_closed"], true);
}

#[test]
fn empty_mesh_ok() {
    let op = find("mesh.remove_small_components").unwrap();
    let out = op
        .execute(
            &Mesh::empty(),
            &mf_operations::Target::WholeObject,
            &params(op.id(), &[]),
            &mf_operations::RunContext::none(),
        )
        .unwrap();
    assert!(out.change.is_none(), "nothing to remove: no new revision");
}

mod edit_based {
    use super::*;
    use mf_geometry::FaceId;
    use mf_operations::{OpError, Target};

    fn island_input() -> Mesh {
        merge(&[cube(1.0), translated(&tetrahedron(), [5.0, 0.0, 0.0])])
    }

    #[test]
    fn kept_faces_keep_their_ids() {
        let input = island_input();
        let op = find("mesh.remove_small_components").unwrap();
        let out = op
            .execute(
                &input,
                &Target::WholeObject,
                &params(
                    op.id(),
                    &[("max_triangles", "5"), ("max_relative_size", "100")],
                ),
                &mf_operations::RunContext::none(),
            )
            .unwrap();
        let (mesh, prov) = out.change.unwrap();
        let kept: Vec<_> = mesh.faces().collect();
        let cube_faces: Vec<_> = input.faces().take(12).collect();
        assert_eq!(kept, cube_faces, "same IDs, same indices");
        assert_eq!(prov.removed_faces, (12..16).map(FaceId).collect::<Vec<_>>());
        assert!(prov.created_faces.is_empty());
    }

    #[test]
    fn noop_returns_no_change() {
        let op = find("mesh.remove_small_components").unwrap();
        let out = op
            .execute(
                &cube(1.0),
                &Target::WholeObject,
                &params(op.id(), &[]),
                &mf_operations::RunContext::none(),
            )
            .unwrap();
        assert!(out.change.is_none());
        assert_eq!(out.report["removed_components"], 0);
    }

    #[test]
    fn targeted_variants_rejected_until_m3() {
        let op = find("mesh.remove_small_components").unwrap();
        let err = op
            .execute(
                &cube(1.0),
                &Target::Faces(vec![FaceId(0)]),
                &params(op.id(), &[]),
                &mf_operations::RunContext::none(),
            )
            .err()
            .unwrap();
        assert!(
            matches!(
                err,
                OpError::Failed {
                    code: "unsupported_target",
                    ..
                }
            ),
            "{err:?}"
        );
    }
}
