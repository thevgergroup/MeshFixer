#![allow(clippy::unwrap_used)] // tests may panic on failure

//! mesh.fill_holes: targeted and whole-object filling (MF-OP-001/002/003).

use std::collections::BTreeMap;

use mf_fixtures::{cube_with_missing_face, grid, uv_sphere};
use mf_geometry::{FaceId, Mesh, VertexId, inspect};
use mf_operations::{OpError, Target, find};
use mf_spec::{Params, builtin};

fn params(pairs: &[(&str, &str)]) -> Params {
    let raw: BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    builtin::get("mesh.fill_holes")
        .unwrap()
        .validate(&raw)
        .unwrap()
}

fn fill(
    m: &Mesh,
    target: Target,
    pairs: &[(&str, &str)],
) -> Result<(Mesh, mf_geometry::Provenance, serde_json::Value), OpError> {
    let out = find("mesh.fill_holes")
        .unwrap()
        .execute(m, &target, &params(pairs))?;
    let (mesh, prov) = out.change.expect("fill_holes changed the mesh");
    Ok((mesh, prov, out.report))
}

const NO_REFINE: &[(&str, &str)] = &[("refine", "false")];

/// MF-OP-003: every input face keeps its ID and indices; every input vertex
/// keeps its position bit-for-bit.
fn assert_untouched_outside_target(before: &Mesh, after: &Mesh) {
    for (id, tri) in before.faces() {
        assert_eq!(after.face(id), Some(tri), "face {id:?} changed");
    }
    for (id, p) in before.vertices() {
        let q = after
            .position(id)
            .unwrap_or_else(|| panic!("vertex {id:?} removed"));
        assert_eq!(
            p.map(f64::to_bits),
            q.map(f64::to_bits),
            "vertex {id:?} moved"
        );
    }
}

/// 6×6 grid with two triangular holes far apart.
fn two_holes() -> Mesh {
    let n = 6;
    let cell = |i: u32, j: u32| 2 * (j * n + i);
    let mut e = grid(n).edit();
    e.remove_face(FaceId(cell(1, 1))).unwrap();
    e.remove_face(FaceId(cell(4, 4))).unwrap();
    e.finish().unwrap().0
}

fn loop_containing(m: &Mesh, v: u32) -> u32 {
    m.topology()
        .boundary_loops
        .iter()
        .find(|l| l.vertices.contains(&v) && l.edge_count == 3)
        .unwrap()
        .id
}

#[test]
fn filled_cube_is_closed_and_consistently_oriented() {
    let m = cube_with_missing_face();
    let (out, _, _) = fill(&m, Target::WholeObject, NO_REFINE).unwrap();
    let d = inspect(&out);
    assert!(d.is_closed && d.is_manifold, "{d:?}");
    assert_eq!(d.inconsistent_orientation_edges, 0);
    assert!((d.signed_volume.unwrap() - 1.0).abs() < 1e-12);
    assert_untouched_outside_target(&m, &out);
}

#[test]
fn fills_only_the_selected_loop() {
    let m = two_holes();
    let n = 6;
    let v11 = n + 2; // vertex (1,1) = 1·(n+1) + 1, on the first hole
    let target = loop_containing(&m, v11);
    let (out, _, report) = fill(&m, Target::Loops(vec![target]), NO_REFINE).unwrap();
    let small: Vec<usize> = out
        .topology()
        .boundary_loops
        .iter()
        .map(|l| l.edge_count)
        .filter(|&e| e == 3)
        .collect();
    assert_eq!(small, vec![3], "the other hole is still open");
    assert_eq!(report["filled_loops"][0]["id"], target);
    assert_untouched_outside_target(&m, &out);
}

#[test]
fn untouched_outside_target_on_a_sphere_hole() {
    let base = uv_sphere(24, 48);
    let mut e = base.edit();
    for f in 300..306 {
        e.remove_face(FaceId(f)).unwrap();
    }
    let m = e.finish().unwrap().0;
    let (out, _, _) = fill(&m, Target::WholeObject, NO_REFINE).unwrap();
    assert_untouched_outside_target(&m, &out);
    assert!(inspect(&out).is_closed);
}

#[test]
fn fill_one_pinched_hole_leaves_the_other() {
    let n = 4;
    let cell = |i: u32, j: u32| 2 * (j * n + i);
    let mut e = grid(n).edit();
    e.remove_face(FaceId(cell(1, 1))).unwrap();
    e.remove_face(FaceId(cell(2, 2) + 1)).unwrap();
    let m = e.finish().unwrap().0;
    let holes: Vec<u32> = m
        .topology()
        .boundary_loops
        .iter()
        .filter(|l| l.edge_count == 3)
        .map(|l| l.id)
        .collect();
    assert_eq!(holes.len(), 2);
    let (out, _, _) = fill(&m, Target::Loops(vec![holes[0]]), NO_REFINE).unwrap();
    let d = inspect(&out);
    assert_eq!(
        d.boundary_loops
            .iter()
            .filter(|l| l.edge_count == 3)
            .count(),
        1
    );
    assert_eq!(d.untraced_boundary_edges, 0);
    assert!(d.is_manifold);
}

