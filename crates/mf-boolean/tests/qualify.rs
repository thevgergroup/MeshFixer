#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Phase 1 Task 7: the kernel qualification gate for D1 (ADR-0002).
//!
//! 1. **Corpus** (`qualify_corpus`, heavy): every model of the M4 messy
//!    collection, unioned with a sphere at its centroid, unioned with
//!    itself translated by 10% of its size, and minus a box, gives a
//!    result that passes `validate_solid` or a named `KernelError`. No
//!    invalid output, no panic (caught here, in the test only), no hang.
//! 2. **Determinism**: identical output hashes at 1 and N threads, and
//!    across two runs (`qualify_determinism`, and inside the heavy tests).
//! 3. **Provenance**: every output triangle's `origin` names a real input
//!    face and lies on it; the output triangles on an input face never
//!    cover more than its area, and cover all of it (or none) where the
//!    other operand can't reach (relative error ≤ 1e-9)
//!    (`qualify_provenance`, and inside the heavy tests).
//! 4. **Cancel-to-stop ≤ 250 ms** on 1M × 1M (`qualify_cancel`, heavy).
//! 5. **Time and peak heap**: 1M × 1M overlapping union ≤ 10 s, and
//!    50k × 50k ≤ 0.5 s (`qualify_1m_union`, heavy).
//!
//! Results go to `artifacts/phase1/p1-m2/qualify.json` (or
//! `$MF_ARTIFACTS/p1-m2/qualify.json`), one key per test, with the
//! machine. The real models are read from `$MF_FIXTURES_DIR` (default
//! `fixtures/real`); a missing model fails the corpus test.
//! Its own binary: the counting allocator is process-wide.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use mf_boolean::{
    BoolOp, Kernel, KernelError, KernelMesh, KernelResult, ManifoldRobust, SolidError,
    validate_solid,
};
use mf_fixtures::uv_sphere;
use mf_metrics::{CountingAlloc, live_bytes, measure};
use mf_runtime::{JobPool, RunContext};
use serde_json::{Value, json};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn sphere(rings: u32, segments: u32, radius: f64, centre: [f64; 3], operand: u8) -> KernelMesh {
    let (p, t) = uv_sphere(rings, segments).to_arrays();
    let p = p
        .iter()
        .map(|v| [0, 1, 2].map(|k| centre[k] + radius * v[k]))
        .collect();
    KernelMesh::labelled(p, t, operand)
}

/// A UV sphere of about `n` triangles (`2 · segments · (rings − 1)`).
fn sphere_of(n: usize, centre: [f64; 3], operand: u8) -> KernelMesh {
    let segments = ((n as f64).sqrt()).round() as u32;
    let rings = (n as f64 / (2.0 * segments as f64)).round() as u32 + 1;
    sphere(rings, segments, 1.0, centre, operand)
}

fn boxed(lo: [f64; 3], hi: [f64; 3], operand: u8) -> KernelMesh {
    let (p, t) = mf_fixtures::cube(1.0).to_arrays();
    let p = p
        .iter()
        .map(|v| [0, 1, 2].map(|k| lo[k] + v[k] * (hi[k] - lo[k])))
        .collect();
    KernelMesh::labelled(p, t, operand)
}

fn hash(r: &KernelResult) -> String {
    let mut h = blake3::Hasher::new();
    for p in &r.positions {
        for c in p {
            h.update(&c.to_bits().to_le_bytes());
        }
    }
    for t in &r.triangles {
        for v in t {
            h.update(&v.to_le_bytes());
        }
    }
    for (o, f) in &r.origin {
        h.update(&[*o]);
        h.update(&f.to_le_bytes());
    }
    h.finalize().to_hex().to_string()
}

