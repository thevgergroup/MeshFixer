#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `mesh.split_plane` (Phase 1 Task 12, MF-MODEL-003, D11, D23): the exact
//! plane cut with caps, run through `execute_doc` as a command runs it.
//! Replay and cancellation through the command layer are in
//! `mf-command/tests/split.rs`.

use std::collections::{BTreeMap, HashSet};

use mf_document::ObjectId;
use mf_fixtures::{cube, merge, rotation_translation, scaled, translated, uv_sphere};
use mf_geometry::{FaceId, Mesh};
use mf_io::threemf::IDENTITY;
use mf_operations::{DocOutput, ObjectChange, OpError, OperandInput, RunContext, Target, find};

fn split_with(
    mesh: &Mesh,
    transform: [f64; 12],
    params: &[(&str, &str)],
) -> Result<DocOutput, OpError> {
    let spec = mf_spec::builtin::get("mesh.split_plane").unwrap();
    let raw: BTreeMap<String, String> = params
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let params = spec.validate(&raw).unwrap();
    find("mesh.split_plane").unwrap().execute_doc(
        &[OperandInput {
            object: ObjectId(1),
            name: "scan",
            mesh,
            transform,
            target: Target::WholeObject,
        }],
        &params,
        &RunContext::none(),
    )
}

fn v3(p: [f64; 3]) -> String {
    format!("{},{},{}", p[0], p[1], p[2])
}

/// `(top, bottom)`: the target's new geometry and the created part.
fn parts(out: &DocOutput) -> (Mesh, Option<(String, Mesh, [f64; 12])>) {
    let mut top = None;
    let mut bottom = None;
    for c in &out.changes {
        match c {
            ObjectChange::Geometry {
                operand: 0, mesh, ..
            } => top = Some(mesh.clone()),
            ObjectChange::Create {
                name,
                mesh,
                transform,
                ..
            } => bottom = Some((name.clone(), mesh.clone(), *transform)),
            ObjectChange::Rename { .. } => {}
            other => panic!("unexpected change {other:?}"),
        }
    }
    (top.unwrap(), bottom)
}

fn volume(mesh: &Mesh) -> f64 {
    mesh.faces()
        .map(|(_, t)| {
            let [a, b, c] = t.map(|v| mesh.corner_position(v));
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum()
}

/// Closed, manifold, consistently oriented, no degenerate faces and no
/// self-intersections (exact; mf-boolean's independent check).
fn assert_solid(mesh: &Mesh, what: &str) {
    let (p, t) = mesh.to_arrays();
    mf_boolean::validate_solid(&p, &t, &RunContext::none())
        .unwrap_or_else(|e| panic!("{what}: {e}"));
    assert!(volume(mesh) > 0.0, "{what}: volume {}", volume(mesh));
}

fn close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * a.abs().max(b.abs())
}

fn code(r: Result<DocOutput, OpError>) -> &'static str {
    match r {
        Err(OpError::Failed { code, .. }) => code,
        other => panic!("expected a refusal, got {:?}", other.map(|o| o.report)),
    }
}

#[test]
fn cut_cube_gives_two_closed_parts_with_exact_volumes() {
    let mesh = cube(2.0);
    let out = split_with(&mesh, IDENTITY, &[("plane_point", "0,0,0.7")]).unwrap();
    let (top, bottom) = parts(&out);
    let (name, bottom, transform) = bottom.unwrap();
    assert_eq!(name, "scan (bottom)");
    assert_eq!(transform, IDENTITY);
    assert!(
        out.changes.iter().any(
            |c| matches!(c, ObjectChange::Rename { operand: 0, name } if name == "scan (top)")
        )
    );
    assert_solid(&top, "top");
    assert_solid(&bottom, "bottom");
    assert!(close(volume(&top), 4.0 * 1.3, 1e-12), "{}", volume(&top));
    assert!(
        close(volume(&bottom), 4.0 * 0.7, 1e-12),
        "{}",
        volume(&bottom)
    );
    // The report lists each part's cap faces, which lie in the plane.
    for (k, part) in [&top, &bottom].into_iter().enumerate() {
        let caps = out.report["parts"][k]["cap_faces"].as_array().unwrap();
        assert!(!caps.is_empty());
        for f in caps {
            let tri = part.face(FaceId(f.as_u64().unwrap() as u32)).unwrap();
            assert!(tri.iter().all(|&v| part.corner_position(v)[2] == 0.7));
        }
    }
}