#[test]
fn loop_too_large_fails_by_name() {
    let m = cube_with_missing_face();
    let id = m.topology().boundary_loops[0].id;
    let err = fill(
        &m,
        Target::Loops(vec![id]),
        &[("refine", "false"), ("max_loop_edges", "3")],
    )
    .unwrap_err();
    match err {
        OpError::Failed { code, message } => {
            assert_eq!(code, "loop_too_large");
            assert!(
                message.contains(&format!("loop {id}")) && message.contains("4 edges"),
                "{message}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn whole_object_reports_skipped_loops() {
    let m = two_holes(); // outer rim (24 edges) + two 3-edge holes
    let (out, _, report) = fill(
        &m,
        Target::WholeObject,
        &[("refine", "false"), ("max_loop_edges", "10")],
    )
    .unwrap();
    assert_eq!(report["filled_loops"].as_array().unwrap().len(), 2);
    assert_eq!(report["skipped_loops"][0]["edges"], 24);
    assert_eq!(report["skipped_loops"][0]["reason"], "too_large");
    assert_eq!(
        out.topology().boundary_loops.len(),
        1,
        "only the rim is left open"
    );
}

#[test]
fn degenerate_loops_fill_or_fail_by_name() {
    // A 3-edge hole fills with exactly one triangle.
    let one = two_holes();
    let target = one
        .topology()
        .boundary_loops
        .iter()
        .find(|l| l.edge_count == 3)
        .unwrap()
        .id;
    let (out, prov, _) = fill(&one, Target::Loops(vec![target]), NO_REFINE).unwrap();
    assert_eq!(prov.created_faces.len(), 1);
    assert_eq!(out.face_count(), one.face_count() + 1);
    // An unknown loop id fails by name.
    match fill(&one, Target::Loops(vec![99]), NO_REFINE).unwrap_err() {
        OpError::Failed { code, message } => {
            assert_eq!(code, "invalid_target");
            assert!(message.contains("loop 99"), "{message}");
        }
        other => panic!("{other:?}"),
    }
}

/// A slit: a 3-edge loop whose vertices are collinear. Two triangles sit
/// above edges 0–1 and 1–2, one below edge 0–2, so the only "fill" would
/// be a zero-area triangle.
fn collinear_slit() -> Mesh {
    Mesh::new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, -1.0, 0.0],
        ],
        vec![[0, 1, 3], [1, 2, 3], [2, 0, 4]],
    )
    .unwrap()
}

#[test]
fn collinear_loop_is_never_filled_with_a_zero_area_triangle() {
    let m = collinear_slit();
    let topo = m.topology();
    let slit = topo
        .boundary_loops
        .iter()
        .find(|l| l.edge_count == 3)
        .expect("the slit is a 3-edge loop")
        .id;
    for refine in ["false", "true"] {
        let p = [("refine", refine)];
        // Selected: fails by name.
        match fill(&m, Target::Loops(vec![slit]), &p).unwrap_err() {
            OpError::Failed { code, message } => {
                assert_eq!(code, "loop_untriangulable");
                assert!(message.contains(&format!("loop {slit}")), "{message}");
            }
            other => panic!("{other:?}"),
        }
        // Whole object: skipped and reported, and nothing degenerate added.
        let out = find("mesh.fill_holes")
            .unwrap()
            .execute(&m, &Target::WholeObject, &params(&p))
            .unwrap();
        let skipped = out.report["skipped_loops"].as_array().unwrap();
        assert!(
            skipped
                .iter()
                .any(|s| s["id"] == slit && s["reason"] == "untriangulable"),
            "{}",
            out.report
        );
        if let Some((after, _)) = out.change {
            assert_eq!(inspect(&after).degenerate_triangles, 0);
        }
    }
}

#[test]
fn provenance_names_the_loop() {
    let m = cube_with_missing_face();
    let id = m.topology().boundary_loops[0].id;
    let (_, prov, _) = fill(&m, Target::Loops(vec![id]), NO_REFINE).unwrap();
    assert_eq!(prov.created_faces.len(), 2);
    assert!(
        prov.created_faces
            .iter()
            .all(|(_, s)| s.loops == vec![id] && s.faces.is_empty())
    );
    assert!(prov.removed_faces.is_empty() && prov.moved_vertices.is_empty());
}