fn bbox(m: &KernelMesh) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in &m.positions {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

fn area(t: [[f64; 3]; 3]) -> f64 {
    0.5 * norm(cross(sub(t[1], t[0]), sub(t[2], t[0])))
}

/// Gate 3, measured on one result.
#[derive(Debug, Default)]
struct Provenance {
    /// Output triangles whose origin names no input face.
    unknown_origin: usize,
    /// Largest distance of an output vertex from its source face's plane,
    /// and from the face itself (outside its edges), over the inputs'
    /// bounding-box diagonal.
    max_plane_distance: f64,
    max_outside_distance: f64,
    /// Largest (covered − area) / area over all input faces.
    max_over_coverage: f64,
    /// Largest |covered − area| / area (or covered / area) over faces the
    /// other operand's box doesn't reach, which are kept whole or dropped.
    max_untouched_error: f64,
    untouched_faces: usize,
}

const COVERAGE_TOL: f64 = 1e-9;

impl Provenance {
    fn passes(&self) -> bool {
        self.unknown_origin == 0
            && self.max_plane_distance <= COVERAGE_TOL
            && self.max_outside_distance <= COVERAGE_TOL
            && self.max_over_coverage <= COVERAGE_TOL
            && self.max_untouched_error <= COVERAGE_TOL
    }

    fn json(&self) -> Value {
        json!({
            "passes": self.passes(),
            "unknown_origin": self.unknown_origin,
            "max_plane_distance_rel": self.max_plane_distance,
            "max_outside_distance_rel": self.max_outside_distance,
            "max_over_coverage_rel": self.max_over_coverage,
            "max_untouched_coverage_error_rel": self.max_untouched_error,
            "untouched_faces": self.untouched_faces,
            "tolerance": COVERAGE_TOL,
        })
    }
}

/// Distance from `p` to triangle `t` (0 inside), and to its plane.
fn distances(p: [f64; 3], t: [[f64; 3]; 3]) -> (f64, f64) {
    let n = cross(sub(t[1], t[0]), sub(t[2], t[0]));
    let len = norm(n);
    if len == 0.0 {
        return (0.0, 0.0);
    }
    let plane = dot(sub(p, t[0]), n) / len;
    // Outside distance: how far past each edge, in the plane.
    let mut outside: f64 = 0.0;
    for i in 0..3 {
        let (a, b) = (t[i], t[(i + 1) % 3]);
        let inward = cross(n, sub(b, a));
        let d = -dot(sub(p, a), inward) / norm(inward);
        outside = outside.max(d);
    }
    (plane.abs(), outside)
}

/// `a` and `b` labelled `(0, f)` and `(1, f)`.
fn provenance(a: &KernelMesh, b: &KernelMesh, r: &KernelResult) -> Provenance {
    let ops = [a, b];
    let (alo, ahi) = bbox(a);
    let (blo, bhi) = bbox(b);
    let lo = [0, 1, 2].map(|k| alo[k].min(blo[k]));
    let hi = [0, 1, 2].map(|k| ahi[k].max(bhi[k]));
    let diag = norm(sub(hi, lo));
    let mut p = Provenance::default();
    let mut covered = [vec![0.0; a.triangles.len()], vec![0.0; b.triangles.len()]];
    for (t, &(op, f)) in r.triangles.iter().zip(&r.origin) {
        let (op, f) = (op as usize, f as usize);
        if op > 1 || f >= ops[op].triangles.len() || ops[op].face_ids[f] != (op as u8, f as u32) {
            p.unknown_origin += 1;
            continue;
        }
        let src = ops[op].triangles[f].map(|v| ops[op].positions[v as usize]);
        let tri = t.map(|v| r.positions[v as usize]);
        for v in tri {
            let (plane, outside) = distances(v, src);
            p.max_plane_distance = p.max_plane_distance.max(plane / diag);
            p.max_outside_distance = p.max_outside_distance.max(outside / diag);
        }
        covered[op][f] += area(tri);
    }
    let reach = [(blo, bhi), (alo, ahi)];
    for op in 0..2 {
        let (olo, ohi) = reach[op];
        for (f, t) in ops[op].triangles.iter().enumerate() {
            let src = t.map(|v| ops[op].positions[v as usize]);
            let full = area(src);
            if full == 0.0 {
                continue;
            }
            let c = covered[op][f];
            p.max_over_coverage = p.max_over_coverage.max((c - full) / full);
            let untouched = (0..3)
                .any(|k| src.iter().all(|v| v[k] < olo[k]) || src.iter().all(|v| v[k] > ohi[k]));
            if untouched {
                p.untouched_faces += 1;
                let err = ((c - full).abs() / full).min(c / full);
                p.max_untouched_error = p.max_untouched_error.max(err);
            }
        }
    }
    p
}

/// Exactly zero-area (collinear or repeated-vertex) triangle.
fn is_degenerate(c: [[f64; 3]; 3]) -> bool {
    let proj = |p: [f64; 3], k: usize| match k {
        0 => robust::Coord { x: p[1], y: p[2] },
        1 => robust::Coord { x: p[2], y: p[0] },
        _ => robust::Coord { x: p[0], y: p[1] },
    };
    (0..3).all(|k| robust::orient2d(proj(c[0], k), proj(c[1], k), proj(c[2], k)) == 0.0)
}

/// Where a result's zero-area faces come from: on input faces that were
/// already degenerate (passed through), or created by the kernel on
/// faces that had area.
fn degenerate_sources(a: &KernelMesh, b: &KernelMesh, r: &KernelResult) -> Value {
    let ops = [a, b];
    let (mut passed_through, mut created) = (0, 0);
    for (t, &(op, f)) in r.triangles.iter().zip(&r.origin) {
        if !is_degenerate(t.map(|v| r.positions[v as usize])) {
            continue;
        }
        let m = ops[op as usize];
        if is_degenerate(m.triangles[f as usize].map(|v| m.positions[v as usize])) {
            passed_through += 1;
        } else {
            created += 1;
        }
    }
    json!({"on_degenerate_input_faces": passed_through, "created_on_faces_with_area": created})
}

fn threads_n() -> usize {
    mf_runtime::PoolChoice::for_request(None).threads
}

fn on_pool<R: Send>(threads: Option<usize>, f: impl FnOnce() -> R + Send) -> R {
    let (pool, _) = JobPool::new(threads);
    pool.install(f)
}

fn machine() -> Value {
    let sysctl = |key: &str| -> Option<String> {
        let out = std::process::Command::new("sysctl")
            .args(["-n", key])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpu": sysctl("machdep.cpu.brand_string"),
        "memory_bytes": sysctl("hw.memsize").and_then(|s| s.parse::<u64>().ok()),
        "cores": std::thread::available_parallelism().map(|n| n.get()).ok(),
        "pool_threads": threads_n(),
        "build": if cfg!(debug_assertions) {
            "test harness (manifold-rust and dashu at opt-level 3 without debug assertions; mf-boolean at opt-level 2)"
        } else {
            "release"
        },
        "kernel": ManifoldRobust.id(),
    })
}

