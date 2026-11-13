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
