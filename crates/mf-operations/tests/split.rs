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
            slot: "target",
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

/// Owner 2026-11-12: "Cap the cut", off — for a vase or bowl. Each kept
/// part is left open at the cut: no cap faces, no joint recorded, and a
/// real open boundary where the plane crossed it.
#[test]
fn cap_off_leaves_the_parts_open_with_no_joint() {
    let mesh = cube(2.0);
    let out = split_with(
        &mesh,
        IDENTITY,
        &[("plane_point", "0,0,0.7"), ("cap", "false")],
    )
    .unwrap();
    let (top, bottom) = parts(&out);
    let (_, bottom, _) = bottom.unwrap();
    for (k, part) in [&top, &bottom].into_iter().enumerate() {
        let caps = out.report["parts"][k]["cap_faces"].as_array().unwrap();
        assert!(caps.is_empty(), "{part:?}: no cap faces when cap is off");
        let topo = part.topology_with(&RunContext::none()).unwrap();
        assert!(
            topo.boundary_edges > 0,
            "part {k}: left open at the cut, a real boundary"
        );
    }
    assert_eq!(
        out.report["joints"].as_array().unwrap().len(),
        0,
        "no cap face pair to report as a joint"
    );
    // The pieces still partition the cube's volume exactly (only the
    // closing cap is skipped, not the cut itself).
    assert!(close(volume(&top) + volume(&bottom), 8.0, 1e-9));
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
    // Genuinely past the cube's top face (not exactly on it, which must
    // never be refused — see `plane_on_a_box_top_face_is_never_refused`).
    assert_eq!(
        code(split_with(&c, IDENTITY, &[("plane_point", "0,0,2")])),
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
    // A unit cube's section is too thin for the default 5 mm hole.
    assert_eq!(
        code(split_with(
            &c,
            IDENTITY,
            &[("plane_point", "0,0,0.5"), ("pegs", "dowels")]
        )),
        "section_too_thin"
    );
    // Two cubes touching along an edge, cut through that edge: the two
    // section squares touch at a point, which is refused, not guessed.
    // Its own code, not `mesh.boolean`'s "section_self_intersects": the
    // model here isn't self-overlapping, only this plane's section is
    // ambiguous (Make solid wouldn't help, so it isn't offered).
    let touching = merge(&[cube(1.0), translated(&cube(1.0), [1.0, 1.0, 0.0])]);
    assert_eq!(
        code(split_with(
            &touching,
            IDENTITY,
            &[("plane_point", "0,0,0.5")]
        )),
        "split_section_self_intersects"
    );
}

/// Owner 2026-11-12: a plane lying exactly on a box's top face must
/// never be refused. The faces either side of the plane resolve by a
/// fixed symbolic perturbation (simulation of simplicity), not by
/// chance: the top face goes to the kept ("bottom") side, unchanged, no
/// cap duplicating it, and the top ("above") side is empty — so `keep:
/// both` quietly keeps just the one real part instead of refusing the
/// whole cut.
#[test]
fn plane_on_a_box_top_face_is_never_refused() {
    let c = cube(20.0);
    let out = split_with(&c, IDENTITY, &[("plane_point", "0,0,20")]).unwrap();
    let (bottom, created) = parts(&out);
    assert!(created.is_none(), "nothing above the plane to create");
    assert_solid(&bottom, "bottom");
    assert!(close(volume(&bottom), 20.0f64.powi(3), 1e-9));
    assert_eq!(
        bottom.face_count(),
        c.face_count(),
        "unchanged, bit for bit"
    );
    for (f, tri) in c.faces() {
        assert_eq!(bottom.face(f), Some(tri), "face {f:?}");
    }
    assert!(
        out.changes
            .iter()
            .any(|c| matches!(c, ObjectChange::Rename { name, .. } if name == "scan (bottom)"))
    );
    assert!(
        !out.changes
            .iter()
            .any(|c| matches!(c, ObjectChange::Create { .. })),
        "no empty \"(top)\" object"
    );

    // The same plane, but keeping only "above" (genuinely empty): that
    // is still refused by name, since asking for just the empty side
    // is asking for nothing.
    assert_eq!(
        code(split_with(
            &c,
            IDENTITY,
            &[("plane_point", "0,0,20"), ("keep", "above")]
        )),
        "plane_misses_object"
    );
}

/// The bug report's own scenario (owner 2026-11-12): a cube and a sphere
/// combined (unioned) so the cube's top face survives in the result,
/// cut exactly along that coplanar face. Never refused; the report
/// names the one part kept.
#[test]
fn combined_cube_and_sphere_at_the_coplanar_plane_is_never_refused() {
    let cube_size = 20.0;
    // Pokes out through the cube's -X side face, not its top, so their
    // surfaces genuinely overlap (a real union, not two separate
    // shells) while the top face stays untouched and purely coplanar
    // with the cut — the bug report's own shape.
    let sphere = translated(
        &scaled(&uv_sphere(24, 48), 8.0),
        [0.0, cube_size / 2.0, cube_size / 2.0],
    );
    let kernel = mf_boolean::ManifoldCpp::checked();
    let combined = mf_boolean::boolean(
        &cube(cube_size),
        &sphere,
        mf_boolean::BoolOp::Union,
        &kernel,
        &RunContext::none(),
    )
    .expect("cube and sphere overlap, so their union succeeds");
    let out = split_with(
        &combined.mesh,
        IDENTITY,
        &[("plane_point", &v3([0.0, 0.0, cube_size]))],
    )
    .unwrap();
    assert_eq!(out.report["parts"].as_array().unwrap().len(), 1);
    assert_eq!(out.report["parts"][0]["name"], "scan (bottom)");
    let (bottom, created) = parts(&out);
    assert!(created.is_none());
    assert_solid(&bottom, "bottom");
    assert!(close(volume(&bottom), volume(&combined.mesh), 1e-9));
}

/// A plane along an edge (not a face): it touches the cube only where
/// one vertical edge lies exactly on it, with the whole rest of the cube
/// strictly on one side. Never refused either, by the same rule.
#[test]
fn plane_along_an_edge_is_never_refused() {
    let c = cube(1.0);
    let out = split_with(
        &c,
        IDENTITY,
        &[("plane_point", "0,0,0.5"), ("plane_normal", "1,1,0")],
    )
    .unwrap();
    let (kept, created) = parts(&out);
    assert!(
        created.is_none(),
        "nothing strictly on the plane's far side"
    );
    assert_solid(&kept, "kept");
    assert!(close(volume(&kept), 1.0, 1e-9));
    assert_eq!(kept.face_count(), c.face_count(), "unchanged, bit for bit");
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

/// A UV sphere cut exactly at a whole ring of vertices (owner
/// 2026-11-12, `split_section_self_intersects` on the demo scan: a
/// horizontal plane at a sphere's equator, where a *ring* of vertices —
/// not just one — lies on the plane). Covers both the equator (ring
/// 12 of 24: `cos(pi/2)` is an ulp off true zero in `f64`, so the ring
/// is a genuine, if near-zero, crossing, not [`mf_boolean::plane_cut`]'s
/// on-plane `SOS_SIDE` case) and another latitude ring (ring 8: `z =
/// 0.5` exactly, a bit-exact ring). Keep both/above/below, cap on and
/// off, all at both rings.
#[test]
fn sphere_cut_at_a_vertex_ring_is_never_refused() {
    use mf_boolean::{Plane, Side, cut_half_with};
    let sphere = scaled(&uv_sphere(24, 48), 10.0);
    for z in [6.123_233_995_736_766e-17_f64 * 10.0, 0.5 * 10.0] {
        let plane = Plane {
            point: [0.0, 0.0, z],
            normal: [0.0, 0.0, 1.0],
        };
        for keep in [Side::Below, Side::Above] {
            for cap in [true, false] {
                let half = cut_half_with(&sphere, &plane, keep, cap, &RunContext::none())
                    .unwrap_or_else(|e| panic!("z={z} {keep:?} cap={cap}: {e}"));
                if cap {
                    let (p, t) = half.mesh.to_arrays();
                    mf_boolean::validate_solid(&p, &t, &RunContext::none())
                        .unwrap_or_else(|e| panic!("z={z} {keep:?} cap={cap}: not solid: {e}"));
                }
            }
        }
    }
}

/// A low-poly cylinder (three rings: bottom, middle, top) cut exactly at
/// its middle vertex ring: every vertex the plane touches is shared by
/// faces genuinely crossing it on both sides, the same shape of fault as
/// the UV sphere's equator but on a mesh with no near-zero
/// floating-point artefact — the ring really is exactly on the plane.
#[test]
fn cylinder_cut_exactly_at_a_vertex_ring_is_never_refused() {
    use mf_boolean::{Plane, Side, cut_half_with};
    let sides: u32 = 8;
    let mut positions = Vec::new();
    for &z in &[0.0_f64, 1.0, 2.0] {
        for s in 0..sides {
            let a = 2.0 * std::f64::consts::PI * s as f64 / sides as f64;
            positions.push([a.cos(), a.sin(), z]);
        }
    }
    let bottom_centre = positions.len() as u32;
    positions.push([0.0, 0.0, 0.0]);
    let top_centre = positions.len() as u32;
    positions.push([0.0, 0.0, 2.0]);
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    for r in 0..2 {
        for s in 0..sides {
            let (b0, b1) = (r * sides + s, r * sides + (s + 1) % sides);
            let (t0, t1) = ((r + 1) * sides + s, (r + 1) * sides + (s + 1) % sides);
            triangles.push([b0, b1, t1]);
            triangles.push([b0, t1, t0]);
        }
    }
    for s in 0..sides {
        triangles.push([bottom_centre, (s + 1) % sides, s]);
    }
    for s in 0..sides {
        let (t0, t1) = (2 * sides + s, 2 * sides + (s + 1) % sides);
        triangles.push([top_centre, t0, t1]);
    }
    let cylinder = Mesh::new(positions, triangles).unwrap();
    // Exactly at the middle ring (z=1): every one of its vertices is on
    // the plane, shared by the side walls genuinely crossing it above
    // and below.
    let plane = Plane {
        point: [0.0, 0.0, 1.0],
        normal: [0.0, 0.0, 1.0],
    };
    for keep in [Side::Below, Side::Above] {
        for cap in [true, false] {
            let half = cut_half_with(&cylinder, &plane, keep, cap, &RunContext::none())
                .unwrap_or_else(|e| panic!("{keep:?} cap={cap}: {e}"));
            if cap {
                let (p, t) = half.mesh.to_arrays();
                mf_boolean::validate_solid(&p, &t, &RunContext::none())
                    .unwrap_or_else(|e| panic!("{keep:?} cap={cap}: not solid: {e}"));
            }
        }
    }
}