fn results_path() -> PathBuf {
    match std::env::var_os("MF_ARTIFACTS") {
        Some(dir) => PathBuf::from(dir).join("p1-m2"),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/phase1/p1-m2"),
    }
    .join("qualify.json")
}

/// Writes `value` under `key` in the results file (other keys kept).
fn record(key: &str, value: Value) {
    let path = results_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut doc: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    doc["task"] = json!("Phase 1 Task 7: kernel qualification (D1, ADR-0002)");
    doc["machine"] = machine();
    doc[key] = value;
    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    println!("recorded {key} in {}", path.display());
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".into())
}

// ---------------------------------------------------------------------------
// Gates 2 and 3 on mid-size cases (fast)
// ---------------------------------------------------------------------------

#[test]
fn qualify_determinism() {
    let a = sphere_of(20_000, [0.0; 3], 0);
    let b = sphere_of(20_000, [0.5, 0.3, 0.2], 1);
    let mut cases = BTreeMap::new();
    for op in [BoolOp::Union, BoolOp::Subtract, BoolOp::Intersect] {
        let run = |threads| {
            on_pool(Some(threads), || {
                hash(
                    &ManifoldRobust
                        .boolean(&a, &b, op, &RunContext::none())
                        .unwrap(),
                )
            })
        };
        let n = threads_n().max(2);
        let hashes = [run(1), run(n), run(n), run(1)];
        assert!(hashes.iter().all(|h| *h == hashes[0]), "{op:?}: {hashes:?}");
        cases.insert(
            format!("{op:?}"),
            json!({"threads": [1, n, n, 1], "hash": hashes[0]}),
        );
    }
    record(
        "determinism_20k",
        json!({"passes": true, "triangles_per_operand": a.triangles.len(), "cases": cases}),
    );
}

