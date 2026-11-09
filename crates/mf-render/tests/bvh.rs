#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Accelerated picking (MF-RENDER-004): the BVH finds exactly what the
//! brute-force ray cast finds, a face pick at 5M triangles takes < 16 ms,
//! and the build stops promptly when cancelled.

use std::time::{Duration, Instant};

use glam::{DMat4, DQuat, DVec3, DVec4};
use mf_fixtures::{DamageSpec, damaged_sphere};
use mf_geometry::{FaceId, Mesh};
use mf_render::{Bvh, OrbitCamera, edge_occluded, pick_loop, ray_pick};
use mf_runtime::RunContext;

/// MF-RENDER-004: a face pick at 5M triangles.
const PICK_BUDGET: Duration = Duration::from_millis(16);
/// MF-RT-002: cancelled work stops within 250 ms.
const CANCEL_BUDGET: Duration = Duration::from_millis(250);

struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn signed(&mut self) -> f64 {
        self.unit() * 2.0 - 1.0
    }
    fn point(&mut self, r: f64) -> DVec3 {
        DVec3::new(self.signed(), self.signed(), self.signed()) * r
    }
}

/// A damaged sphere (holes, islands, unwelded corners) with every seventh
/// face deleted, so the BVH has dead face slots to skip.
fn test_mesh() -> Mesh {
    let mesh = damaged_sphere(&DamageSpec::for_triangles(20_000, 3));
    let mut edit = mesh.edit();
    for f in (0..mesh.face_slots()).step_by(7) {
        edit.remove_face(FaceId(f)).unwrap();
    }
    edit.finish().unwrap().0
}

#[test]
fn bvh_matches_brute_force_on_random_rays() {
    let mesh = test_mesh();
    let bvh = Bvh::build(&mesh, &RunContext::none()).unwrap();
    let models = [
        DMat4::IDENTITY,
        // Rotated, non-uniformly scaled, mirrored and far from the origin.
        DMat4::from_scale_rotation_translation(
            DVec3::new(2.0, -1.0, 0.5),
            DQuat::from_axis_angle(DVec3::new(1.0, 2.0, 3.0).normalize(), 0.7),
            DVec3::new(1.0e5, -3.0, 4.0),
        ),
    ];
    let vertices: Vec<[f64; 3]> = mesh.vertices().map(|(_, p)| p).collect();
    let mut rng = SplitMix(42);
    let (mut hits, mut ties) = (0, 0);
    for i in 0..10_000 {
        let model = models[i % 2];
        // Object-space ray: from outside the sphere toward a point near
        // it, or (every tenth ray) exactly through a vertex, where faces
        // tie on distance.
        let from = rng.point(1.0).normalize_or_zero() * 3.0;
        let toward = if i % 10 == 0 {
            ties += 1;
            DVec3::from(vertices[(rng.next() % vertices.len() as u64) as usize])
        } else {
            rng.point(1.2)
        };
        let o = (model * DVec4::from((from, 1.0))).truncate();
        let t = (model * DVec4::from((toward, 1.0))).truncate();
        let d = (t - o).normalize();
        let expected = ray_pick(&mesh, &model, o.to_array(), d.to_array());
        let got = bvh.ray_pick(&mesh, o.to_array(), d.to_array(), &model);
        assert_eq!(got, expected, "ray {i}: from {o:?} along {d:?}");
        hits += usize::from(expected.is_some());
    }
    assert!(hits > 5_000, "most rays hit the mesh: {hits}");
    assert!(ties > 0);
}

#[test]
fn bvh_occlusion_matches_brute_force() {
    let mesh = test_mesh();
    let bvh = Bvh::build(&mesh, &RunContext::none()).unwrap();
    let model = DMat4::from_scale_rotation_translation(
        DVec3::splat(3.0),
        DQuat::from_rotation_z(1.1),
        DVec3::new(-7.0, 2.0, 0.0),
    );
    let mut rng = SplitMix(7);
    let (mut hidden, mut shown) = (0, 0);
    for i in 0..2_000 {
        // From outside to a point on a random face, skipping faces that
        // touch one of that face's vertices (as an edge's own faces are).
        let eye = (model * DVec4::from((rng.point(1.0).normalize_or_zero() * 3.0, 1.0))).truncate();
        let f = loop {
            let f = FaceId((rng.next() % u64::from(mesh.face_slots())) as u32);
            if let Some(t) = mesh.face(f) {
                break t;
            }
        };
        let [a, b, c] = f.map(|v| DVec3::from(mesh.corner_position(v)));
        let p = (model * DVec4::from(((a + b + c) / 3.0, 1.0))).truncate();
        let skip = [f[0], f[1]];
        let expected = edge_occluded(&mesh, None, &model, eye, p, skip);
        let got = edge_occluded(&mesh, Some(&bvh), &model, eye, p, skip);
        assert_eq!(got, expected, "segment {i}");
        if expected {
            hidden += 1;
        } else {
            shown += 1;
        }
    }
    assert!(
        hidden > 100 && shown > 100,
        "{hidden} hidden, {shown} shown"
    );
}

