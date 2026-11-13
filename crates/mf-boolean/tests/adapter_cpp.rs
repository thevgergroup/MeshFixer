#![cfg(feature = "manifold-cpp")]
#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The C++ Manifold adapter (Task 7b): results, refusals, provenance,
//! cancel, progress and thread-count determinism on small cases. The
//! qualification gates are in `qualify.rs`.

use mf_boolean::{
    BoolOp, Kernel, KernelError, KernelMesh, KernelResult, ManifoldCpp, validate_solid,
};
use mf_fixtures::{cube, cube_with_missing_face, translated};
use mf_geometry::Mesh;
use mf_runtime::{JobPool, RunContext};

fn operand(m: &Mesh, operand: u8) -> KernelMesh {
    let (p, t) = m.to_arrays();
    KernelMesh::labelled(p, t, operand)
}

fn volume(r: &KernelResult) -> f64 {
    r.triangles
        .iter()
        .map(|t| {
            let [a, b, c] = t.map(|v| r.positions[v as usize]);
            (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum()
}

fn cubes() -> (KernelMesh, KernelMesh) {
    (
        operand(&cube(1.0), 0),
        operand(&translated(&cube(1.0), [0.5, 0.5, 0.5]), 1),
    )
}

#[test]
fn overlapping_cubes_give_exact_volumes_and_valid_solids() {
    let (a, b) = cubes();
    for (op, v) in [
        (BoolOp::Union, 2.0 - 0.125),
        (BoolOp::Subtract, 1.0 - 0.125),
        (BoolOp::Intersect, 0.125),
    ] {
        let r = ManifoldCpp::default()
            .boolean(&a, &b, op, &RunContext::none())
            .unwrap();
        assert!((volume(&r) - v).abs() < 1e-12, "{op:?}: {}", volume(&r));
        validate_solid(&r.positions, &r.triangles, &RunContext::none()).unwrap();
        assert_eq!(r.origin.len(), r.triangles.len());
    }
}

#[test]
fn origin_names_the_caller_labels_and_lies_on_the_face() {
    let (mut a, mut b) = cubes();
    a.face_ids = (0..12).map(|f| (7, 100 + f)).collect();
    b.face_ids = (0..12).map(|f| (9, 200 + f)).collect();
    let r = ManifoldCpp::default()
        .boolean(&a, &b, BoolOp::Subtract, &RunContext::none())
        .unwrap();
    let (mut seen_a, mut seen_b) = (false, false);
    for (t, &(op, f)) in r.triangles.iter().zip(&r.origin) {
        let (src, i) = match op {
            7 => {
                seen_a = true;
                (&a, f - 100)
            }
            9 => {
                seen_b = true;
                (&b, f - 200)
            }
            _ => panic!("unknown operand label {op} on {t:?}"),
        };
        let [p, q, s] = src.triangles[i as usize].map(|v| src.positions[v as usize]);
        for v in t.map(|v| r.positions[v as usize]) {
            let d = robust::orient3d(c(p), c(q), c(s), c(v));
            assert_eq!(d, 0.0, "output vertex {v:?} off its source face {i}");
        }
    }
    assert!(seen_a && seen_b);
}

fn c(p: [f64; 3]) -> robust::Coord3D<f64> {
    robust::Coord3D {
        x: p[0],
        y: p[1],
        z: p[2],
    }
}

#[test]
fn open_operands_are_refused_by_name() {
    let open = operand(&cube_with_missing_face(), 0);
    let closed = operand(&cube(1.0), 1);
    let ctx = RunContext::none();
    assert_eq!(
        ManifoldCpp::default().boolean(&open, &closed, BoolOp::Union, &ctx),
        Err(KernelError::NotClosed { operand: 0 })
    );
    assert_eq!(
        ManifoldCpp::default().boolean(&closed, &open, BoolOp::Union, &ctx),
        Err(KernelError::NotClosed { operand: 1 })
    );
}

#[test]
fn an_edge_on_four_faces_is_split_on_import() {
    // Two cubes sharing an edge by vertex index: closed, but four faces
    // on one edge. C++ Manifold accepts it by duplicating the edge's
    // vertices (so the solids touch along it); the volume is kept.
    let (mut p, mut t) = cube(1.0).to_arrays();
    let (q, u) = translated(&cube(1.0), [1.0, 1.0, 0.0]).to_arrays();
    let base = p.len() as u32;
    let remap = |v: u32| match v {
        0 => 2,
        4 => 6,
        v => v + base,
    };
    p.extend(q);
    t.extend(u.iter().map(|f| f.map(remap)));
    let a = KernelMesh::labelled(p, t, 0);
    let b = operand(&translated(&cube(1.0), [5.0, 0.0, 0.0]), 1);
    let r = ManifoldCpp::default()
        .boolean(&a, &b, BoolOp::Union, &RunContext::none())
        .unwrap();
    assert!((volume(&r) - 3.0).abs() < 1e-12, "{}", volume(&r));
}

#[test]
fn bad_indices_are_refused_not_panicked() {
    let (mut a, b) = cubes();
    a.triangles[0][0] = 99;
    assert!(matches!(
        ManifoldCpp::default().boolean(&a, &b, BoolOp::Union, &RunContext::none()),
        Err(KernelError::Internal(_))
    ));
}

#[test]
fn a_cancelled_context_returns_cancelled() {
    let (a, b) = cubes();
    let ctx = RunContext::none();
    ctx.cancel.cancel();
    assert_eq!(
        ManifoldCpp::default().boolean(&a, &b, BoolOp::Union, &ctx),
        Err(KernelError::Cancelled)
    );
}

#[test]
fn progress_ends_at_one() {
    let (a, b) = cubes();
    let ctx = RunContext::none();
    ManifoldCpp::default()
        .boolean(&a, &b, BoolOp::Union, &ctx)
        .unwrap();
    assert_eq!(ctx.progress.snapshot(), ("boolean: manifold", 1.0));
}

#[test]
fn results_are_identical_at_one_and_many_threads() {
    let a = operand(&mf_fixtures::uv_sphere(48, 96), 0);
    let b = operand(
        &translated(&mf_fixtures::uv_sphere(48, 96), [0.5, 0.3, 0.2]),
        1,
    );
    let run = |threads| {
        let (pool, _) = JobPool::new(Some(threads));
        pool.install(|| {
            ManifoldCpp::default()
                .boolean(&a, &b, BoolOp::Union, &RunContext::none())
                .unwrap()
        })
    };
    let one = run(1);
    assert_eq!(one, run(4));
    assert_eq!(one, run(1));
}

#[test]
fn a_self_intersecting_operand_is_refused_by_name() {
    // C++ Manifold assumes operands that don't intersect themselves; on
    // ones that do, Task 7b measured invalid output. The adapter checks
    // exactly first and refuses by name.
    let a = operand(
        &mf_fixtures::merge(&[cube(1.0), translated(&cube(1.0), [0.5, 0.25, 0.125])]),
        0,
    );
    let b = operand(&translated(&cube(1.0), [0.25, 0.25, 0.25]), 1);
    match ManifoldCpp::default().boolean(&a, &b, BoolOp::Union, &RunContext::none()) {
        Err(e @ KernelError::SelfIntersecting { operand: 0, faces }) => {
            assert!(faces >= 8, "{faces}");
            assert_eq!(e.code(), "self_intersecting_operand");
        }
        other => panic!("{other:?}"),
    }
    match ManifoldCpp::default().boolean(&b, &a, BoolOp::Union, &RunContext::none()) {
        Err(KernelError::SelfIntersecting { operand: 1, .. }) => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_open_operand_is_named_open_even_if_it_intersects_itself() {
    // Not closed comes first, as for manifold-rust: it is the more basic
    // defect, and the one the user fixes first.
    let a = operand(
        &mf_fixtures::merge(&[
            cube_with_missing_face(),
            translated(&cube(1.0), [0.5, 0.25, 0.125]),
        ]),
        0,
    );
    let b = operand(&translated(&cube(1.0), [5.0, 0.0, 0.0]), 1);
    assert_eq!(
        ManifoldCpp::default().boolean(&a, &b, BoolOp::Union, &RunContext::none()),
        Err(KernelError::NotClosed { operand: 0 })
    );
}
