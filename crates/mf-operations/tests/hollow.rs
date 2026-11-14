#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `mesh.hollow` (MF-ISSUE-112): a sphere's shell (closed, manifold, two
//! shells, the wall within a cell of the setting, the outside unchanged),
//! a thin model left solid where it's thin and said so, drain holes that
//! connect the cavity to the outside (auto and placed, teardrop when
//! sideways), cancellation at every stage, determinism across thread
//! counts, and refusals by name.

use std::collections::BTreeMap;

use mf_document::ObjectId;
use mf_fixtures::{cube, scaled, uv_sphere};
use mf_geometry::{Bvh, FaceId, Mesh, inspect};
use mf_operations::{DocOutput, ObjectChange, OpError, OperandInput, RunContext, Target, find};
use mf_runtime::JobPool;

const IDENTITY: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

fn run_at(
    m: &Mesh,
    transform: [f64; 12],
    params: &[(&str, &str)],
    ctx: &RunContext,
) -> Result<DocOutput, OpError> {
    let op = find("mesh.hollow").unwrap();
    let spec = mf_spec::builtin::get("mesh.hollow").unwrap();
    let raw: BTreeMap<String, String> = params
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    op.execute_doc(
        &[OperandInput {
            slot: "target",
            object: ObjectId(1),
            name: "model",
            mesh: m,
            transform,
            target: Target::WholeObject,
        }],
        &spec.validate(&raw).unwrap(),
        ctx,
    )
}

fn run(m: &Mesh, params: &[(&str, &str)]) -> Result<(Mesh, serde_json::Value), OpError> {
    let out = run_at(m, IDENTITY, params, &RunContext::none())?;
    let mesh = match out.changes.into_iter().next().unwrap() {
        ObjectChange::Geometry { mesh, .. } => mesh,
        other => panic!("{other:?}"),
    };
    Ok((mesh, out.report))
}

/// Valid by D4's own check over every face, closed and manifold by
/// inspection, with positive volume.
fn assert_solid(m: &Mesh) {
    let all: Vec<FaceId> = m.faces().map(|(f, _)| f).collect();
    let bvh = Bvh::build(m, &RunContext::none()).unwrap();
    mf_geometry::validate_solid(m, &all, &bvh, &RunContext::none())
        .unwrap()
        .unwrap();
    let d = inspect(m);
    assert!(d.is_closed && d.is_manifold, "{d:?}");
    assert!(d.signed_volume.unwrap() > 0.0);
}

fn ball() -> Mesh {
    scaled(&uv_sphere(48, 96), 20.0)
}

/// A mushroom: a stem 30 across and 20 tall under a cap 60 across and 3
/// thick. With a 2 wall the cap (thinner than 4) stays solid.
fn mushroom() -> Mesh {
    let profile = [(15.0, 0.0), (15.0, 20.0), (30.0, 20.0), (30.0, 23.0)];
    let segments = 96u32;
    let mut positions = vec![[0.0, 0.0, 0.0], [0.0, 0.0, 23.0]];
    for k in 0..segments {
        let a = std::f64::consts::TAU * f64::from(k) / f64::from(segments);
        for &(r, z) in &profile {
            positions.push([r * a.cos(), r * a.sin(), z]);
        }
    }
    let p = profile.len() as u32;
    let at = |k: u32, i: u32| 2 + (k % segments) * p + i;
    let mut tris = Vec::new();
    for k in 0..segments {
        // Bottom disc (facing down) and top disc (facing up).
        tris.push([0, at(k + 1, 0), at(k, 0)]);
        tris.push([1, at(k, p - 1), at(k + 1, p - 1)]);
        for i in 0..p - 1 {
            let (a, b, c, d) = (at(k, i), at(k + 1, i), at(k + 1, i + 1), at(k, i + 1));
            tris.push([a, b, c]);
            tris.push([a, c, d]);
        }
    }
    let m = Mesh::new(positions, tris).unwrap();
    assert_solid(&m);
    m
}

