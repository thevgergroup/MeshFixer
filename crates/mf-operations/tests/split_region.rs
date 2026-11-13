#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Dovetail joints, split by region and delete faces (Phase 1 Task 14).

use std::collections::BTreeMap;

use mf_document::ObjectId;
use mf_fixtures::{cube, scaled, uv_sphere};
use mf_geometry::affine::to_dmat4;
use mf_geometry::{Bvh, FaceId, Mesh, inspect, intersections_between};
use mf_io::threemf::IDENTITY;
use mf_operations::{DocOutput, ObjectChange, OpError, OperandInput, RunContext, Target, find};

fn run(
    op: &str,
    mesh: &Mesh,
    target: Target,
    params: &[(&str, &str)],
) -> Result<DocOutput, OpError> {
    let spec = mf_spec::builtin::get(op).unwrap();
    let raw: BTreeMap<String, String> = params
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    find(op).unwrap().execute_doc(
        &[OperandInput {
            slot: "target",
            object: ObjectId(1),
            name: "scan",
            mesh,
            transform: IDENTITY,
            target,
        }],
        &spec.validate(&raw).unwrap(),
        &RunContext::none(),
    )
}

fn parts(out: &DocOutput) -> (Mesh, Mesh) {
    let mut kept = None;
    let mut made = None;
    for c in &out.changes {
        match c {
            ObjectChange::Geometry {
                operand: 0, mesh, ..
            } => kept = Some(mesh.clone()),
            ObjectChange::Create { mesh, .. } => made = Some(mesh.clone()),
            _ => {}
        }
    }
    (kept.unwrap(), made.unwrap())
}

fn closed(m: &Mesh, what: &str) {
    let d = inspect(m);
    assert!(
        d.is_closed && d.is_manifold && d.inconsistent_orientation_edges == 0,
        "{what}: {d:?}"
    );
    assert!(d.signed_volume.unwrap() > 0.0, "{what}");
}

/// Crossing face pairs between `a` and `b` moved by `d`.
fn crossings(a: &Mesh, b: &Mesh, d: [f64; 3]) -> u64 {
    let ctx = RunContext::none();
    let mut t = IDENTITY;
    t[9..].copy_from_slice(&d);
    intersections_between(
        a,
        &Bvh::build(a, &ctx).unwrap(),
        b,
        &Bvh::build(b, &ctx).unwrap(),
        &to_dmat4(&t),
        &ctx,
    )
    .unwrap()
    .pair_count
}

#[test]
fn dovetail_parts_slide_together_with_clearance() {
    // A 60 × 30 × 40 block: its section's principal axis is x.
    let (p, t) = cube(1.0).to_arrays();
    let block = Mesh::new(
        p.iter()
            .map(|q| [q[0] * 60.0, q[1] * 30.0, q[2] * 40.0])
            .collect(),
        t,
    )
    .unwrap();
    let out = run(
        "mesh.split_plane",
        &block,
        Target::WholeObject,
        &[
            ("pegs", "dovetail"),
            ("plane_point", "0,0,20.37"),
            ("hole_depth_below", "6"),
        ],
    )
    .unwrap();
    assert_eq!(out.report["pegs"]["dovetail_angle_degrees"], 15.0);
    let (top, bottom) = parts(&out);
    closed(&top, "top");
    closed(&bottom, "bottom");
    // The tail hangs below the cut, into the groove.
    assert!(top.bounding_box().unwrap().min[2] < 20.37 - 5.0);
    // Lifted off the cut (less than the length allowance), the tail clears
    // the groove everywhere; it still clears sliding along x...
    let up = 0.01;
    assert_eq!(crossings(&bottom, &top, [0.0, 0.0, up]), 0);
    assert_eq!(crossings(&bottom, &top, [7.0, 0.0, up]), 0);
    // ...and across the joint it has half the clearance (0.1 mm) each
    // side, and no more.
    assert_eq!(crossings(&bottom, &top, [0.0, 0.08, up]), 0);
    assert!(crossings(&bottom, &top, [0.0, 0.12, up]) > 0);
    assert!(crossings(&bottom, &top, [0.0, -0.12, up]) > 0);
}

#[test]
fn split_region_gives_two_closed_parts_and_keeps_faces() {
    let sphere = scaled(&uv_sphere(48, 96), 30.0);
    let selected: Vec<FaceId> = sphere
        .faces()
        .filter(|(_, t)| t.iter().all(|&v| sphere.corner_position(v)[2] > 20.0))
        .map(|(f, _)| f)
        .collect();
    for style in ["smooth", "flat"] {
        let out = run(
            "mesh.split_region",
            &sphere,
            Target::Faces(selected.clone()),
            &[("cap_style", style)],
        )
        .unwrap();
        let (kept, piece) = parts(&out);
        closed(&kept, "kept");
        closed(&piece, "piece");
        // Faces away from the cut keep their ids, bit for bit.
        for (f, t) in sphere.faces() {
            if t.iter().all(|&v| sphere.corner_position(v)[2] < 15.0) {
                assert_eq!(kept.face(f), Some(t));
            }
        }
        // A flat cap stays at the cut; a smooth one continues the curve.
        if style == "flat" {
            assert!(piece.bounding_box().unwrap().min[2] > 19.0, "{style}");
        }
    }
}

#[test]
fn region_boundary_over_1000_edges_refuses_citing_issue_043() {
    let sphere = uv_sphere(8, 1100);
    let selected: Vec<FaceId> = sphere
        .faces()
        .filter(|(_, t)| t.iter().all(|&v| sphere.corner_position(v)[2] > 0.5))
        .map(|(f, _)| f)
        .collect();
    match run("mesh.split_region", &sphere, Target::Faces(selected), &[]) {
        Err(OpError::Failed { code, message }) => {
            assert_eq!(code, "region_boundary_too_long");
            assert!(message.contains("MF-ISSUE-043"), "{message}");
        }
        other => panic!("{:?}", other.map(|o| o.report)),
    }
}

#[test]
fn delete_faces_leaves_an_open_boundary_listed_as_a_hole() {
    let c = cube(1.0);
    let out = run(
        "mesh.delete_faces",
        &c,
        Target::Faces(vec![FaceId(2), FaceId(3)]),
        &[],
    )
    .unwrap();
    let ObjectChange::Geometry {
        mesh, provenance, ..
    } = &out.changes[0]
    else {
        panic!()
    };
    assert_eq!(provenance.removed_faces, vec![FaceId(2), FaceId(3)]);
    let d = inspect(mesh);
    assert_eq!(d.boundary_loops.len(), 1);
    assert_eq!(d.boundary_loops[0].edge_count, 4);
    // Fill it again: closed.
    let filled = find("mesh.fill_holes")
        .unwrap()
        .execute(
            mesh,
            &Target::WholeObject,
            &mf_spec::builtin::get("mesh.fill_holes")
                .unwrap()
                .validate(&BTreeMap::new())
                .unwrap(),
            &RunContext::none(),
        )
        .unwrap();
    assert!(inspect(&filled.change.unwrap().0).is_closed);
}