#[test]
fn qualify_provenance() {
    let cases: Vec<(&str, KernelMesh, KernelMesh)> = vec![
        (
            "sphere_sphere_20k",
            sphere_of(20_000, [0.0; 3], 0),
            sphere_of(20_000, [0.5, 0.3, 0.2], 1),
        ),
        (
            "box_sphere",
            boxed([-1.0, -1.0, -1.0], [1.0, 1.0, 0.5], 0),
            sphere(32, 64, 0.8, [0.6, 0.1, 0.5], 1),
        ),
        (
            // Coplanar faces: the boxes share part of a wall.
            "box_box_coplanar",
            boxed([0.0; 3], [1.0; 3], 0),
            boxed([0.5, 0.25, 0.0], [1.5, 0.75, 1.0], 1),
        ),
    ];
    let mut out = BTreeMap::new();
    for (name, a, b) in &cases {
        for op in [BoolOp::Union, BoolOp::Subtract, BoolOp::Intersect] {
            let r = ManifoldRobust
                .boolean(a, b, op, &RunContext::none())
                .unwrap();
            validate_solid(&r.positions, &r.triangles, &RunContext::none())
                .unwrap_or_else(|e| panic!("{name} {op:?}: {e}"));
            let p = provenance(a, b, &r);
            assert!(p.passes(), "{name} {op:?}: {p:?}");
            out.insert(format!("{name}/{op:?}"), p.json());
        }
    }
    record("provenance", json!({"passes": true, "cases": out}));
}

// ---------------------------------------------------------------------------
// Gate 1: the messy corpus (heavy)
// ---------------------------------------------------------------------------

/// The M4 messy collection (`crates/mf-operations/tests/messy_corpus.rs`,
/// `fixtures/SOURCES.md`).
const MESSY: [&str; 15] = [
    "messy-inopos-131487.stl",
    "messy-inopos-131443.obj",
    "messy-verlan-dress-285440.obj",
    "messy-aztec-chief-384098.stl",
    "messy-moon-shot-45809.stl",
    "messy-walking-man-75275.stl",
    "messy-cube-achim-252785.stl",
    "messy-site3-scan-85538.stl",
    "messy-duplo-crossing-111959.stl",
    "messy-bunny-peel-298323.obj",
    "messy-cleopatras-needle-63942.stl",
    "messy-puritan-91945.stl",
    "messy-sullivan-1716279.stl",
    "messy-openrc-differential-100727.stl",
    "messy-winged-victory-376253.obj",
];

fn fixtures_dir() -> PathBuf {
    std::env::var_os("MF_FIXTURES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(mf_fixtures::real::dir)
}

/// One kernel call may take this long; past it, it is cancelled and
/// recorded as a hang (a gate failure).
fn hang_limit() -> Duration {
    Duration::from_secs(
        std::env::var("MF_QUALIFY_HANG_S")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300),
    )
}

/// The heap a kernel call may use before it is cancelled and recorded
/// as over the limit (a gate failure): this machine is shared.
fn heap_limit() -> u64 {
    let gb: f64 = std::env::var("MF_QUALIFY_HEAP_LIMIT_GB")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12.0);
    (gb * 1e9) as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    None,
    Hang,
    Heap,
}