#[test]
fn hollow_sphere_is_a_closed_two_shell_solid_with_the_wall_asked_for() {
    let input = ball();
    let (out, report) = run(&input, &[("wall_thickness", "2"), ("inner_detail", "0.5")]).unwrap();
    assert_solid(&out);
    assert_eq!(inspect(&out).components, 2, "outer and inner shells");
    // The outside is bit-identical: same ids, same positions.
    for (f, t) in input.faces() {
        assert_eq!(out.face(f), Some(t));
    }
    for (v, p) in input.vertices() {
        assert_eq!(out.position(v), Some(p));
    }
    // The wall: every inner vertex within a cell of 2 in from the
    // (tessellated) sphere of radius 20.
    let cell = report["cell_size"].as_f64().unwrap();
    assert_eq!(cell, 0.5);
    let outer_r = input
        .vertices()
        .map(|(_, p)| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt())
        .fold(f64::INFINITY, f64::min);
    let inner: Vec<[f64; 3]> = out
        .vertices()
        .filter(|(v, _)| input.position(*v).is_none())
        .map(|(_, p)| p)
        .collect();
    assert!(!inner.is_empty());
    for p in &inner {
        let r = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
        let wall = 20.0 - r;
        assert!(
            (wall - 2.0).abs() <= cell + (20.0 - outer_r),
            "wall {wall} at {p:?}"
        );
    }
    // Volume: the shell of a sphere 20 with a 2 wall, within the cell.
    let v = inspect(&out).signed_volume.unwrap();
    let shell = |w: f64| 4.0 / 3.0 * std::f64::consts::PI * (20f64.powi(3) - (20.0 - w).powi(3));
    assert!(v > shell(2.0 - cell) * 0.97 && v < shell(2.0 + cell), "{v}");
    assert_eq!(report["thin"]["faces"], 0, "{report}");
    assert!(report["thin"]["message"].is_null());
}

#[test]
fn hollow_thin_parts_stay_solid_and_the_report_says_so() {
    let input = mushroom();
    let (out, report) = run(&input, &[("wall_thickness", "2"), ("inner_detail", "0.5")]).unwrap();
    assert_solid(&out);
    assert_eq!(inspect(&out).components, 2);
    let thin = &report["thin"];
    assert!(thin["faces"].as_u64().unwrap() > 0, "{report}");
    assert_eq!(thin["message"], "Parts thinner than 4 were left solid");
    // The cavity stays in the stem (radius 13 = 15 less the wall, up to
    // 2 under the top at 23): none of it reaches out into the cap.
    for (v, p) in out.vertices() {
        if input.position(v).is_none() {
            let r = (p[0] * p[0] + p[1] * p[1]).sqrt();
            assert!(
                r < 13.0 + 0.6 && p[2] < 21.0 + 0.5,
                "inner vertex {p:?} in the cap"
            );
        }
    }
    // The cap's rim faces (r = 30) are among the thin ones.
    let thin_faces: Vec<FaceId> = {
        let shell =
            mf_operations::hollow::hollow_shell(&input, 2.0, 0.5, &RunContext::none()).unwrap();
        shell.thin
    };
    let rim = input.faces().filter(|(_, t)| {
        t.iter().all(|&v| {
            let p = input.corner_position(v);
            ((p[0] * p[0] + p[1] * p[1]).sqrt() - 30.0).abs() < 1e-9
        })
    });
    for (f, _) in rim {
        assert!(thin_faces.contains(&f), "rim face {f:?} should be thin");
    }
    // The stem's side faces aren't.
    let side = input.faces().filter(|(_, t)| {
        t.iter().all(|&v| {
            let p = input.corner_position(v);
            ((p[0] * p[0] + p[1] * p[1]).sqrt() - 15.0).abs() < 1e-9 && p[2] < 15.0
        })
    });
    for (f, _) in side {
        assert!(!thin_faces.contains(&f), "stem face {f:?} isn't thin");
    }
}

/// A ray from `from` along `dir` hits nothing of `m`.
fn escapes(m: &Mesh, from: [f64; 3], dir: [f64; 3]) -> bool {
    let bvh = Bvh::build(m, &RunContext::none()).unwrap();
    bvh.ray_pick(m, from, dir, &glam::DMat4::IDENTITY).is_none()
}

