#![cfg(feature = "manifold-rust")]
#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The `manifold-rust` adapter (D1): results, refusals, provenance, cancel
//! and progress on small cases. The qualification gates are in
//! `qualify.rs`.

use std::time::{Duration, Instant};

use mf_boolean::{
    BoolOp, Kernel, KernelError, KernelMesh, KernelResult, ManifoldRobust, bridge, validate_solid,
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
    let k = ManifoldRobust;
    for (op, v) in [
        (BoolOp::Union, 2.0 - 0.125),
        (BoolOp::Subtract, 1.0 - 0.125),
        (BoolOp::Intersect, 0.125),
    ] {
        let r = k.boolean(&a, &b, op, &RunContext::none()).unwrap();
        assert!((volume(&r) - v).abs() < 1e-12, "{op:?}: {}", volume(&r));
        validate_solid(&r.positions, &r.triangles, &RunContext::none()).unwrap();
        assert_eq!(r.origin.len(), r.triangles.len());
    }
}

#[test]
fn origin_names_the_caller_labels_of_the_input_faces() {
    let (mut a, mut b) = cubes();
    a.face_ids = (0..12).map(|f| (7, 100 + f)).collect();
    b.face_ids = (0..12).map(|f| (9, 200 + f)).collect();
    let r = ManifoldRobust
        .boolean(&a, &b, BoolOp::Subtract, &RunContext::none())
        .unwrap();
    let mut seen_a = false;
    let mut seen_b = false;
    for (t, &(op, f)) in r.triangles.iter().zip(&r.origin) {
        match op {
            7 => {
                assert!((100..112).contains(&f));
                seen_a = true;
            }
            9 => {
                assert!((200..212).contains(&f));
                seen_b = true;
            }
            _ => panic!("unknown operand label {op} on {t:?}"),
        }
        // The triangle lies on the plane of the face it names.
        let src = if op == 7 { &a } else { &b };
        let i = (f - if op == 7 { 100 } else { 200 }) as usize;
        let [p, q, s] = src.triangles[i].map(|v| src.positions[v as usize]);
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
    let k = ManifoldRobust;
    let ctx = RunContext::none();
    assert_eq!(
        k.boolean(&open, &closed, BoolOp::Union, &ctx),
        Err(KernelError::NotClosed { operand: 0 })
    );
    assert_eq!(
        k.boolean(&closed, &open, BoolOp::Union, &ctx),
        Err(KernelError::NotClosed { operand: 1 })
    );
}

#[test]
fn bad_indices_are_refused_not_panicked() {
    let (mut a, b) = cubes();
    a.triangles[0][0] = 99;
    assert!(matches!(
        ManifoldRobust.boolean(&a, &b, BoolOp::Union, &RunContext::none()),
        Err(KernelError::Internal(_))
    ));
    let (mut a, b) = cubes();
    a.face_ids.pop();
    assert!(matches!(
        ManifoldRobust.boolean(&a, &b, BoolOp::Union, &RunContext::none()),
        Err(KernelError::Internal(_))
    ));
}

#[test]
fn a_cancelled_context_returns_cancelled() {
    let (a, b) = cubes();
    let ctx = RunContext::none();
    ctx.cancel.cancel();
    assert_eq!(
        ManifoldRobust.boolean(&a, &b, BoolOp::Union, &ctx),
        Err(KernelError::Cancelled)
    );
}

#[test]
fn the_bridge_forwards_cancel_within_10ms() {
    let ctx = RunContext::none();
    let (token, guard) = bridge(&ctx);
    std::thread::sleep(Duration::from_millis(20));
    assert!(!token.is_cancelled());
    let t0 = Instant::now();
    ctx.cancel.cancel();
    while !token.is_cancelled() {
        assert!(
            t0.elapsed() < Duration::from_millis(10),
            "not forwarded in 10 ms"
        );
        std::thread::yield_now();
    }
    let t1 = Instant::now();
    drop(guard);
    assert!(
        t1.elapsed() < Duration::from_millis(50),
        "guard drop is prompt"
    );
}

#[test]
fn progress_is_reported_and_ends_at_one() {
    let (a, b) = cubes();
    let ctx = RunContext::none();
    let (_p, reader) = ctx.progress.channel();
    ManifoldRobust.boolean(&a, &b, BoolOp::Union, &ctx).unwrap();
    let mut last = 0.0;
    let mut n = 0;
    while let Some((stage, f)) = reader.try_recv() {
        assert!(stage.starts_with("boolean"), "{stage}");
        assert!(f >= last, "progress went back: {last} -> {f} ({stage})");
        last = f;
        n += 1;
    }
    assert!(n >= 2, "{n} updates");
    assert_eq!(last, 1.0);
}

#[test]
fn results_are_identical_at_one_and_many_threads() {
    let a = operand(&mf_fixtures::uv_sphere(24, 48), 0);
    let b = operand(
        &translated(&mf_fixtures::uv_sphere(24, 48), [0.5, 0.3, 0.2]),
        1,
    );
    let run = |threads| {
        let (pool, _) = JobPool::new(Some(threads));
        pool.install(|| {
            ManifoldRobust
                .boolean(&a, &b, BoolOp::Union, &RunContext::none())
                .unwrap()
        })
    };
    let one = run(1);
    assert_eq!(one, run(4));
    assert_eq!(one, run(1));
}