/// Runs `f` with a context a watchdog cancels on a hang or past the heap
/// limit, catching panics; returns the result, wall time, peak heap and
/// why it was stopped.
fn guarded<T>(name: &str, f: impl FnOnce(&RunContext) -> T) -> (Result<T, String>, f64, u64, Stop) {
    let ctx = RunContext::none();
    let token = ctx.cancel.clone();
    let (done, wait) = channel::<()>();
    let limit = heap_limit();
    let hang = hang_limit();
    let watchdog = std::thread::spawn(move || {
        let start = Instant::now();
        loop {
            match wait.recv_timeout(Duration::from_millis(20)) {
                Err(RecvTimeoutError::Timeout) => {}
                _ => return Stop::None,
            }
            if live_bytes() > limit {
                token.cancel();
                return Stop::Heap;
            }
            if start.elapsed() > hang {
                token.cancel();
                return Stop::Hang;
            }
        }
    });
    let (result, phase) = measure(name, || catch_unwind(AssertUnwindSafe(|| f(&ctx))));
    // allow-swallow: a watchdog that already stopped the run has hung up.
    let _ = done.send(());
    let stop = watchdog.join().unwrap();
    (
        result.map_err(panic_message),
        phase.wall_s,
        phase.peak_bytes,
        stop,
    )
}

fn import(file: &str) -> KernelMesh {
    let path = fixtures_dir().join(file);
    let model = mf_io::read_model(&path)
        .unwrap_or_else(|e| panic!("{}: could not import: {e}", path.display()));
    let mesh = &model
        .objects
        .first()
        .unwrap_or_else(|| panic!("{file}: no objects"))
        .mesh;
    let (p, t) = mesh.to_arrays();
    KernelMesh::labelled(p, t, 0)
}

/// The three tools of gate 1 for model `a`.
fn tools(a: &KernelMesh) -> Vec<(&'static str, BoolOp, KernelMesh)> {
    let (lo, hi) = bbox(a);
    let size = sub(hi, lo);
    let diag = norm(size);
    let n = a.positions.len() as f64;
    let mut centroid = [0.0; 3];
    for p in &a.positions {
        for k in 0..3 {
            centroid[k] += p[k] / n;
        }
    }
    let shifted = KernelMesh {
        positions: a
            .positions
            .iter()
            .map(|p| [0, 1, 2].map(|k| p[k] + 0.1 * size[k]))
            .collect(),
        triangles: a.triangles.clone(),
        face_ids: a.face_ids.iter().map(|&(_, f)| (1, f)).collect(),
    };
    let quarter = size.map(|s| s / 4.0);
    vec![
        (
            "union_sphere_at_centroid",
            BoolOp::Union,
            sphere(32, 64, diag / 4.0, centroid, 1),
        ),
        ("union_self_translated_10pct", BoolOp::Union, shifted),
        (
            "subtract_box",
            BoolOp::Subtract,
            boxed(
                [0, 1, 2].map(|k| centroid[k] - quarter[k]),
                [0, 1, 2].map(|k| centroid[k] + quarter[k]),
                1,
            ),
        ),
    ]
}