#[test]
fn hollow_auto_drain_hole_connects_the_cavity_to_the_outside() {
    let input = ball();
    let (out, report) = run(
        &input,
        &[
            ("wall_thickness", "2"),
            ("inner_detail", "0.5"),
            ("drain_holes", "auto"),
            ("hole_diameter", "3"),
        ],
    )
    .unwrap();
    assert_solid(&out);
    // One connected, boundary-free surface: the hole joins the shells.
    assert_eq!(inspect(&out).components, 1, "{report}");
    let holes = report["holes"].as_array().unwrap();
    assert_eq!(holes.len(), 1);
    // At the bottom, vertical, so round.
    assert_eq!(holes[0]["profile"], "round");
    let at = holes[0]["at"].as_array().unwrap();
    assert!(at[2].as_f64().unwrap() < -19.0, "{report}");
    // A ray from inside the cavity straight down escapes through it.
    let x = at[0].as_f64().unwrap();
    let y = at[1].as_f64().unwrap();
    assert!(escapes(&out, [x, y, 0.0], [0.0, 0.0, -1.0]));
    // Without the hole it wouldn't.
    let (closed, _) = run(&input, &[("wall_thickness", "2"), ("inner_detail", "0.5")]).unwrap();
    assert!(!escapes(&closed, [x, y, 0.0], [0.0, 0.0, -1.0]));
}

#[test]
fn hollow_auto_drain_holes_follow_the_plate_orientation() {
    // Turned so the object's +x points down: the lowest point of the
    // cavity is at the object's +x side.
    let turned = [0.0, 0.0, -1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 50.0];
    let out = run_at(
        &ball(),
        turned,
        &[
            ("wall_thickness", "2"),
            ("inner_detail", "0.5"),
            ("drain_holes", "auto"),
            ("hole_count", "2"),
        ],
        &RunContext::none(),
    )
    .unwrap();
    let holes = out.report["holes"].as_array().unwrap();
    assert_eq!(holes.len(), 2, "{}", out.report);
    let first = holes[0]["at"].as_array().unwrap();
    assert!(first[0].as_f64().unwrap() > 15.0, "{}", out.report);
    for h in holes {
        let axis = h["axis"].as_array().unwrap();
        // Outward along object +x: world down.
        assert!(axis[0].as_f64().unwrap() > 0.99, "{}", out.report);
        assert_eq!(h["profile"], "round");
    }
}

#[test]
fn hollow_placed_sideways_hole_is_a_teardrop_and_reaches_the_cavity() {
    let input = ball();
    // The face nearest the +x equator.
    let (face, _) = input
        .faces()
        .min_by(|(_, a), (_, b)| {
            let score = |t: &[u32; 3]| {
                let c: [f64; 3] = std::array::from_fn(|k| {
                    t.iter().map(|&v| input.corner_position(v)[k]).sum::<f64>() / 3.0
                });
                (c[0] - 20.0).powi(2) + c[1].powi(2) + c[2].powi(2)
            };
            score(a).total_cmp(&score(b))
        })
        .unwrap();
    let points = format!("{},0.3333333333,0.3333333333,0.3333333334,0", face.0);
    let (out, report) = run(
        &input,
        &[
            ("wall_thickness", "2"),
            ("inner_detail", "0.5"),
            ("drain_holes", "placed"),
            ("hole_points", &points),
        ],
    )
    .unwrap();
    assert_solid(&out);
    assert_eq!(inspect(&out).components, 1);
    assert_eq!(report["holes"][0]["profile"], "teardrop", "{report}");
    assert!(escapes(&out, [0.0, 0.0, 0.0], [1.0, 0.0, 0.0]));
}

#[test]
fn hollow_placed_hole_on_a_thin_part_is_refused_by_name() {
    let input = mushroom();
    // A face of the cap's rim: solid there.
    let (face, _) = input
        .faces()
        .find(|(_, t)| {
            t.iter().all(|&v| {
                let p = input.corner_position(v);
                ((p[0] * p[0] + p[1] * p[1]).sqrt() - 30.0).abs() < 1e-9
            })
        })
        .unwrap();
    let points = format!("{},0.3333333333,0.3333333333,0.3333333334,0", face.0);
    match run(
        &input,
        &[
            ("wall_thickness", "2"),
            ("inner_detail", "0.5"),
            ("drain_holes", "placed"),
            ("hole_points", &points),
        ],
    ) {
        Err(OpError::Failed { code, message }) => {
            assert_eq!(code, "hole_misses_cavity", "{message}");
            assert!(message.contains("thicker part"), "{message}");
        }
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("should be refused"),
    }
}

