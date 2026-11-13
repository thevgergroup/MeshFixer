#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `mesh.boolean` (MF-MODEL-001, Phase 1 Task 8): results, the D5 window,
//! D15 openings, refusals by name, cancel, determinism and provenance.
//! Heavy and timing tests (real models, 30M and 1M) run on demand only
//! (`scripts/run-tests.sh`'s timing list).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use mf_boolean::{
    BoolOp, BooleanError, Kernel, KernelError, KernelMesh, KernelResult, ManifoldCpp, STAGES,
    boolean,
};
use mf_document::ObjectId;
use mf_fixtures::{cube, merge, scaled, translated, uv_sphere};
use mf_geometry::{FaceId, Mesh, inspect};
use mf_operations::{DocOutput, ObjectChange, OpError, OperandInput, RunContext, Target, find};
use mf_runtime::JobPool;

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
    let raw: BTreeMap<String, String> = params
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let params = spec.validate(&raw).unwrap();
    op.execute_doc(
        &[
            OperandInput {
                slot: "target",
                object: ObjectId(1),
                name: "object",
                mesh: target,
                transform: target_t,
                target: Target::WholeObject,
            },
            OperandInput {
                slot: "tool",
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

fn result(out: &DocOutput) -> (&Mesh, &mf_geometry::Provenance) {
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

fn volume(m: &Mesh) -> f64 {
    mf_geometry::signed_volume(m) / 6.0
}

fn code(r: Result<DocOutput, OpError>) -> (&'static str, String) {
    match r {
        Err(OpError::Failed { code, message }) => (code, message),
        Err(e) => panic!("{e}"),
        Ok(o) => panic!("expected a refusal, got {}", o.report),
    }
}

fn translate(d: [f64; 3]) -> [f64; 12] {
    let mut t = IDENTITY;
    t[9..].copy_from_slice(&d);
    t
}

/// A sphere of radius 10 with the faces whose every corner satisfies
/// `cut` removed (an opening).
fn sphere_without(cut: impl Fn([f64; 3]) -> bool) -> Mesh {
    let base = scaled(&uv_sphere(48, 96), 10.0);
    let mut e = base.edit();
    for (f, t) in base.faces() {
        if t.iter().all(|&v| cut(base.corner_position(v))) {
            e.remove_face(f).unwrap();
        }
    }
    e.finish().unwrap().0
}

#[test]
fn union_subtract_intersect_of_offset_cubes_match_exact_volumes() {
    let target = cube(2.0);
    let tool = cube(1.0);
    for (op, want) in [("union", 8.5), ("subtract", 7.5), ("intersect", 0.5)] {
        let out = run(
            &target,
            IDENTITY,
            &tool,
            translate([1.5, 0.5, 0.5]),
            &[("operation", op)],
            &RunContext::none(),
        )
        .unwrap_or_else(|e| panic!("{op}: {e}"));
        let v = volume(result(&out).0);
        assert!((v - want).abs() <= 1e-12 * want, "{op}: {v} vs {want}");
    }
}

#[test]
fn faces_outside_the_window_are_kept_bitwise() {
    let target = scaled(&uv_sphere(64, 128), 10.0);
    let out = run(
        &target,
        IDENTITY,
        &cube(1.0),
        translate([9.5, -0.5, -0.5]),
        &[],
        &RunContext::none(),
    )
    .unwrap();
    let (mesh, prov) = result(&out);
    let removed: HashSet<FaceId> = prov.removed_faces.iter().copied().collect();
    assert!(
        removed.len() * 50 < target.face_count(),
        "{} cut",
        removed.len()
    );
    assert!(prov.modified_faces.is_empty() && prov.moved_vertices.is_empty());
    for (f, t) in target.faces() {
        if !removed.contains(&f) {
            assert_eq!(mesh.face(f), Some(t), "face {f:?}");
            for v in t {
                assert_eq!(
                    mesh.position(mf_geometry::VertexId(v))
                        .map(|p| p.map(f64::to_bits)),
                    Some(target.corner_position(v).map(f64::to_bits))
                );
            }
        }
    }
}

#[test]
fn touching_operands_are_valid_or_refused_by_name() {
    // Sharing a face, sharing an edge, sharing a corner, and a part
    // resting exactly on a flat top (Review Focus 1).
    let cases = [
        ("face", cube(1.0), translate([1.0, 0.0, 0.0])),
        ("edge", cube(1.0), translate([1.0, 1.0, 0.0])),
        ("corner", cube(1.0), translate([1.0, 1.0, 1.0])),
        ("resting", cube(0.5), translate([0.25, 0.25, 1.0])),
    ];
    for (name, tool, t) in cases {
        for op in ["union", "subtract", "intersect"] {
            match run(
                &cube(1.0),
                IDENTITY,
                &tool,
                t,
                &[("operation", op)],
                &RunContext::none(),
            ) {
                Ok(out) => {
                    // Valid by construction (D4 checked it); and a closed solid.
                    let d = inspect(result(&out).0);
                    assert_eq!(d.boundary_edges, 0, "{name} {op}");
                }
                Err(OpError::Failed { code, message }) => {
                    assert!(
                        [
                            "result_not_manifold",
                            "result_self_intersects",
                            "result_empty",
                            "no_overlap"
                        ]
                        .contains(&code),
                        "{name} {op}: {code}: {message}"
                    );
                    assert!(
                        message.contains(". "),
                        "{name} {op}: no next action in {message}"
                    );
                }
                Err(e) => panic!("{name} {op}: {e}"),
            }
        }
    }
}

#[test]
fn boolean_with_mirrored_tool_keeps_outward_normals() {
    // The tool is mirrored in x (determinant -1) and placed overlapping.
    let mirror = [-1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 2.5, 0.5, 0.5];
    let out = run(
        &cube(2.0),
        IDENTITY,
        &cube(1.0),
        mirror,
        &[],
        &RunContext::none(),
    )
    .unwrap();
    let (mesh, _) = result(&out);
    let v = volume(mesh);
    assert!((v - 8.5).abs() < 1e-12, "{v}");
    assert_eq!(inspect(mesh).inconsistent_orientation_edges, 0);
}

#[test]
fn subtract_on_open_target_is_refused_with_fill_holes_next_action() {
    let (code, message) = code(run(
        &mf_fixtures::cube_with_missing_face(),
        IDENTITY,
        &cube(0.5),
        translate([0.25, 0.25, -0.25]),
        &[("operation", "subtract")],
        &RunContext::none(),
    ));
    assert_eq!(code, "operand_not_closed");
    assert!(
        message.contains("Fill all holes, then combine"),
        "{message}"
    );
}

#[test]
fn union_onto_open_target_closes_a_covered_opening_locally() {
    // Two openings: one at the top, under the tool; one at the bottom.
    let target = sphere_without(|p| p[2] > 9.5 || p[2] < -9.5);
    assert_eq!(inspect(&target).boundary_loops.len(), 2);
    let tool = scaled(&uv_sphere(24, 48), 4.0);
    let out = run(
        &target,
        IDENTITY,
        &tool,
        translate([0.0, 0.0, 9.8]),
        &[],
        &RunContext::none(),
    )
    .unwrap();
    let (mesh, prov) = result(&out);
    let d = inspect(mesh);
    assert_eq!(d.boundary_loops.len(), 1, "only the bottom opening is left");
    assert_eq!(d.non_manifold_edges, 0);
    // The bottom opening's faces are untouched.
    let removed: HashSet<FaceId> = prov.removed_faces.iter().copied().collect();
    for (f, t) in target.faces() {
        if t.iter().all(|&v| target.corner_position(v)[2] < -5.0) {
            assert!(!removed.contains(&f));
            assert_eq!(mesh.face(f), Some(t));
        }
    }
}

#[test]
fn union_with_uncovered_opening_is_refused_as_opening_not_covered() {
    let target = sphere_without(|p| p[2] > 9.5);
    let tool = scaled(&uv_sphere(16, 32), 1.5);
    let (code, message) = code(run(
        &target,
        IDENTITY,
        &tool,
        translate([2.6, 0.0, 9.2]),
        &[],
        &RunContext::none(),
    ));
    assert_eq!(code, "opening_not_covered", "{message}");
    assert!(message.contains("Move the part to cover the opening, or fill the hole first"));
}

#[test]
fn opening_far_outside_the_window_doesnt_block_the_union() {
    let target = sphere_without(|p| p[2] < -9.5);
    let tool = cube(1.0);
    let out = run(
        &target,
        IDENTITY,
        &tool,
        translate([9.5, -0.5, -0.5]),
        &[],
        &RunContext::none(),
    )
    .unwrap();
    assert_eq!(inspect(result(&out).0).boundary_loops.len(), 1);
}

#[test]
fn cancel_at_every_stage_leaves_document_unchanged() {
    // Review Focus 3. A run cancelled at any stage produces nothing (the
    // command layer then changes no document state: MF-CMD-006, tested in
    // mf-command's `boolean_cancel_*`).
    let target = scaled(&uv_sphere(32, 64), 10.0);
    let before = target.content_hash();
    for stage in STAGES {
        let ctx = RunContext::cancelling_at(stage);
        let r = run(
            &target,
            IDENTITY,
            &cube(1.0),
            translate([9.5, -0.5, -0.5]),
            &[],
            &ctx,
        );
        assert!(
            matches!(r, Err(OpError::Cancelled)),
            "{stage}: {:?}",
            r.map(|o| o.report)
        );
        assert_eq!(target.content_hash(), before);
    }
    // And inside the kernel.
    let ctx = RunContext::cancelling_at("boolean: manifold");
    let r = run(
        &target,
        IDENTITY,
        &cube(1.0),
        translate([9.5, -0.5, -0.5]),
        &[],
        &ctx,
    );
    assert!(matches!(r, Err(OpError::Cancelled)));
}

#[test]
fn boolean_replays_exact_and_strict_at_1_and_n_threads() {
    let target = scaled(&uv_sphere(48, 96), 10.0);
    let tool = scaled(&uv_sphere(16, 32), 2.0);
    let hash = |threads: usize| {
        let (pool, _) = JobPool::new(Some(threads));
        pool.install(|| {
            let out = run(
                &target,
                IDENTITY,
                &tool,
                translate([9.0, 0.3, 0.1]),
                &[],
                &RunContext::none(),
            )
            .unwrap();
            let (mesh, prov) = result(&out);
            (
                mesh.content_hash(),
                mesh.structure_hash(),
                format!("{prov:?}"),
            )
        })
    };
    let one = hash(1);
    for n in [2, 4, 8] {
        assert!(hash(n) == one, "{n} threads differ from 1");
    }
}

#[test]
fn consumed_tool_is_removed_and_its_faces_are_sources() {
    let tool = cube(1.0);
    let out = run(
        &cube(2.0),
        IDENTITY,
        &tool,
        translate([1.5, 0.5, 0.5]),
        &[],
        &RunContext::none(),
    )
    .unwrap();
    assert!(
        out.changes
            .iter()
            .any(|c| matches!(c, ObjectChange::Remove { operand: 1 }))
    );
    let (_, prov) = result(&out);
    let from_tool: Vec<FaceId> = prov
        .created_faces
        .iter()
        .flat_map(|(_, s)| s.operands.iter().map(|&(k, f)| (k, f)))
        .filter(|&(k, _)| k == 1)
        .map(|(_, f)| f)
        .collect();
    assert!(!from_tool.is_empty());
    assert!(from_tool.iter().all(|&f| tool.is_face_live(f)));
    // Every created face names exactly one source.
    for (_, s) in &prov.created_faces {
        assert_eq!(s.faces.len() + s.operands.len(), 1, "{s:?}");
    }
    let kept = run(
        &cube(2.0),
        IDENTITY,
        &tool,
        translate([1.5, 0.5, 0.5]),
        &[("keep_tool", "true")],
        &RunContext::none(),
    )
    .unwrap();
    assert!(
        !kept
            .changes
            .iter()
            .any(|c| matches!(c, ObjectChange::Remove { .. }))
    );
}

/// A kernel that returns C++ Manifold's result with one vertex pushed
/// through the surface: a fault the validator must catch.
struct Faulty;

impl Kernel for Faulty {
    fn boolean(
        &self,
        a: &KernelMesh,
        b: &KernelMesh,
        op: BoolOp,
        ctx: &mf_runtime::RunContext,
    ) -> Result<KernelResult, KernelError> {
        let mut r = ManifoldCpp::default().boolean(a, b, op, ctx)?;
        // The vertex furthest along +x, pulled far back through the solid.
        let i = (0..r.positions.len())
            .max_by(|&i, &j| r.positions[i][0].total_cmp(&r.positions[j][0]))
            .unwrap();
        r.positions[i][0] -= 5.0;
        Ok(r)
    }

    fn id(&self) -> &'static str {
        "faulty"
    }
}

#[test]
fn validator_catches_an_injected_self_intersection() {
    let r = boolean(
        &cube(2.0),
        &translated(&cube(1.0), [1.5, 0.5, 0.5]),
        BoolOp::Union,
        &Faulty,
        &RunContext::none(),
    );
    match r {
        Err(BooleanError::ResultSelfIntersects(_) | BooleanError::ResultNotManifold(_)) => {}
        other => panic!("{other:?}"),
    }
}

/// A sphere with a small cube crossing its surface on the far (-x) side:
/// a scan that intersects itself away from where the tool goes.
fn self_crossing_sphere() -> Mesh {
    merge(&[
        scaled(&uv_sphere(48, 96), 10.0),
        translated(&cube(2.0), [-11.0, -1.0, -1.0]),
    ])
}

#[test]
fn self_crossing_far_from_tool_doesnt_block_the_boolean() {
    let out = run(
        &self_crossing_sphere(),
        IDENTITY,
        &cube(1.0),
        translate([9.5, -0.5, -0.5]),
        &[],
        &RunContext::none(),
    )
    .unwrap();
    assert!(result(&out).0.face_count() > 0);
}

#[test]
fn self_crossing_inside_the_window_is_refused_with_make_solid_next_action() {
    let (code, message) = code(run(
        &self_crossing_sphere(),
        IDENTITY,
        &cube(1.0),
        translate([-10.5, -1.5, 0.5]),
        &[],
        &RunContext::none(),
    ));
    assert_eq!(code, "operand_overlaps_itself", "{message}");
    assert!(message.contains("Run Make solid on it first, or move the tool"));
}

// ---- heavy (on demand: scripts/run-tests.sh's timing list) ----

fn fixtures_dir() -> PathBuf {
    std::env::var_os("MF_FIXTURES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(mf_fixtures::real::dir)
}

/// A sphere `part` placed half-sunk into the model's surface at its +x
/// extreme, sized to 1/10 of the model.
fn part_on(model: &Mesh, part_faces: u32) -> (Mesh, [f64; 12]) {
    let bb = model.bounding_box().unwrap();
    let size = bb.diagonal() / 10.0;
    let rings = ((part_faces / 2) as f64).sqrt().max(4.0) as u32;
    let part = scaled(&uv_sphere(rings / 2 * 2, rings * 2), size / 2.0);
    let (x, i) = model.vertices().map(|(v, p)| (p[0], v)).fold(
        (f64::NEG_INFINITY, mf_geometry::VertexId(0)),
        |a, b| {
            if b.0 > a.0 { b } else { a }
        },
    );
    let p = model.position(i).unwrap();
    (part, translate([x, p[1], p[2]]))
}

#[test]
fn messy_corpus_booleans_are_valid_or_named() {
    let dir = fixtures_dir();
    let mut results = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.starts_with("messy-") {
            continue;
        }
        let model = mf_io::read_path(&path).unwrap().mesh;
        let (part, t) = part_on(&model, 2000);
        for op in ["union", "subtract", "intersect"] {
            let start = Instant::now();
            let r = run(
                &model,
                IDENTITY,
                &part,
                t,
                &[("operation", op)],
                &RunContext::none(),
            );
            let verdict = match &r {
                Ok(_) => "valid".to_string(),
                Err(OpError::Failed { code, .. }) => code.to_string(),
                Err(e) => panic!("{name} {op}: {e}"),
            };
            eprintln!("{name} {op}: {verdict} in {:?}", start.elapsed());
            results.push((name.clone(), op, verdict));
        }
    }
    assert!(!results.is_empty(), "no messy models in {}", dir.display());
    let refused = results.iter().filter(|r| r.2 != "valid").count();
    eprintln!("{} of {} refused by name", refused, results.len());
}

fn median_of_3(mut f: impl FnMut() -> Duration) -> Duration {
    let mut t: Vec<Duration> = (0..3).map(|_| f()).collect();
    t.sort();
    t[1]
}

#[test]
fn union_30m_scan_with_50k_part_under_3s() {
    // ~30M triangles: a 3873 × 3873 UV sphere.
    let target = scaled(&uv_sphere(3873, 3873), 100.0);
    let (part, t) = part_on(&target, 50_000);
    let took = median_of_3(|| {
        let start = Instant::now();
        run(&target, IDENTITY, &part, t, &[], &RunContext::none()).unwrap();
        start.elapsed()
    });
    eprintln!("30M ∪ 50k: {took:?} (median of 3)");
    assert!(took < Duration::from_secs(3), "{took:?}");
}

#[test]
fn union_1m_by_1m_under_10s() {
    let target = scaled(&uv_sphere(708, 708), 10.0);
    let tool = scaled(&uv_sphere(708, 708), 10.0);
    let start = Instant::now();
    run(
        &target,
        IDENTITY,
        &tool,
        translate([7.0, 1.0, 0.5]),
        &[],
        &RunContext::none(),
    )
    .unwrap();
    let took = start.elapsed();
    eprintln!("1M ∪ 1M: {took:?}");
    assert!(took < Duration::from_secs(10), "{took:?}");
}

#[test]
fn cancel_within_250ms_in_kernel() {
    let target = scaled(&uv_sphere(708, 708), 10.0);
    let tool = scaled(&uv_sphere(708, 708), 10.0);
    let ctx = RunContext::none();
    let cancel = ctx.cancel.clone();
    let progress = ctx.progress.clone();
    let watcher = std::thread::spawn(move || {
        loop {
            if progress.snapshot().0 == "boolean: manifold" {
                let at = Instant::now();
                cancel.cancel();
                return at;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let r = run(
        &target,
        IDENTITY,
        &tool,
        translate([7.0, 1.0, 0.5]),
        &[],
        &ctx,
    );
    let stopped = Instant::now();
    let at = watcher.join().unwrap();
    assert!(matches!(r, Err(OpError::Cancelled)));
    let took = stopped - at;
    eprintln!("cancel in the kernel stopped in {took:?}");
    assert!(took < Duration::from_millis(250), "{took:?}");
}
