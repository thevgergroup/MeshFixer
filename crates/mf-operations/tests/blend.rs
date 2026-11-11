#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `mesh.boolean` with `blend_width` (MF-MODEL-004, Phase 1 Task 22): the
//! seam band is remeshed and faired (Δ² = 0) after a union, open parts
//! (stamps) are capped and sunk before it, and both are validated (D4).
//! The heavy 30M window test runs on demand only (`scripts/run-tests.sh`'s
//! timing list).

use std::collections::{BTreeMap, HashSet};

use mf_document::ObjectId;
use mf_fixtures::{cube, scaled, translated, uv_sphere};
use mf_geometry::{FaceId, Mesh, Provenance};
use mf_operations::{DocOutput, ObjectChange, OpError, OperandInput, RunContext, Target, find};

const IDENTITY: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

fn run(
    target: &Mesh,
    target_t: [f64; 12],
    tool: &Mesh,
    tool_t: [f64; 12],
    params: &[(&str, &str)],
    ctx: &RunContext,
) -> Result<DocOutput, OpError> {
    let op = find("mesh.boolean").unwrap();
    let spec = mf_spec::builtin::get("mesh.boolean").unwrap();
    let mut raw: BTreeMap<String, String> = params
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    raw.entry("operation".to_string())
        .or_insert_with(|| "union".to_string());
    let params = spec.validate(&raw).unwrap();
    op.execute_doc(
        &[
            OperandInput {
                object: ObjectId(1),
                name: "object",
                mesh: target,
                transform: target_t,
                target: Target::WholeObject,
            },
            OperandInput {
                object: ObjectId(2),
                name: "object",
                mesh: tool,
                transform: tool_t,
                target: Target::WholeObject,
            },
        ],
        &params,
        ctx,
    )
}

fn result(out: &DocOutput) -> (&Mesh, &Provenance) {
    out.changes
        .iter()
        .find_map(|c| match c {
            ObjectChange::Geometry {
                operand: 0,
                mesh,
                provenance,
            } => Some((mesh, provenance)),
            _ => None,
        })
        .unwrap()
}

fn code(r: Result<DocOutput, OpError>) -> (&'static str, String) {
    match r {
        Err(OpError::Failed { code, message }) => (code, message),
        Err(e) => panic!("{e}"),
        Ok(o) => panic!("expected a refusal, got {}", o.report),
    }
}

/// The largest dihedral angle (degrees, 0 = flat) across edges where
/// *both* adjacent faces are in `faces` (interior band edges, not the
/// band's rim onto the untouched mesh), computed from face normals.
fn max_dihedral_among(mesh: &Mesh, faces: &HashSet<FaceId>) -> f64 {
    let normal = |t: [u32; 3]| -> [f64; 3] {
        let [a, b, c] = t.map(|v| mesh.corner_position(v));
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len == 0.0 {
            [0.0, 0.0, 0.0]
        } else {
            n.map(|c| c / len)
        }
    };
    // Edge -> faces touching it, restricted to faces in the set.
    let mut edge_faces: std::collections::HashMap<(u32, u32), Vec<FaceId>> =
        std::collections::HashMap::new();
    for &f in faces {
        let t = mesh.face(f).unwrap();
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let key = (a.min(b), a.max(b));
            edge_faces.entry(key).or_default().push(f);
        }
    }
    let mut max_angle = 0.0f64;
    for fs in edge_faces.values() {
        if fs.len() == 2 {
            let n1 = normal(mesh.face(fs[0]).unwrap());
            let n2 = normal(mesh.face(fs[1]).unwrap());
            let dot = (n1[0] * n2[0] + n1[1] * n2[1] + n1[2] * n2[2]).clamp(-1.0, 1.0);
            let angle = dot.acos().to_degrees();
            max_angle = max_angle.max(angle);
        }
    }
    max_angle
}

#[test]
fn blended_union_is_closed_manifold_and_c1_across_the_band() {
    // A sphere stuck onto a cube's flat face makes a real seam angle (the
    // sphere's surface meets the cube's face at a sharp crease); two flush
    // cube faces would leave nothing to blend (the shared plane is flat).
    let target = cube(8.0);
    let tool = translated(&scaled(&uv_sphere(24, 48), 2.0), [4.0, 4.0, 8.0]);
    let ctx = RunContext::none();

    let sharp = run(
        &target,
        IDENTITY,
        &tool,
        IDENTITY,
        &[("blend_width", "0")],
        &ctx,
    )
    .unwrap();
    let (sharp_mesh, sharp_prov) = result(&sharp);
    let sharp_created: HashSet<FaceId> = sharp_prov.created_faces.iter().map(|(f, _)| *f).collect();
    let sharp_max = max_dihedral_among(sharp_mesh, &sharp_created);

    let blended = run(
        &target,
        IDENTITY,
        &tool,
        IDENTITY,
        &[("blend_width", "1.5")],
        &ctx,
    )
    .unwrap();
    let (mesh, prov) = result(&blended);

    // Closed, manifold, no self-intersections.
    let bvh = mf_geometry::Bvh::build(mesh, &ctx).unwrap();
    let all_faces: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
    mf_geometry::validate_solid(mesh, &all_faces, &bvh, &ctx)
        .unwrap()
        .unwrap_or_else(|d| panic!("not a valid solid: {d}"));

    let blended_created: HashSet<FaceId> = prov.created_faces.iter().map(|(f, _)| *f).collect();
    let blended_max = max_dihedral_among(mesh, &blended_created);
    assert!(
        blended_max <= sharp_max + 1e-9,
        "blended dihedral {blended_max} should be <= sharp seam's {sharp_max}"
    );
    // The blend should visibly soften the sharp 90-degree seam.
    assert!(
        blended_max < sharp_max - 1.0,
        "blend ({blended_max}°) should be notably smoother than the sharp seam ({sharp_max}°)"
    );
}