#[test]
fn hollow_refuses_by_name() {
    let code = |r: Result<(Mesh, serde_json::Value), OpError>| match r {
        Err(OpError::Failed { code, .. }) => code,
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("should be refused"),
    };
    // Open.
    assert_eq!(
        code(run(&mf_fixtures::cube_with_missing_face(), &[])),
        "open_input"
    );
    // Too thin to hollow anywhere: a 3 thick plate with a 2 wall.
    let plate = mf_parts::connectors::joint::prism_z(
        &[[0.0, 0.0], [40.0, 0.0], [40.0, 40.0], [0.0, 40.0]],
        0.0,
        3.0,
    );
    assert_eq!(
        code(run(
            &plate,
            &[("wall_thickness", "2"), ("inner_detail", "0.25")]
        )),
        "too_thin"
    );
    // A cell over half the wall.
    assert_eq!(
        code(run(
            &ball(),
            &[("wall_thickness", "2"), ("inner_detail", "1.5")]
        )),
        "cell_too_coarse"
    );
    // Over the budget, with the next action.
    match run(
        &ball(),
        &[("wall_thickness", "2"), ("inner_detail", "0.01")],
    ) {
        Err(OpError::Failed { code, message }) => {
            assert_eq!(code, "grid_too_large");
            assert!(message.contains("Use a larger cell size"), "{message}");
        }
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("should be refused"),
    }
    // Placed with no points.
    assert_eq!(
        code(run(&ball(), &[("drain_holes", "placed")])),
        "no_hole_points"
    );
}

#[test]
fn hollow_cancels_at_every_stage() {
    for stage in mf_operations::hollow::STAGES {
        let ctx = RunContext::cancelling_at(stage);
        let r = run_at(
            &ball(),
            IDENTITY,
            &[
                ("wall_thickness", "2"),
                ("inner_detail", "0.5"),
                ("drain_holes", "auto"),
            ],
            &ctx,
        );
        assert!(matches!(r, Err(OpError::Cancelled)), "{stage}");
    }
}

#[test]
fn hollow_is_deterministic_at_1_and_n_threads() {
    let run_with = |threads: usize| {
        let (pool, _) = JobPool::new(Some(threads));
        pool.install(|| {
            let (m, _) = run(
                &ball(),
                &[
                    ("wall_thickness", "2"),
                    ("inner_detail", "0.6"),
                    ("drain_holes", "auto"),
                ],
            )
            .unwrap();
            (m.content_hash(), m.structure_hash())
        })
    };
    let one = run_with(1);
    for n in [2, 8] {
        assert!(run_with(n) == one, "{n} threads");
    }
}

#[test]
fn hollow_cube_keeps_its_corners_and_gets_a_box_cavity() {
    let input = scaled(&cube(1.0), 30.0);
    let (out, report) = run(&input, &[("wall_thickness", "3"), ("inner_detail", "0.5")]).unwrap();
    assert_solid(&out);
    assert_eq!(inspect(&out).components, 2);
    // The cavity's faces are within a cell of 3 in from each side
    // (corners and edges round off a little).
    let bb = out
        .vertices()
        .filter(|(v, _)| input.position(*v).is_none())
        .fold(
            ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]),
            |(lo, hi), (_, p)| {
                (
                    std::array::from_fn(|k| lo[k].min(p[k])),
                    std::array::from_fn(|k| hi[k].max(p[k])),
                )
            },
        );
    for k in 0..3 {
        assert!((bb.0[k] - 3.0).abs() <= 0.5, "{bb:?} {report}");
        assert!((bb.1[k] - 27.0).abs() <= 0.5, "{bb:?} {report}");
    }
    assert_eq!(report["thin"]["faces"], 0, "{report}");
}

/// The settings are in document units: an object placed at twice its
/// size gets walls half as thick in its own units.
#[test]
fn hollow_settings_are_in_document_units_under_a_scaled_placement() {
    let doubled = [2.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0];
    let out = run_at(
        &ball(),
        doubled,
        &[("wall_thickness", "4"), ("inner_detail", "1")],
        &RunContext::none(),
    )
    .unwrap();
    assert_eq!(out.report["cell_size"], 1.0, "{}", out.report);
    let ObjectChange::Geometry { mesh, .. } = &out.changes[0] else {
        panic!()
    };
    let input = ball();
    for (v, p) in mesh.vertices() {
        if input.position(v).is_none() {
            let r = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
            assert!((r - 18.0).abs() < 0.6, "{r}");
        }
    }
}