#[test]
fn other_targets_are_rejected() {
    let m = cube_with_missing_face();
    let err = fill(&m, Target::Components(vec![0]), NO_REFINE).unwrap_err();
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
    let _ = VertexId(0);
}

mod refinement {
    use super::*;

    /// A fine sphere with a round hole of about 64 boundary edges.
    fn round_hole() -> Mesh {
        let base = uv_sphere(80, 160);
        let axis = [1.0, 0.0, 0.0];
        let cos_r = 0.18f64.cos();
        let mut e = base.edit();
        for (id, t) in base.faces() {
            let c =
                [0, 1, 2].map(|k| t.iter().map(|&v| base.corner_position(v)[k]).sum::<f64>() / 3.0);
            let len = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
            if (c[0] * axis[0] + c[1] * axis[1] + c[2] * axis[2]) / len > cos_r {
                e.remove_face(id).unwrap();
            }
        }
        e.finish().unwrap().0
    }

    fn len(a: [f64; 3], b: [f64; 3]) -> f64 {
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    }

    fn min_angle_deg(m: &Mesh, t: [u32; 3]) -> f64 {
        let p = t.map(|v| m.corner_position(v));
        (0..3)
            .map(|k| {
                let (a, b, c) = (p[k], p[(k + 1) % 3], p[(k + 2) % 3]);
                let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                let d = (u[0] * v[0] + u[1] * v[1] + u[2] * v[2]) / (len(a, b) * len(a, c));
                d.clamp(-1.0, 1.0).acos().to_degrees()
            })
            .fold(180.0, f64::min)
    }

    #[test]
    fn refined_patch_matches_boundary_density() {
        let m = round_hole();
        let hole = m
            .topology()
            .boundary_loops
            .iter()
            .find(|l| l.edge_count > 20)
            .unwrap()
            .clone();
        assert!(
            (40..=120).contains(&hole.edge_count),
            "hole has {} edges",
            hole.edge_count
        );
        let (out, prov, _) = fill(&m, Target::Loops(vec![hole.id]), &[("refine", "true")]).unwrap();
        assert!(
            !prov.created_vertices.is_empty(),
            "refinement added vertices"
        );
        let n = hole.vertices.len();
        let boundary_mean: f64 = (0..n)
            .map(|k| {
                len(
                    m.corner_position(hole.vertices[k]),
                    m.corner_position(hole.vertices[(k + 1) % n]),
                )
            })
            .sum::<f64>()
            / n as f64;
        let boundary: std::collections::BTreeSet<(u32, u32)> = (0..n)
            .map(|k| {
                let (a, b) = (hole.vertices[k], hole.vertices[(k + 1) % n]);
                (a.min(b), a.max(b))
            })
            .collect();
        let mut interior = std::collections::BTreeSet::new();
        for (f, _) in &prov.created_faces {
            let t = out.face(*f).unwrap();
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                let e = (a.min(b), a.max(b));
                if !boundary.contains(&e) {
                    interior.insert(e);
                }
            }
            let angle = min_angle_deg(&out, t);
            assert!(angle >= 5.0, "patch triangle {f:?} has a {angle:.2}° angle");
        }
        let patch_mean: f64 = interior
            .iter()
            .map(|&(a, b)| len(out.corner_position(a), out.corner_position(b)))
            .sum::<f64>()
            / interior.len() as f64;
        let ratio = patch_mean / boundary_mean;
        let worst_angle = prov
            .created_faces
            .iter()
            .map(|(f, _)| min_angle_deg(&out, out.face(*f).unwrap()))
            .fold(180.0, f64::min);
        eprintln!(
            "hole {} edges: {} patch faces, {} new vertices, edge ratio {ratio:.3}, min angle {worst_angle:.1}°",
            hole.edge_count,
            prov.created_faces.len(),
            prov.created_vertices.len()
        );
        assert!(
            (0.75..=1.25).contains(&ratio),
            "patch/boundary edge length ratio {ratio:.3}"
        );
        let d = inspect(&out);
        assert!(
            d.is_manifold && d.inconsistent_orientation_edges == 0,
            "{d:?}"
        );
        assert_untouched_outside_target(&m, &out);
    }

    #[test]
    fn refine_false_gives_the_bare_triangulation() {
        let m = round_hole();
        let (_, prov, _) = fill(
            &m,
            Target::WholeObject,
            &[("refine", "false"), ("max_loop_edges", "200")],
        )
        .unwrap();
        assert!(prov.created_vertices.is_empty());
    }

    #[test]
    fn refinement_is_deterministic_at_1_and_8_threads() {
        let m = round_hole();
        let run = |threads| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            pool.install(|| {
                fill(&m, Target::WholeObject, &[("max_loop_edges", "200")])
                    .unwrap()
                    .0
                    .content_hash()
            })
        };
        assert_eq!(run(1), run(8));
    }
}