/// An octahedron cut through its equator, and a cube cut diagonally
/// through four of its corners: a vertex exactly on the plane is used by
/// the faces either side and the cap, never doubled by a cut point.
#[test]
fn vertex_exactly_on_plane_is_shared_not_duplicated() {
    let octa = Mesh::new(
        vec![
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
        ],
        vec![
            [0, 1, 4],
            [1, 2, 4],
            [2, 3, 4],
            [3, 0, 4],
            [1, 0, 5],
            [2, 1, 5],
            [3, 2, 5],
            [0, 3, 5],
        ],
    )
    .unwrap();
    let cube2 = cube(2.0);
    for (mesh, point, normal, vol) in [
        (&octa, [0.0, 0.0, 0.0], [0.0, 0.0, 1.0], 2.0 / 3.0),
        (&cube2, [2.0, 0.0, 0.0], [1.0, 1.0, 0.0], 4.0),
    ] {
        let out = split_with(
            mesh,
            IDENTITY,
            &[("plane_point", &v3(point)), ("plane_normal", &v3(normal))],
        )
        .unwrap();
        let (top, bottom) = parts(&out);
        let bottom = bottom.unwrap().1;
        for part in [&top, &bottom] {
            assert_solid(part, "part");
            assert!(close(volume(part), vol, 1e-12), "{}", volume(part));
            let mut seen = HashSet::new();
            for (_, p) in part.vertices() {
                assert!(seen.insert(p.map(f64::to_bits)), "{p:?} is doubled");
            }
        }
    }
    // The octahedron's equator is all on the plane: nothing is created.
    let out = split_with(&octa, IDENTITY, &[("plane_point", "0,0,0")]).unwrap();
    for c in &out.changes {
        if let ObjectChange::Geometry { provenance, .. } = c {
            assert!(provenance.created_vertices.is_empty());
        }
    }
}

/// A hollow sphere: the section is an annulus, capped with its hole.
#[test]
fn nested_sections_cap_with_holes() {
    let outer = scaled(&uv_sphere(24, 48), 2.0);
    let inner = {
        let (p, t) = uv_sphere(16, 32).to_arrays();
        Mesh::new(p, t.into_iter().map(|[a, b, c]| [a, c, b]).collect()).unwrap()
    };
    let hollow = merge(&[outer, inner]);
    let before = volume(&hollow);
    let out = split_with(&hollow, IDENTITY, &[("plane_point", "0,0,0.1")]).unwrap();
    assert_eq!(out.report["section_regions"], 1);
    let (top, bottom) = parts(&out);
    let bottom = bottom.unwrap().1;
    assert_solid(&top, "top");
    assert_solid(&bottom, "bottom");
    assert!(close(volume(&top) + volume(&bottom), before, 1e-9));
    // The region has the inner circle as its hole.
    let plane = mf_boolean::Plane {
        point: [0.0, 0.0, 0.1],
        normal: [0.0, 0.0, 1.0],
    };
    let half = mf_boolean::cut_half(
        &hollow,
        &plane,
        mf_boolean::Side::Below,
        &RunContext::none(),
    )
    .unwrap();
    assert_eq!(half.section_regions.len(), 1);
    let region = &half.section_regions[0];
    assert_eq!(region.holes.len(), 1);
    // Every vertical edge and every quad's diagonal is cut.
    assert_eq!(region.outer.len(), 96);
    assert_eq!(region.holes[0].len(), 64);
}