// ---------------------------------------------------------------------
// Open through the picked surface.
// ---------------------------------------------------------------------

/// A split sphere's dome: a ball cut at `z = at` (`mesh.split_plane`,
/// `keep: above`), closed with a flat cap — the common case `open_through`
/// is for. Returns the dome and its cap's input-face seeds (the faces
/// `mesh.split_plane` created to close it).
fn split_sphere_cap(radius: f64, at: f64) -> (Mesh, Vec<FaceId>) {
    split_sphere_cap_at(radius, at, 48, 96)
}

fn split_sphere_cap_at(radius: f64, at: f64, rings: u32, segments: u32) -> (Mesh, Vec<FaceId>) {
    let spec = mf_spec::builtin::get("mesh.split_plane").unwrap();
    let raw: BTreeMap<String, String> = [
        ("plane_point", format!("0,0,{at}")),
        ("plane_normal", "0,0,1".to_string()),
        ("keep", "above".to_string()),
        ("cap", "true".to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let params = spec.validate(&raw).unwrap();
    let sphere = scaled(&uv_sphere(rings, segments), radius);
    let out = find("mesh.split_plane")
        .unwrap()
        .execute_doc(
            &[OperandInput {
                slot: "target",
                object: ObjectId(1),
                name: "scan",
                mesh: &sphere,
                transform: IDENTITY,
                target: Target::WholeObject,
            }],
            &params,
            &RunContext::none(),
        )
        .unwrap();
    let ObjectChange::Geometry {
        operand: 0,
        mesh,
        provenance,
    } = out.changes.into_iter().next().unwrap()
    else {
        panic!("expected the target's own geometry to change");
    };
    let cap: Vec<FaceId> = provenance.created_faces.iter().map(|(f, _)| *f).collect();
    assert!(!cap.is_empty(), "the cut should be capped");
    (mesh, cap)
}

fn faces_param(faces: &[FaceId]) -> String {
    faces
        .iter()
        .map(|f| f.0.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Hollowing a split-sphere's dome with `open_through` set to its cap
/// gives a single open-topped shell: closed, manifold (D4's own check,
/// `assert_solid`), with the rim connecting the inner and outer walls —
/// and the rest of the outer surface (away from the cap) untouched. A
/// large sphere, cut well short of its pole, so the cap is gently
/// curved relative to the wall plus margin `open_through` extrudes
/// through (a tight dome, like a cap cut close to a sphere's pole,
/// tapers far too fast for any fixed-depth plug to track — not the
/// "planar split cap" case this feature is for).
#[test]
fn hollow_open_through_a_split_cap_gives_an_open_topped_shell() {
    let (dome, cap) = split_sphere_cap(20.0, 0.0);
    let (out, report) = run(
        &dome,
        &[
            ("wall_thickness", "2"),
            ("inner_detail", "0.5"),
            ("open_through", "true"),
            ("open_through_faces", &faces_param(&cap)),
        ],
    )
    .unwrap();
    assert_solid(&out);
    let topo = inspect(&out);
    assert!(topo.is_closed && topo.is_manifold, "{topo:?}");
    // One connected surface: the outer wall, the rim and the inner wall
    // all join up (not two separate closed shells any more).
    assert_eq!(topo.components, 1, "{report}");
    // The spherical surface away from the cap moved nowhere (a boolean
    // cut's own local window around the opening may give a nearby face
    // a new id, same as any other cut, but never moves one).
    let mut moved = 0;
    for (f, t) in dome.faces() {
        if cap.contains(&f) || !out.is_face_live(f) {
            continue;
        }
        let before: Vec<[f64; 3]> = t.iter().map(|&v| dome.corner_position(v)).collect();
        let after: Vec<[f64; 3]> = out
            .face(f)
            .unwrap()
            .iter()
            .map(|&v| out.corner_position(v))
            .collect();
        if before != after {
            moved += 1;
        }
    }
    assert_eq!(moved, 0, "{report}");
    // No face of the cap itself survives: the opening replaces it.
    for &f in &cap {
        assert!(!out.is_face_live(f), "cap face {f:?} should be gone");
    }
    // A bowl's shell has no boundary at all — the rim joins the outer
    // and inner surfaces into one closed, manifold surface (already
    // checked by `assert_solid`); the opening into the cavity is purely
    // topological (one component, not two separate closed shells).
    assert_eq!(topo.boundary_loops.len(), 0, "{report}");
}

/// The owner's own case (2026-11-14 follow-up): Hollow with
/// `open_through` on the bottom half of a split sphere (a hemisphere)
/// must leave its rim at the cut height, with the full wall thickness
/// — not with the whole cap region's radius cut away, which took the
/// rim's own wall down with it (the bug: the plug was the full cap
/// extruded inward, so it cut the outer wall ring too; the fix: the
/// plug is the cap shrunk inward by the wall thickness, the cavity's
/// own outline at the cap plane).
#[test]
fn hollow_open_through_a_hemisphere_keeps_the_rim_at_the_cut_height_and_full_thickness() {
    let radius = 20.0;
    let wall = 2.0;
    let cell = 0.5;
    let (dome, cap) = split_sphere_cap(radius, 0.0);
    let (out, report) = run(
        &dome,
        &[
            ("wall_thickness", &wall.to_string()),
            ("inner_detail", &cell.to_string()),
            ("open_through", "true"),
            ("open_through_faces", &faces_param(&cap)),
        ],
    )
    .unwrap();
    assert_solid(&out);
    let topo = inspect(&out);
    assert_eq!(topo.components, 1, "{report}");
    assert_eq!(topo.boundary_loops.len(), 0, "{report}");

    // The rim: vertices near the cap's own plane (z = 0) whose radius
    // sits between the inner wall (radius ~ wall in from the outer
    // surface) and the outer surface itself — the ring the plug's own
    // side wall cut, where the outer and inner surfaces now meet.
    let rim: Vec<[f64; 3]> = out
        .vertices()
        .map(|(_, p)| p)
        .filter(|p| p[2].abs() < cell * 2.0)
        .collect();
    assert!(!rim.is_empty(), "there should be a rim near z = 0");

    // The rim height equals the cut plane height (z = 0) within one
    // cell: every rim vertex is already filtered to within 2 cells of
    // it above, so this restates the bound tightly on the filtered set
    // itself (the outer ones, at the true cut height exactly, since
    // the outer surface is bit-identical there).
    let outer_rim_z: Vec<f64> = rim
        .iter()
        .filter(|p| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt() > radius - cell)
        .map(|p| p[2])
        .collect();
    assert!(!outer_rim_z.is_empty(), "no outer rim vertex found");
    for &z in &outer_rim_z {
        assert!(
            z.abs() <= cell,
            "outer rim at z={z}, expected within {cell} of 0"
        );
    }

    // The rim wall thickness: the gap between the outer rim's radius
    // (the original sphere's own radius, unchanged) and the inner
    // rim's radius (the plug's own inset boundary, nominally
    // radius - wall) equals wall_thickness within one cell.
    let radii: Vec<f64> = rim
        .iter()
        .map(|p| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt())
        .collect();
    let outer_radius = radii.iter().cloned().fold(0.0_f64, f64::max);
    let inner_radius = radii.iter().cloned().fold(f64::INFINITY, f64::min);
    assert!(
        (outer_radius - radius).abs() <= cell,
        "outer rim radius {outer_radius}, expected near {radius}"
    );
    let measured_wall = outer_radius - inner_radius;
    assert!(
        (measured_wall - wall).abs() <= cell,
        "rim wall thickness {measured_wall}, expected near {wall} (report: {report})"
    );

    // The outer surface is bit-identical everywhere except the cap: a
    // face outside the picked region may get a new id if the boolean
    // kernel's own local window around the opening touched it (same as
    // any other cut), but one that's still live at its original id
    // never moves, and most of the outer surface (far from the cut)
    // keeps its id outright.
    let mut unchanged = 0;
    for (f, t) in dome.faces() {
        if cap.contains(&f) || !out.is_face_live(f) {
            continue;
        }
        let before: Vec<[f64; 3]> = t.iter().map(|&v| dome.corner_position(v)).collect();
        let after: Vec<[f64; 3]> = out
            .face(f)
            .unwrap()
            .iter()
            .map(|&v| out.corner_position(v))
            .collect();
        assert_eq!(before, after, "face {f:?} outside the cap moved");
        unchanged += 1;
    }
    assert!(unchanged > 0);
    // Far from the cap (well outside the kernel's own local window),
    // every face keeps its id outright.
    let far_unchanged = dome
        .faces()
        .filter(|(f, t)| {
            !cap.contains(f)
                && t.iter().all(|&v| {
                    let p = dome.corner_position(v);
                    p[2] > wall * 4.0
                })
        })
        .count();
    assert!(far_unchanged > 0);
    for (f, t) in dome.faces() {
        if cap.contains(&f) || !t.iter().all(|&v| dome.corner_position(v)[2] > wall * 4.0) {
            continue;
        }
        assert_eq!(out.face(f), Some(t), "far face {f:?} should keep its id");
    }
}

/// `open_through` needs a picked region; refused by name when it's on
/// with no faces given.
#[test]
fn hollow_open_through_without_faces_is_refused() {
    let err = run(
        &ball(),
        &[
            ("wall_thickness", "2"),
            ("inner_detail", "0.5"),
            ("open_through", "true"),
        ],
    )
    .unwrap_err();
    assert_eq!(code_of(&err), "open_through_needs_faces");
}

/// A cap close enough to a sphere's pole that the dome's own radius
/// shrinks below the plug's (inset) footprint before its depth clears
/// the wall is refused by name, not silently cut — severing the outer
/// surface in two rather than opening a rim (owner 2026-11-14's own
/// hemisphere bug, caught here one step short of ever reaching D4).
#[test]
fn hollow_open_through_too_close_to_the_pole_is_refused_not_corrupted() {
    let (dome, cap) = split_sphere_cap(20.0, 10.0);
    let err = run(
        &dome,
        &[
            ("wall_thickness", "2"),
            ("inner_detail", "0.5"),
            ("open_through", "true"),
            ("open_through_faces", &faces_param(&cap)),
        ],
    )
    .unwrap_err();
    assert_eq!(code_of(&err), "open_through_region_too_curved");
}

/// Automatic detail (a finer cell than a fixed 0.5 for this object's
/// size) still clears through to the inner wall cleanly: the margin
/// and depth scale with the cell either way (owner 2026-11-14's own
/// GUI repro, which used the panel's automatic default).
#[test]
fn hollow_open_through_works_with_automatic_detail_too() {
    let (dome, cap) = split_sphere_cap_at(20.0, 1.3, 32, 48);
    let (out, report) = run(
        &dome,
        &[
            ("wall_thickness", "2"),
            ("inner_detail", "0"),
            ("open_through", "true"),
            ("open_through_faces", &faces_param(&cap)),
        ],
    )
    .unwrap();
    assert_solid(&out);
    assert_eq!(inspect(&out).components, 1, "{report}");
}

// ---------------------------------------------------------------------
// Drain holes: optional, default none.
// ---------------------------------------------------------------------

/// `drain_holes` defaults to `none` (the spec's own default): a hollow
/// run with no `drain_holes` given at all cuts no hole and reports
/// none, leaving the outer and inner shells as two separate closed
/// surfaces — Auto and Placed stay available by asking for them.
#[test]
fn hollow_drain_holes_defaults_to_none() {
    let (out, report) = run(&ball(), &[("wall_thickness", "2"), ("inner_detail", "0.5")]).unwrap();
    assert_solid(&out);
    assert_eq!(
        inspect(&out).components,
        2,
        "no hole joins the outer and inner shells"
    );
    assert_eq!(report["holes"].as_array().unwrap().len(), 0, "{report}");
}

/// Asking for `drain_holes: none` explicitly behaves the same as
/// leaving it out.
#[test]
fn hollow_drain_holes_none_explicit_matches_the_default() {
    let (default, _) = run(&ball(), &[("wall_thickness", "2"), ("inner_detail", "0.5")]).unwrap();
    let (explicit, _) = run(
        &ball(),
        &[
            ("wall_thickness", "2"),
            ("inner_detail", "0.5"),
            ("drain_holes", "none"),
        ],
    )
    .unwrap();
    assert_eq!(default.content_hash(), explicit.content_hash());
}

fn code_of(e: &OpError) -> &'static str {
    match e {
        OpError::Failed { code, .. } => code,
        other => panic!("expected a refusal, got {other:?}"),
    }
}