#[test]
fn qualify_corpus() {
    let dir = fixtures_dir();
    let missing: Vec<_> = MESSY.iter().filter(|f| !dir.join(f).is_file()).collect();
    assert!(
        missing.is_empty(),
        "messy models missing from {} (set MF_FIXTURES_DIR, or run scripts/fetch-fixtures.sh): {missing:?}",
        dir.display()
    );
    // Smallest first, so the cheap results are in before the big ones.
    // `MF_QUALIFY_ONLY=a.stl,b.obj` runs a subset while iterating; the
    // record then says it is partial.
    let only: Option<Vec<String>> = std::env::var("MF_QUALIFY_ONLY")
        .ok()
        .map(|s| s.split(',').map(str::to_string).collect());
    let mut files: Vec<&str> = MESSY
        .iter()
        .copied()
        .filter(|f| only.as_ref().is_none_or(|o| o.iter().any(|x| x == f)))
        .collect();
    files.sort_by_key(|f| std::fs::metadata(dir.join(f)).unwrap().len());

    let mut models = Vec::new();
    let mut failures = Vec::new();
    let mut computed = 0;
    let mut refused = BTreeMap::<String, usize>::new();
    for file in files {
        let a = import(file);
        println!("{file}: {} triangles", a.triangles.len());
        let mut ops = Vec::new();
        for (tool, op, b) in tools(&a) {
            let label = format!("{file} {tool}");
            let (r, wall_s, peak, stop) =
                guarded(&label, |ctx| ManifoldRobust.boolean(&a, &b, op, ctx));
            let mut entry = json!({
                "tool": tool,
                "op": format!("{op:?}"),
                "tool_triangles": b.triangles.len(),
                "wall_s": wall_s,
                "peak_heap_bytes": peak,
            });
            match (r, stop) {
                (Err(panic), _) => {
                    entry["outcome"] = json!("panicked");
                    entry["message"] = json!(panic);
                    failures.push(format!("{label}: panicked: {panic}"));
                }
                (Ok(Err(KernelError::Cancelled)), Stop::Hang) => {
                    entry["outcome"] = json!("timed_out");
                    failures.push(format!("{label}: still running after {wall_s:.0} s"));
                }
                (Ok(Err(KernelError::Cancelled)), Stop::Heap) => {
                    entry["outcome"] = json!("heap_limit");
                    failures.push(format!(
                        "{label}: passed the {:.0} GB heap limit",
                        heap_limit() as f64 / 1e9
                    ));
                }
                (Ok(Err(e)), _) => {
                    entry["outcome"] = json!("refused");
                    entry["code"] = json!(e.code());
                    entry["message"] = json!(e.to_string());
                    *refused.entry(e.code().to_string()).or_default() += 1;
                }
                (Ok(Ok(result)), _) => {
                    computed += 1;
                    entry["triangles"] = json!(result.triangles.len());
                    let (v, v_wall, _, v_stop) = guarded(&format!("{label} validate"), |ctx| {
                        validate_solid(&result.positions, &result.triangles, ctx)
                    });
                    entry["validate_wall_s"] = json!(v_wall);
                    match (v, v_stop) {
                        (Ok(Ok(())), _) => entry["outcome"] = json!("valid"),
                        (Ok(Err(e)), Stop::None) => {
                            if let SolidError::DegenerateFace { .. } = e {
                                entry["degenerate"] = degenerate_sources(&a, &b, &result);
                            }
                            entry["outcome"] = json!("invalid");
                            entry["code"] = json!(e.code());
                            entry["message"] = json!(e.to_string());
                            failures.push(format!("{label}: invalid output: {e}"));
                        }
                        (Ok(Err(_)), s) => {
                            entry["outcome"] = json!("validation_stopped");
                            failures.push(format!("{label}: validation stopped ({s:?})"));
                        }
                        (Err(panic), _) => {
                            entry["outcome"] = json!("validator_panicked");
                            failures.push(format!("{label}: validator panicked: {panic}"));
                        }
                    }
                    let p = provenance(&a, &b, &result);
                    if !p.passes() {
                        failures.push(format!("{label}: provenance {p:?}"));
                    }
                    entry["provenance"] = p.json();
                    entry["hash"] = json!(hash(&result));
                    // Gate 2 on real geometry, where it is affordable.
                    if a.triangles.len() <= 500_000 {
                        let again = on_pool(Some(1), || {
                            ManifoldRobust.boolean(&a, &b, op, &RunContext::none())
                        });
                        let same = again.as_ref().map(hash).ok() == Some(hash(&result));
                        entry["same_hash_at_1_thread"] = json!(same);
                        if !same {
                            failures.push(format!("{label}: different output at 1 thread"));
                        }
                    }
                }
            }
            println!("  {tool}: {entry}");
            ops.push(entry);
        }
        models.push(json!({"file": file, "triangles": a.triangles.len(), "ops": ops}));
    }
    let passes = failures.is_empty();
    record(
        "corpus",
        json!({
            "passes": passes,
            "partial": only.is_some(),
            "computed": computed,
            "refused": refused,
            "failures": failures,
            "hang_limit_s": hang_limit().as_secs(),
            "heap_limit_bytes": heap_limit(),
            "models": models,
        }),
    );
    assert!(passes, "corpus gate failed:\n{}", failures.join("\n"));
    assert!(
        only.is_none(),
        "MF_QUALIFY_ONLY ran a subset: not a qualification"
    );
}

