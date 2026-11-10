#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Exact intersection analysis (MF-MODEL-002, Phase 1 Task 9).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use mf_fixtures::{cube, scaled, translated, uv_sphere};
use mf_geometry::affine::{to_dmat4, tool_to_target};
use mf_geometry::{
    Bvh, Mesh, TriTri, intersections_between, self_intersections, tri_tri_intersect,
};
use mf_runtime::{JobPool, RunContext};

const IDENTITY: [f64; 12] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];

fn translate(d: [f64; 3]) -> [f64; 12] {
    let mut t = IDENTITY;
    t[9..].copy_from_slice(&d);
    t
}

const O: [f64; 3] = [0.0, 0.0, 0.0];
const X: [f64; 3] = [1.0, 0.0, 0.0];
const Y: [f64; 3] = [0.0, 1.0, 0.0];

#[test]
fn tri_tri_exact_cases() {
    let a = [O, X, Y];
    // Crossing: b pierces a's interior.
    assert_eq!(
        tri_tri_intersect(a, [[0.2, 0.2, -1.0], [0.2, 0.2, 1.0], [0.3, -1.0, 0.0]]),
        TriTri::Crossing
    );
    // Generic crossing in every vertex order and swapped roles.
    let b = [[0.25, 0.25, -1.0], [0.3, 0.2, 1.0], [-0.5, 0.1, 0.5]];
    for r in 0..3 {
        let rb = [b[r], b[(r + 1) % 3], b[(r + 2) % 3]];
        let fb = [rb[0], rb[2], rb[1]];
        for (x, y) in [(a, rb), (rb, a), (a, fb), (fb, a)] {
            assert_eq!(tri_tri_intersect(x, y), TriTri::Crossing);
        }
    }
    // Touching at a vertex (b's corner on a's interior, b above).
    assert_eq!(
        tri_tri_intersect(a, [[0.2, 0.2, 0.0], [0.2, 0.2, 1.0], [0.3, -1.0, 1.0]]),
        TriTri::Touching
    );
    // Sharing a vertex position only.
    assert_eq!(
        tri_tri_intersect(a, [O, [-1.0, 0.0, 0.0], [0.0, -1.0, 0.0]]),
        TriTri::Touching
    );
    // Sharing an edge, bent.
    assert_eq!(
        tri_tri_intersect(a, [O, X, [0.5, -0.5, 0.5]]),
        TriTri::Touching
    );
    // b's edge lies on a's edge, b standing up.
    assert_eq!(
        tri_tri_intersect(a, [[0.2, 0.0, 0.0], [0.6, 0.0, 0.0], [0.4, 0.0, 1.0]]),
        TriTri::Touching
    );
    // Coplanar overlap, and coplanar side by side.
    assert_eq!(
        tri_tri_intersect(a, [[0.1, 0.1, 0.0], [0.9, 0.0, 0.0], [0.0, 0.9, 0.0]]),
        TriTri::Crossing
    );
    assert_eq!(tri_tri_intersect(a, a), TriTri::Crossing);
    assert_eq!(
        tri_tri_intersect(a, [X, [1.0, 1.0, 0.0], Y]),
        TriTri::Touching
    );
    assert_eq!(
        tri_tri_intersect(a, [[1.0, 1.0, 0.0], [2.0, 1.0, 0.0], [1.0, 2.0, 0.0]]),
        TriTri::Disjoint
    );
    // Near misses at 1e-300 scale (unit triangles, offsets of 1e-300):
    // exact predicates still decide. (Triangles 1e-300 across would
    // underflow f64 products; offsets don't.)
    let e = 1e-300;
    let above = |z: f64| [[0.2, 0.2, z], [0.2, 0.2, 1.0], [0.3, -1.0, 1.0]];
    assert_eq!(tri_tri_intersect(a, above(e)), TriTri::Disjoint);
    assert_eq!(tri_tri_intersect(a, above(0.0)), TriTri::Touching);
    assert_eq!(tri_tri_intersect(a, above(-e)), TriTri::Crossing);
    // An edge passing 1e-300 outside a's corner.
    let past = [[-e, -e, -1.0], [-e, -e, 1.0], [-1.0, -1.0, 0.0]];
    assert_eq!(tri_tri_intersect(a, past), TriTri::Disjoint);
}

#[test]
fn tri_tri_agrees_with_the_closed_test_on_random_pairs() {
    // Random pairs in general position never just touch: crossing iff
    // the closed triangles meet.
    let mut s: u64 = 0x2545_f491_4f6c_dd1d;
    let mut r = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut crossing = 0;
    for _ in 0..20_000 {
        let mut tri = || [[r(), r(), r()], [r(), r(), r()], [r(), r(), r()]];
        let (a, b) = (tri(), tri());
        match tri_tri_intersect(a, b) {
            TriTri::Crossing => crossing += 1,
            TriTri::Disjoint => {}
            TriTri::Touching => panic!("{a:?} {b:?}"),
        }
        assert_eq!(
            tri_tri_intersect(b, a) == TriTri::Crossing,
            tri_tri_intersect(a, b) == TriTri::Crossing
        );
    }
    assert!(crossing > 1000, "{crossing}");
}