#[test]
fn horizontal_vertical_and_bias_cuts_all_give_closed_parts() {
    let sphere = translated(&uv_sphere(24, 48), [0.1, -0.2, 0.3]);
    let before = volume(&sphere);
    let (s, c) = 20_f64.to_radians().sin_cos();
    let r = std::f64::consts::FRAC_1_SQRT_2;
    let bias = [c * r, s * r, r];
    // World placements: identity, and a turned, moved one (the plane is
    // given in world space).
    let turned = rotation_translation(1, 35.0, [5.0, -3.0, 2.0]);
    for transform in [IDENTITY, turned] {
        for normal in [[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], bias] {
            // The sphere's centre, in world space.
            let centre = {
                let p = [0.1, -0.2, 0.3];
                let t = transform;
                [
                    t[0] * p[0] + t[3] * p[1] + t[6] * p[2] + t[9] + 0.05,
                    t[1] * p[0] + t[4] * p[1] + t[7] * p[2] + t[10],
                    t[2] * p[0] + t[5] * p[1] + t[8] * p[2] + t[11] - 0.05,
                ]
            };
            let out = split_with(
                &sphere,
                transform,
                &[("plane_point", &v3(centre)), ("plane_normal", &v3(normal))],
            )
            .unwrap();
            let (top, bottom) = parts(&out);
            let (_, bottom, t) = bottom.unwrap();
            assert_eq!(t, transform);
            assert_solid(&top, "top");
            assert_solid(&bottom, "bottom");
            assert!(close(volume(&top) + volume(&bottom), before, 1e-9));
            // In world space the top part is on the normal's side.
            let world = |p: [f64; 3]| -> [f64; 3] {
                std::array::from_fn(|i| {
                    transform[i] * p[0]
                        + transform[3 + i] * p[1]
                        + transform[6 + i] * p[2]
                        + transform[9 + i]
                })
            };
            let side = |p: [f64; 3]| {
                let w = world(p);
                (0..3).map(|i| (w[i] - centre[i]) * normal[i]).sum::<f64>()
            };
            assert!(top.vertices().all(|(_, p)| side(p) > -1e-12));
            assert!(bottom.vertices().all(|(_, p)| side(p) < 1e-12));
        }
    }
}

#[test]
fn cut_gap_leaves_exactly_that_gap_between_parts() {
    let sphere = scaled(&uv_sphere(24, 48), 10.0);
    let (s, c) = 20_f64.to_radians().sin_cos();
    let r = std::f64::consts::FRAC_1_SQRT_2;
    for (mesh, normal) in [
        (cube(10.0), [0.0, 0.0, 1.0]),
        (sphere.clone(), [0.0, 0.0, 1.0]),
        (sphere, [c * r, s * r, r]),
    ] {
        let point = [5.0, 5.0, 5.0];
        let gap = 0.5;
        let out = split_with(
            &mesh,
            IDENTITY,
            &[
                ("plane_point", &v3(point)),
                ("plane_normal", &v3(normal)),
                ("cut_gap", "0.5"),
            ],
        )
        .unwrap();
        let (top, bottom) = parts(&out);
        let bottom = bottom.unwrap().1;
        assert_solid(&top, "top");
        assert_solid(&bottom, "bottom");
        let along = |p: [f64; 3]| (0..3).map(|i| (p[i] - point[i]) * normal[i]).sum::<f64>();
        let top_min = top
            .vertices()
            .map(|(_, p)| along(p))
            .fold(f64::MAX, f64::min);
        let bottom_max = bottom
            .vertices()
            .map(|(_, p)| along(p))
            .fold(f64::MIN, f64::max);
        assert!(
            close(top_min - bottom_max, gap, 1e-9),
            "{normal:?}: gap {}",
            top_min - bottom_max
        );
    }
}