// ---------------------------------------------------------------------------
// Gates 5 (and 2, 3 at scale): time and peak heap (heavy)
// ---------------------------------------------------------------------------

fn timed_union(a: &KernelMesh, b: &KernelMesh, threads: Option<usize>) -> (KernelResult, f64, u64) {
    let (r, phase) = measure("union", || {
        on_pool(threads, || {
            ManifoldRobust
                .boolean(a, b, BoolOp::Union, &RunContext::none())
                .unwrap()
        })
    });
    (r, phase.wall_s, phase.peak_bytes)
}

#[test]
fn qualify_1m_union() {
    let mut failures = Vec::new();

    // 50k × 50k: three runs, gated on the median.
    let a = sphere_of(50_000, [0.0; 3], 0);
    let b = sphere_of(50_000, [0.5, 0.3, 0.2], 1);
    let mut walls = Vec::new();
    let mut peak50 = 0;
    let mut hashes = Vec::new();
    for _ in 0..3 {
        let (r, wall, peak) = timed_union(&a, &b, None);
        walls.push(wall);
        peak50 = peak50.max(peak);
        hashes.push(hash(&r));
    }
    let mut sorted = walls.clone();
    sorted.sort_by(f64::total_cmp);
    let median50 = sorted[1];
    if median50 > 0.5 {
        failures.push(format!("50k × 50k union: median {median50:.3} s > 0.5 s"));
    }
    let r50 = timed_union(&a, &b, None).0;
    validate_solid(&r50.positions, &r50.triangles, &RunContext::none()).unwrap();
    let p50 = provenance(&a, &b, &r50);
    if !p50.passes() {
        failures.push(format!("50k provenance: {p50:?}"));
    }
    let entry50 = json!({
        "triangles_per_operand": a.triangles.len(),
        "result_triangles": r50.triangles.len(),
        "wall_s": walls,
        "median_wall_s": median50,
        "limit_s": 0.5,
        "peak_heap_bytes": peak50,
        "deterministic_across_runs": hashes.iter().all(|h| *h == hashes[0]),
        "provenance": p50.json(),
    });
    println!("50k × 50k: {entry50}");
    drop((a, b, r50));

    // 1M × 1M.
    let a = sphere_of(1_000_000, [0.0; 3], 0);
    let b = sphere_of(1_000_000, [0.5, 0.3, 0.2], 1);
    let (r1, wall1, peak1) = timed_union(&a, &b, None);
    println!("1M × 1M: {wall1:.2} s, peak {:.2} GB", peak1 as f64 / 1e9);
    if wall1 > 10.0 {
        failures.push(format!("1M × 1M union: {wall1:.2} s > 10 s"));
    }
    let (v, v_phase) = measure("validate", || {
        validate_solid(&r1.positions, &r1.triangles, &RunContext::none())
    });
    if let Err(e) = &v {
        failures.push(format!("1M × 1M union: invalid output: {e}"));
    }
    let p1 = provenance(&a, &b, &r1);
    if !p1.passes() {
        failures.push(format!("1M provenance: {p1:?}"));
    }
    let h1 = hash(&r1);
    let tris1 = r1.triangles.len();
    drop(r1);
    let (r2, wall2, _) = timed_union(&a, &b, None);
    let same_run = hash(&r2) == h1;
    drop(r2);
    let (r3, wall_1t, _) = timed_union(&a, &b, Some(1));
    let same_1t = hash(&r3) == h1;
    drop(r3);
    if !same_run || !same_1t {
        failures.push(format!(
            "1M × 1M union not deterministic: second run same {same_run}, 1 thread same {same_1t}"
        ));
    }
    let entry1 = json!({
        "triangles_per_operand": a.triangles.len(),
        "result_triangles": tris1,
        "wall_s": wall1,
        "second_run_wall_s": wall2,
        "one_thread_wall_s": wall_1t,
        "limit_s": 10.0,
        "peak_heap_bytes": peak1,
        "validate": v.as_ref().map(|_| "valid".to_string()).unwrap_or_else(|e| e.to_string()),
        "validate_wall_s": v_phase.wall_s,
        "validate_peak_heap_bytes": v_phase.peak_bytes,
        "provenance": p1.json(),
        "hash": h1,
        "same_hash_second_run": same_run,
        "same_hash_one_thread": same_1t,
    });
    println!("1M × 1M: {entry1}");
    let passes = failures.is_empty();
    record(
        "union_timing",
        json!({"passes": passes, "failures": failures, "50k": entry50, "1m": entry1}),
    );
    assert!(passes, "{}", failures.join("\n"));
}