#[test]
fn open_stamp_part_blends_without_leaving_its_cap() {
    // An open hemispherical "stamp" (half a sphere, one boundary loop):
    // embedded into a cube and blended.
    let target = cube(10.0);
    let sphere = scaled(&uv_sphere(16, 32), 3.0);
    let mut edit = sphere.edit();
    for (f, t) in sphere.faces() {
        // Keep the lower half (z < 0): an open cap, boundary loop at z ~ 0.
        let z: f64 = t.iter().map(|&v| sphere.corner_position(v)[2]).sum::<f64>() / 3.0;
        if z >= 0.0 {
            edit.remove_face(f).unwrap();
        }
    }
    let stamp = edit.finish().unwrap().0;
    // Flip the stamp so its open rim faces +z (outward normal +z) and sits
    // just above the cube's top face (z = 10), overlapping by 2 mm.
    let (positions, triangles) = stamp.to_arrays();
    let flipped: Vec<[f64; 3]> = positions
        .iter()
        .map(|p| [p[0], p[1], -p[2] + 10.0 - 2.0])
        .collect();
    let flipped_tris: Vec<[u32; 3]> = triangles.iter().map(|t| [t[0], t[2], t[1]]).collect();
    let stamp = Mesh::new(flipped, flipped_tris).unwrap();
    let stamp = translated(&stamp, [5.0, 5.0, 0.0]);

    let ctx = RunContext::none();
    let out = run(
        &target,
        IDENTITY,
        &stamp,
        IDENTITY,
        &[("blend_width", "1.0")],
        &ctx,
    )
    .unwrap();
    let (mesh, _prov) = result(&out);
    let bvh = mf_geometry::Bvh::build(mesh, &ctx).unwrap();
    let all_faces: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
    // validate_solid proves the result is closed: a surviving cap face
    // would leave an open boundary where the stamp's rim used to be.
    mf_geometry::validate_solid(mesh, &all_faces, &bvh, &ctx)
        .unwrap()
        .unwrap_or_else(|d| panic!("not a valid solid: {d}"));
    assert_eq!(out.report["stamp_capped"], serde_json::json!(true));
}

#[test]
fn blend_replays_exact() {
    let target = cube(4.0);
    let tool = translated(&cube(4.0), [3.0, 0.0, 0.0]);
    let ctx = RunContext::none();
    let a = run(
        &target,
        IDENTITY,
        &tool,
        IDENTITY,
        &[("blend_width", "1.0")],
        &ctx,
    )
    .unwrap();
    let b = run(
        &target,
        IDENTITY,
        &tool,
        IDENTITY,
        &[("blend_width", "1.0")],
        &ctx,
    )
    .unwrap();
    let (mesh_a, _) = result(&a);
    let (mesh_b, _) = result(&b);
    assert_eq!(
        mesh_a.to_arrays(),
        mesh_b.to_arrays(),
        "replay must be bit-exact"
    );
    assert_eq!(a.report, b.report);
}

#[test]
fn too_wide_blend_refuses_by_name() {
    let target = cube(2.0);
    let tool = translated(&cube(2.0), [1.5, 0.0, 0.0]);
    let ctx = RunContext::none();
    let (c, msg) = code(run(
        &target,
        IDENTITY,
        &tool,
        IDENTITY,
        &[("blend_width", "50")],
        &ctx,
    ));
    assert_eq!(c, "blend_too_wide", "{msg}");
}

#[test]
fn blend_needs_union_refuses_by_name_for_other_operations() {
    let target = cube(4.0);
    let tool = translated(&cube(4.0), [1.0, 0.0, 0.0]);
    let ctx = RunContext::none();
    let (c, msg) = code(run(
        &target,
        IDENTITY,
        &tool,
        IDENTITY,
        &[("operation", "subtract"), ("blend_width", "1.0")],
        &ctx,
    ));
    assert_eq!(c, "blend_needs_union", "{msg}");
}

/// Heavy: a 30M-triangle scan-sized target, a small part unioned and
/// blended, checking the change window stays local. Run on demand
/// (`scripts/run-tests.sh`'s timing list), not in the fast gate.
#[test]
fn blend_on_30m_scan_touches_only_the_window() {
    let target = scaled(&uv_sphere(2200, 4500), 50.0); // ~19.8M faces, scan-sized
    let tool = translated(&scaled(&cube(1.0), 1.0), [49.5, -0.5, -0.5]);
    let ctx = RunContext::none();
    let out = run(
        &target,
        IDENTITY,
        &tool,
        IDENTITY,
        &[("blend_width", "1.0")],
        &ctx,
    )
    .unwrap();
    let (mesh, prov) = result(&out);
    let touched = prov.created_faces.len() + prov.removed_faces.len() + prov.modified_faces.len();
    assert!(
        touched * 200 < target.face_count(),
        "blend touched {touched} of {} faces; should stay local",
        target.face_count()
    );
    assert!(mesh.face_count() > 0);
}