/// The same tree however many threads build it (big enough that its
/// subtrees are built in parallel).
#[test]
fn bvh_is_deterministic() {
    let mesh = damaged_sphere(&DamageSpec::for_triangles(300_000, 9));
    let build = |threads| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| Bvh::build(&mesh, &RunContext::none()).unwrap())
    };
    let one = build(1);
    assert_eq!(one, build(4));
    assert_eq!(one, build(4));
    assert_eq!(one.face_count(), mesh.face_count());
}

fn median_and_p95(mut times: Vec<Duration>) -> (Duration, Duration) {
    times.sort_unstable();
    let p95 = times[(times.len() * 95).div_ceil(100) - 1];
    (times[times.len() / 2], p95)
}

#[test]
fn face_pick_under_16ms_at_5m() {
    let started = Instant::now();
    let mesh = damaged_sphere(&DamageSpec::for_triangles(5_000_000, 11));
    println!(
        "5M mesh: {} triangles, generated in {:?}",
        mesh.triangle_count(),
        started.elapsed()
    );
    let built = Instant::now();
    let bvh = Bvh::build(&mesh, &RunContext::none()).unwrap();
    let build_time = built.elapsed();
    let per_face = bvh.heap_bytes() as f64 / mesh.triangle_count() as f64;
    println!(
        "BVH: built in {build_time:?}, {} nodes, {} bytes ({per_face:.1} B/face)",
        bvh.node_count(),
        bvh.heap_bytes()
    );
    assert!(per_face <= 64.0, "{per_face:.1} B/face");
    // The app has the topology already (the analysis computed it).
    mesh.topology();

    let bb = mesh.bounding_box().unwrap();
    let cam = OrbitCamera::fit(&bb);
    let origin = DVec3::from(bb.center());
    let size = (1440.0, 900.0);
    let model = DMat4::IDENTITY;
    let mut rng = SplitMix(5);
    let (mut ray_times, mut pick_times) = (Vec::new(), Vec::new());
    let mut hits = 0;
    for i in 0..50 {
        // Over the sphere (it fills the middle of the fitted view), and
        // now and then past its edge.
        let (r, a) = (
            rng.unit().sqrt() * 0.27 * size.1,
            rng.unit() * std::f64::consts::TAU,
        );
        let px = (size.0 * 0.5 + r * a.cos(), size.1 * 0.5 + r * a.sin());
        // A click: the nearest visible hole edge, else the face under it.
        let start = Instant::now();
        let edge = pick_loop(&mesh, &bvh, &model, &cam, origin, size, px, 8.0);
        let (o, d) = cam.ray(origin, size, px);
        let ray_start = Instant::now();
        let face = bvh.ray_pick(&mesh, o, d, &model);
        let end = Instant::now();
        ray_times.push(end - ray_start);
        pick_times.push(end - start);
        hits += usize::from(face.is_some() || edge.is_some());
        // The first few agree with brute force (too slow to do for all).
        if i < 3 {
            assert_eq!(face, ray_pick(&mesh, &model, o, d));
        }
    }
    assert!(hits > 25, "most picks land on the sphere: {hits}");
    let (ray_median, ray_p95) = median_and_p95(ray_times);
    let (median, p95) = median_and_p95(pick_times);
    println!(
        "5M face pick (BVH ray cast): median {ray_median:?}, p95 {ray_p95:?}; \
         whole pick (hole edges + occlusion + ray): median {median:?}, p95 {p95:?}"
    );
    assert!(median < PICK_BUDGET, "median pick {median:?}");
}

#[test]
fn bvh_build_is_cancellable() {
    let mesh = damaged_sphere(&DamageSpec::for_triangles(2_000_000, 5));
    // Cancelled before it starts: nothing is built.
    let ctx = RunContext::none();
    ctx.cancel.cancel();
    assert!(Bvh::build(&mesh, &ctx).is_err());

    // Uncancelled, it checks for cancellation at least every 100 ms.
    let (ctx, probe) = RunContext::instrumented();
    Bvh::build(&mesh, &ctx).unwrap();
    assert!(probe.checkpoints() > 10, "{}", probe.checkpoints());
    assert!(
        probe.max_gap() <= Duration::from_millis(100),
        "longest gap between checkpoints {:?} (at {:?})",
        probe.max_gap(),
        probe.worst()
    );

    // Cancelled mid-build, it stops promptly.
    let ctx = RunContext::none();
    let cancel = ctx.cancel.clone();
    let (stopped, result) = std::thread::scope(|s| {
        let build = s.spawn(|| {
            let r = Bvh::build(&mesh, &ctx);
            (Instant::now(), r.is_err())
        });
        std::thread::sleep(Duration::from_millis(30));
        let cancelled = Instant::now();
        cancel.cancel();
        let (stopped, err) = build.join().unwrap();
        (stopped.saturating_duration_since(cancelled), err)
    });
    assert!(result, "a cancelled build reports Cancelled");
    assert!(stopped < CANCEL_BUDGET, "stopped {stopped:?} after cancel");
}
