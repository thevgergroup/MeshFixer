#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `mesh.make_solid` (Phase 1 Task 8a): the Meshmixer route for messy
//! scans, and the next action of the boolean's `operand_overlaps_itself`
//! refusal. Real-model cases are heavy (on demand).

use std::collections::BTreeMap;
use std::path::PathBuf;

use mf_document::ObjectId;
use mf_fixtures::{cube, merge, scaled, translated, uv_sphere};
use mf_geometry::{Bvh, FaceId, Mesh, inspect};
use mf_operations::{DocContext, OpError, OperandInput, RunContext, Target, find};
use mf_runtime::JobPool;

const IDENTITY: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

fn make_solid(m: &Mesh, params: &[(&str, &str)], ctx: &RunContext) -> Result<Mesh, OpError> {
    let op = find("mesh.make_solid").unwrap();
    let spec = mf_spec::builtin::get("mesh.make_solid").unwrap();
    let raw: BTreeMap<String, String> = params
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let out = op.execute(
        m,
        &Target::WholeObject,
        &spec.validate(&raw).unwrap(),
        &DocContext::default(),
        ctx,
    )?;
    Ok(out.change.unwrap().0)
}

/// Valid by D4's own check, and closed and manifold by inspection.
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

/// A sphere with a cube crossing its surface: one mesh, two shells that
/// intersect.
fn self_crossing_sphere() -> Mesh {
    merge(&[
        scaled(&uv_sphere(48, 96), 10.0),
        translated(&cube(2.0), [-11.0, -1.0, -1.0]),
    ])
}

#[test]
fn make_solid_turns_self_intersecting_scan_into_valid_solid() {
    let out = make_solid(
        &self_crossing_sphere(),
        &[("cell_size", "0.25")],
        &RunContext::none(),
    )
    .unwrap();
    assert_solid(&out);
    // One shell: the union of the sphere and the cube.
    assert_eq!(inspect(&out).components, 1);
    // And with smoothing.
    let smooth = make_solid(
        &self_crossing_sphere(),
        &[("cell_size", "0.25"), ("preserve_sharp", "false")],
        &RunContext::none(),
    )
    .unwrap();
    assert_solid(&smooth);
}

#[test]
fn make_solid_output_is_within_cell_size_of_input() {
    let h = 0.25;
    let sphere = scaled(&uv_sphere(96, 192), 10.0);
    let out = make_solid(&sphere, &[("cell_size", "0.25")], &RunContext::none()).unwrap();
    assert_solid(&out);
    // Output → input: every vertex within a cell of the sphere (the
    // tessellated sphere is within 0.01 of radius 10 too).
    let worst = out
        .vertices()
        .map(|(_, p)| ((p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt() - 10.0).abs())
        .fold(0.0, f64::max);
    assert!(worst <= h, "{worst}");
    // Input → output: the output reaches as far as the input in every
    // direction, within a cell.
    let (a, b) = (sphere.bounding_box().unwrap(), out.bounding_box().unwrap());
    for k in 0..3 {
        assert!((a.min[k] - b.min[k]).abs() <= h && (a.max[k] - b.max[k]).abs() <= h);
    }
}

#[test]
fn make_solid_is_deterministic_at_1_and_n_threads() {
    let run = |threads: usize| {
        let (pool, _) = JobPool::new(Some(threads));
        pool.install(|| {
            let m = make_solid(
                &self_crossing_sphere(),
                &[("cell_size", "0.4")],
                &RunContext::none(),
            )
            .unwrap();
            (m.content_hash(), m.structure_hash())
        })
    };
    let one = run(1);
    for n in [2, 8] {
        assert!(run(n) == one, "{n} threads");
    }
}

#[test]
fn make_solid_refuses_over_budget_grid_with_next_action() {
    match make_solid(
        &scaled(&uv_sphere(8, 16), 10.0),
        &[("cell_size", "0.001")],
        &RunContext::none(),
    ) {
        Err(OpError::Failed { code, message }) => {
            assert_eq!(code, "grid_too_large");
            assert!(message.contains("Use a larger cell size"), "{message}");
        }
        other => panic!("{:?}", other.map(|m| m.face_count())),
    }
}

#[test]
fn make_solid_cancel_leaves_document_unchanged() {
    let input = self_crossing_sphere();
    let before = input.content_hash();
    for stage in mf_operations::MAKE_SOLID_STAGES {
        let r = make_solid(
            &input,
            &[("cell_size", "0.4")],
            &RunContext::cancelling_at(stage),
        );
        assert!(matches!(r, Err(OpError::Cancelled)), "{stage}");
        assert_eq!(input.content_hash(), before);
    }
}

#[test]
fn boolean_after_make_solid_succeeds_where_it_was_refused() {
    let scan = self_crossing_sphere();
    let mut at = IDENTITY;
    at[9..].copy_from_slice(&[-10.5, -1.5, 0.5]);
    let union = |target: &Mesh| {
        let op = find("mesh.boolean").unwrap();
        let params = mf_spec::builtin::get("mesh.boolean")
            .unwrap()
            .validate(&BTreeMap::new())
            .unwrap();
        op.execute_doc(
            &[
                OperandInput {
                    slot: "target",
                    object: ObjectId(1),
                    name: "object",
                    mesh: target,
                    transform: IDENTITY,
                    target: Target::WholeObject,
                },
                OperandInput {
                    slot: "tool",
                    object: ObjectId(2),
                    name: "object",
                    mesh: &cube(1.0),
                    transform: at,
                    target: Target::WholeObject,
                },
            ],
            &params,
            &DocContext::default(),
            &RunContext::none(),
        )
    };
    match union(&scan) {
        Err(OpError::Failed { code, message }) => {
            assert_eq!(code, "operand_overlaps_itself");
            assert!(message.contains("Run Make solid on it first"));
        }
        other => panic!("{:?}", other.map(|o| o.report)),
    }
    let solid = make_solid(&scan, &[("cell_size", "0.25")], &RunContext::none()).unwrap();
    let out = union(&solid).unwrap();
    assert!(!out.changes.is_empty());
}

// ---- heavy (on demand) ----

fn fixtures_dir() -> PathBuf {
    std::env::var_os("MF_FIXTURES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(mf_fixtures::real::dir)
}

#[test]
fn make_solid_on_real_self_intersecting_scans() {
    // Winged victory (5.7M, crosses itself) and a corpus model.
    for name in [
        "messy-winged-victory-376253.obj",
        "messy-bunny-peel-298323.obj",
    ] {
        let path = fixtures_dir().join(name);
        let m = mf_io::read_path(&path).unwrap().mesh;
        let out = make_solid(&m, &[], &RunContext::none()).unwrap();
        assert_solid(&out);
        eprintln!("{name}: {} faces", out.face_count());
    }
}