// ---------------------------------------------------------------------------
// Gate 4: cancel-to-stop on 1M × 1M (heavy)
// ---------------------------------------------------------------------------

#[test]
fn qualify_cancel() {
    let a = Arc::new(sphere_of(1_000_000, [0.0; 3], 0));
    let b = Arc::new(sphere_of(1_000_000, [0.5, 0.3, 0.2], 1));
    let (pool, _) = JobPool::new(None);
    let mut points = Vec::new();
    let mut worst = Duration::ZERO;
    let mut cancelled_runs = 0;
    for delay_ms in [20u64, 100, 300, 600, 1000, 1500, 2000, 3000, 4000, 6000] {
        let ctx = RunContext::none();
        let (tx, rx) = channel();
        let (a2, b2, ctx2) = (Arc::clone(&a), Arc::clone(&b), ctx.clone());
        let finished = Arc::new(AtomicBool::new(false));
        let fin = Arc::clone(&finished);
        pool.spawn(move || {
            let r = catch_unwind(AssertUnwindSafe(|| {
                ManifoldRobust.boolean(&a2, &b2, BoolOp::Union, &ctx2)
            }));
            fin.store(true, Ordering::SeqCst);
            tx.send((r.map_err(panic_message), Instant::now())).unwrap();
        });
        std::thread::sleep(Duration::from_millis(delay_ms));
        let stage = ctx.progress.snapshot();
        let at = Instant::now();
        let was_running = !finished.load(Ordering::SeqCst);
        ctx.cancel.cancel();
        let (result, ended) = rx.recv().unwrap();
        let latency = ended.saturating_duration_since(at);
        let outcome = match &result {
            Err(p) => format!("panicked: {p}"),
            Ok(Err(KernelError::Cancelled)) => "cancelled".into(),
            Ok(Err(e)) => format!("error: {e}"),
            Ok(Ok(_)) => "completed".into(),
        };
        if was_running {
            cancelled_runs += 1;
            worst = worst.max(latency);
            assert!(
                matches!(result, Ok(Err(KernelError::Cancelled))),
                "cancel at {delay_ms} ms: {outcome}"
            );
        }
        let point = json!({
            "cancel_after_ms": delay_ms,
            "running_at_cancel": was_running,
            "stage_at_cancel": stage.0,
            "fraction_at_cancel": stage.1,
            "cancel_to_stop_ms": latency.as_secs_f64() * 1e3,
            "outcome": outcome,
        });
        println!("{point}");
        points.push(point);
    }
    let passes = worst <= Duration::from_millis(250) && cancelled_runs >= 3;
    record(
        "cancel",
        json!({
            "passes": passes,
            "limit_ms": 250,
            "worst_cancel_to_stop_ms": worst.as_secs_f64() * 1e3,
            "cancelled_runs": cancelled_runs,
            "points": points,
        }),
    );
    assert!(
        passes,
        "worst cancel-to-stop {worst:?} over {cancelled_runs} cancelled runs"
    );
}