fn between(a: &Mesh, b: &Mesh, b_t: [f64; 12]) -> mf_geometry::IntersectionReport {
    let ctx = RunContext::none();
    let (ab, bb) = (Bvh::build(a, &ctx).unwrap(), Bvh::build(b, &ctx).unwrap());
    let t = tool_to_target(&b_t, &IDENTITY).unwrap();
    intersections_between(a, &ab, b, &bb, &to_dmat4(&t), &ctx).unwrap()
}

#[test]
fn intersections_found_regardless_of_thread_count() {
    let a = scaled(&uv_sphere(48, 96), 10.0);
    let b = scaled(&uv_sphere(24, 48), 3.0);
    let at = translate([9.0, 0.5, 0.25]);
    let one = JobPool::new(Some(1)).0.install(|| between(&a, &b, at));
    assert!(one.pair_count > 0);
    assert_eq!(one.regions.len(), 1, "one ring where they cross");
    assert!(one.regions[0].extent > 1.0);
    for n in [2, 8] {
        assert_eq!(JobPool::new(Some(n)).0.install(|| between(&a, &b, at)), one);
    }
    // The smaller side may be either: the same pairs, swapped.
    let swapped = between(&b, &a, translate([-9.0, -0.5, -0.25]));
    assert_eq!(swapped.pair_count, one.pair_count);
}

#[test]
fn detection_agrees_with_boolean_no_overlap() {
    // Whenever detection reports none, mesh.boolean reports no_overlap;
    // and when it reports some, the boolean doesn't. Including a tool
    // that only touches, and one inside the target.
    let target = cube(2.0);
    for (name, at, scale) in [
        ("apart", [3.0, 0.0, 0.0], 1.0),
        ("crossing", [1.5, 0.5, 0.5], 1.0),
        ("inside", [0.5, 0.5, 0.5], 1.0),
        ("touching a face", [2.0, 0.5, 0.5], 1.0),
        ("crossing a corner", [1.75, 1.75, 1.75], 0.5),
    ] {
        let tool = scaled(&cube(1.0), scale);
        let report = between(&target, &tool, translate(at));
        // Touching is reported separately from crossing; either is contact.
        let found = report.pair_count > 0 || report.touching_pairs > 0;
        let tool_in_frame = translated(&tool, at);
        let r = mf_boolean::boolean(
            &target,
            &tool_in_frame,
            mf_boolean::BoolOp::Union,
            &mf_boolean::ManifoldCpp::default(),
            &RunContext::none(),
        );
        let no_overlap = matches!(r, Err(mf_boolean::BooleanError::NoOverlap));
        if !found {
            assert!(
                no_overlap,
                "{name}: detection found none, the boolean said {r:?}"
            );
        } else {
            assert!(!no_overlap, "{name}");
        }
    }
}

fn fixtures_dir() -> PathBuf {
    std::env::var_os("MF_FIXTURES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(mf_fixtures::real::dir)
}

#[test]
fn self_intersections_found_on_messy_self_intersecting_export() {
    // Thingi10K 111959 (duplo crossing): 10,253 self-intersections in the
    // dataset's count.
    let path = fixtures_dir().join("messy-duplo-crossing-111959.stl");
    if !path.is_file() {
        mf_fixtures::skip::fixture_missing(
            "self_intersections_found_on_messy_self_intersecting_export",
            &path,
        );
        return;
    }
    let m = mf_io::read_path(&path).unwrap().mesh;
    let ctx = RunContext::none();
    let r = self_intersections(&m, &Bvh::build(&m, &ctx).unwrap(), &ctx).unwrap();
    assert!(r.pair_count > 100, "{}", r.pair_count);
    assert!(!r.regions.is_empty());
    // A clean sphere has none.
    let s = scaled(&uv_sphere(32, 64), 5.0);
    assert_eq!(
        self_intersections(&s, &Bvh::build(&s, &ctx).unwrap(), &ctx)
            .unwrap()
            .pair_count,
        0
    );
}

// ---- timing (on demand) ----

#[test]
fn intersections_30m_vs_50k_under_1s() {
    let a = scaled(&uv_sphere(3873, 3873), 100.0);
    let b = scaled(&uv_sphere(158, 158), 10.0);
    let ctx = RunContext::none();
    let ab = Bvh::build(&a, &ctx).unwrap();
    let bb = Bvh::build(&b, &ctx).unwrap();
    let t = to_dmat4(&translate([100.0, 0.0, 0.0]));
    let start = Instant::now();
    let r = intersections_between(&a, &ab, &b, &bb, &t, &ctx).unwrap();
    let took = start.elapsed();
    eprintln!("30M vs 50k: {took:?}, {} pairs", r.pair_count);
    assert!(r.pair_count > 0);
    assert!(took < Duration::from_secs(1), "{took:?}");
}

#[test]
fn self_intersections_5m_cancellable() {
    let m = scaled(&uv_sphere(1581, 1581), 10.0);
    let bvh = Bvh::build(&m, &RunContext::none()).unwrap();
    let (ctx, probe) = RunContext::instrumented();
    let cancel = ctx.cancel.clone();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        let at = Instant::now();
        cancel.cancel();
        at
    });
    let r = self_intersections(&m, &bvh, &ctx);
    let stopped = Instant::now();
    let at = stopper.join().unwrap();
    assert!(r.is_err(), "finished before the cancel");
    eprintln!(
        "cancel stopped in {:?}; worst checkpoint gap {:?}",
        stopped - at,
        probe.max_gap()
    );
    assert!(stopped - at < Duration::from_millis(250));
    assert!(probe.max_gap() < Duration::from_millis(100));
}