#[test]
fn faces_away_from_the_plane_are_kept_bitwise() {
    let sphere = uv_sphere(24, 48);
    let out = split_with(
        &sphere,
        IDENTITY,
        &[("plane_point", "0,0,0.13"), ("plane_normal", "0.2,0.1,1")],
    )
    .unwrap();
    let (top, bottom) = parts(&out);
    let bottom = bottom.unwrap().1;
    let side = |p: [f64; 3]| 0.2 * p[0] + 0.1 * p[1] + (p[2] - 0.13);
    let (mut above, mut below) = (0, 0);
    for (f, tri) in sphere.faces() {
        let s = tri.map(|v| side(sphere.corner_position(v)));
        let part = if s.iter().all(|&x| x > 1e-9) {
            above += 1;
            &top
        } else if s.iter().all(|&x| x < -1e-9) {
            below += 1;
            &bottom
        } else {
            continue;
        };
        assert_eq!(part.face(f), Some(tri), "face {f:?}");
        for v in tri {
            assert_eq!(
                part.corner_position(v).map(f64::to_bits),
                sphere.corner_position(v).map(f64::to_bits)
            );
        }
    }
    assert!(above > 100 && below > 100);
}

#[test]
fn refusals_are_named() {
    let open = mf_fixtures::cube_with_missing_face();
    assert_eq!(
        code(split_with(&open, IDENTITY, &[("plane_point", "0,0,0.5")])),
        "operand_not_closed"
    );
    let c = cube(1.0);
    assert_eq!(
        code(split_with(&c, IDENTITY, &[("plane_point", "0,0,1")])),
        "plane_misses_object"
    );
    assert_eq!(
        code(split_with(
            &c,
            IDENTITY,
            &[("plane_point", "0,0,0.5"), ("plane_normal", "0,0,0")]
        )),
        "invalid_plane"
    );
    // HOOK (Task 13): pegs need the boolean engine.
    assert_eq!(
        code(split_with(
            &c,
            IDENTITY,
            &[("plane_point", "0,0,0.5"), ("pegs", "dowels")]
        )),
        "pegs_need_booleans"
    );
    // Two cubes touching along an edge, cut through that edge: the two
    // section squares touch at a point, which is refused, not guessed.
    let touching = merge(&[cube(1.0), translated(&cube(1.0), [1.0, 1.0, 0.0])]);
    assert_eq!(
        code(split_with(
            &touching,
            IDENTITY,
            &[("plane_point", "0,0,0.5")]
        )),
        "section_self_intersects"
    );
}

#[test]
fn keep_one_side_lists_the_other_sides_faces_as_removed() {
    let c = cube(2.0);
    let out = split_with(
        &c,
        IDENTITY,
        &[("plane_point", "0,0,0.5"), ("keep", "below")],
    )
    .unwrap();
    let mut names = Vec::new();
    for ch in &out.changes {
        match ch {
            ObjectChange::Geometry {
                mesh, provenance, ..
            } => {
                assert_solid(mesh, "kept");
                assert!(close(volume(mesh), 2.0, 1e-12));
                // The top face's two triangles are gone.
                assert!(provenance.removed_faces.contains(&FaceId(2)));
                assert!(provenance.removed_faces.contains(&FaceId(3)));
            }
            ObjectChange::Rename { name, .. } => names.push(name.clone()),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(names, ["scan (bottom)"]);
}

/// Every face of the created part is either a face of the target with
/// the same id and corners, or created with sources in operand 0.
#[test]
fn created_part_provenance_names_operand_faces() {
    let sphere = uv_sphere(12, 24);
    let out = split_with(&sphere, IDENTITY, &[("plane_point", "0,0,0.2")]).unwrap();
    for ch in &out.changes {
        if let ObjectChange::Create {
            mesh, provenance, ..
        } = ch
        {
            let created: HashSet<FaceId> = provenance.created_faces.iter().map(|c| c.0).collect();
            for (f, tri) in mesh.faces() {
                assert!(created.contains(&f) ^ (sphere.face(f) == Some(tri)));
            }
            for (_, s) in &provenance.created_faces {
                assert!(s.faces.is_empty() && !s.operands.is_empty());
                assert!(
                    s.operands
                        .iter()
                        .all(|&(k, f)| k == 0 && sphere.is_face_live(f))
                );
            }
        }
    }
}
