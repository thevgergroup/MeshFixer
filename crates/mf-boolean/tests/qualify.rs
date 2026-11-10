#![cfg(feature = "manifold-rust")]
#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Phase 1 Tasks 7 and 7b: the kernel qualification gate for D1 (ADR-0002),
//! run for every kernel compiled in: `manifold-rust` (always) and the C++
//! Manifold library (feature `manifold-cpp`), with its operand check
//! (`cpp`) and without (`cpp-unchecked`, to measure what the check
//! prevents). `MF_QUALIFY_KERNEL=rust,cpp,cpp-unchecked` picks a subset.
//!
//! Every result goes through the product pipeline: the kernel, then the
//! degenerate post-pass (`remove_created_degenerates`), then D4's
//! `validate_boolean`, which refuses *new* self-intersections (those not
//! already present between the same input faces).
//!
//! 1. **Corpus** (`qualify_corpus`, heavy): every model of the M4 messy
//!    collection, unioned with a sphere at its centroid, unioned with
//!    itself translated by 10% of its size, and minus a box: a valid
//!    result or a named `KernelError`. No invalid output, no panic (caught
//!    here, in the test only), no hang.
//! 2. **Determinism**: identical output hashes at 1 and N threads, and
//!    across two runs (`qualify_determinism`, and inside the heavy tests).
//! 3. **Provenance**: every output triangle's `origin` names a real input
//!    face and lies on it; the output on an input face never covers more
//!    than its area, and covers all of it (or none) where the other operand
//!    can't reach, except on faces that intersect their own operand
//!    (relative error ≤ 1e-9) (`qualify_provenance`, and the heavy tests).
//! 4. **Cancel-to-stop ≤ 250 ms** on the largest D5 window case
//!    (`qualify_windows`, heavy).
//! 5. **Time through a D5 window** (`qualify_windows`, heavy): a 50k part
//!    unioned onto winged victory (5.7M) and onto the Archaeopteryx scan
//!    (1M) ≤ 3 s end to end; a plane slab cut through a 1M model ≤ 10 s;
//!    50k × 50k ≤ 0.5 s. The whole-model 1M × 1M is informational
//!    (`qualify_1m_union`, heavy).
//!
//! Results go to `artifacts/phase1/p1-m2/qualify.json` (or
//! `$MF_ARTIFACTS/p1-m2/qualify.json`) under `kernels.<kernel>.<gate>`,
//! with the machine and the load average at each timing. The real models
//! are read from `$MF_FIXTURES_DIR` (default `fixtures/real`); a missing
//! model fails the heavy tests. Its own binary: the counting allocator is
//! process-wide.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use mf_boolean::{
    BoolOp, DegenerateCleanup, Kernel, KernelError, KernelMesh, KernelResult, ManifoldRobust,
    SolidError, remove_created_degenerates, self_intersecting_faces, validate_boolean,
};
use mf_fixtures::uv_sphere;
use mf_metrics::{CountingAlloc, live_bytes, measure};
use mf_runtime::{JobPool, RunContext};
use rayon::prelude::*;
use serde_json::{Value, json};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

// ---------------------------------------------------------------------------
// Kernels
// ---------------------------------------------------------------------------

/// Every kernel compiled in, by short name, filtered by `MF_QUALIFY_KERNEL`.
fn kernels() -> Vec<(&'static str, Arc<dyn Kernel>)> {
    #[allow(unused_mut)]
    let mut all: Vec<(&'static str, Arc<dyn Kernel>)> = vec![("rust", Arc::new(ManifoldRobust))];
    #[cfg(feature = "manifold-cpp")]
    {
        all.push(("cpp", Arc::new(mf_boolean::ManifoldCpp::default())));
        all.push((
            "cpp-unchecked",
            Arc::new(mf_boolean::ManifoldCpp::unchecked()),
        ));
    }
    let only: Option<Vec<String>> = std::env::var("MF_QUALIFY_KERNEL")
        .ok()
        .map(|s| s.split(',').map(|k| k.trim().to_string()).collect());
    all.into_iter()
        .filter(|(name, _)| only.as_ref().is_none_or(|o| o.iter().any(|x| x == name)))
        .collect()
}

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
fn sphere_n(n: usize, radius: f64, centre: [f64; 3], operand: u8) -> KernelMesh {
    let segments = ((n as f64).sqrt()).round() as u32;
    let rings = (n as f64 / (2.0 * segments as f64)).round() as u32 + 1;
    sphere(rings, segments, radius, centre, operand)
}

fn sphere_of(n: usize, centre: [f64; 3], operand: u8) -> KernelMesh {
    sphere_n(n, 1.0, centre, operand)
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

fn bbox_of(p: &[[f64; 3]]) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in p {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}

fn bbox(m: &KernelMesh) -> ([f64; 3], [f64; 3]) {
    bbox_of(&m.positions)
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

/// The load average (1, 5, 15 min), recorded next to every timing.
fn load_average() -> Value {
    let text = std::process::Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .or_else(|| std::fs::read_to_string("/proc/loadavg").ok())
        .unwrap_or_default();
    let v: Vec<f64> = text
        .split_whitespace()
        .filter_map(|w| w.trim_matches(['{', '}']).parse().ok())
        .take(3)
        .collect();
    json!(v)
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
    /// Faces that intersect their own operand are excluded (Task 7b): a
    /// kernel that resolves self-intersections cuts them legitimately.
    max_untouched_error: f64,
    untouched_faces: usize,
    untouched_self_intersecting_excluded: usize,
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
            "untouched_self_intersecting_excluded": self.untouched_self_intersecting_excluded,
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
    let mut outside: f64 = 0.0;
    for i in 0..3 {
        let (a, b) = (t[i], t[(i + 1) % 3]);
        let inward = cross(n, sub(b, a));
        let d = -dot(sub(p, a), inward) / norm(inward);
        outside = outside.max(d);
    }
    (plane.abs(), outside)
}

/// Gate 3 for `a op b`. Labels are `face_ids`; `self_hit[i][f]` is whether
/// face `f` of operand `i` intersects its own operand.
fn provenance(
    a: &KernelMesh,
    b: &KernelMesh,
    r: &KernelResult,
    self_hit: [&[bool]; 2],
) -> Provenance {
    let ops = [a, b];
    let mut index: BTreeMap<(u8, u32), (usize, usize)> = BTreeMap::new();
    for (i, m) in ops.iter().enumerate() {
        for (f, l) in m.face_ids.iter().enumerate() {
            index.insert(*l, (i, f));
        }
    }
    let (alo, ahi) = bbox(a);
    let (blo, bhi) = bbox(b);
    let lo = [0, 1, 2].map(|k| alo[k].min(blo[k]));
    let hi = [0, 1, 2].map(|k| ahi[k].max(bhi[k]));
    let diag = norm(sub(hi, lo));
    let mut p = Provenance::default();
    let mut covered = [vec![0.0; a.triangles.len()], vec![0.0; b.triangles.len()]];
    for (t, o) in r.triangles.iter().zip(&r.origin) {
        let Some(&(op, f)) = index.get(o) else {
            p.unknown_origin += 1;
            continue;
        };
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
                if self_hit[op].get(f).copied().unwrap_or(false) {
                    p.untouched_self_intersecting_excluded += 1;
                    continue;
                }
                p.untouched_faces += 1;
                let err = ((c - full).abs() / full).min(c / full);
                p.max_untouched_error = p.max_untouched_error.max(err);
            }
        }
    }
    p
}

fn self_hits(m: &KernelMesh) -> Vec<bool> {
    self_intersecting_faces(&m.positions, &m.triangles, &RunContext::none()).unwrap()
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

/// Where a result's remaining zero-area faces come from.
fn degenerate_sources(a: &KernelMesh, b: &KernelMesh, r: &KernelResult) -> Value {
    let ops = [a, b];
    let mut index: BTreeMap<(u8, u32), (usize, usize)> = BTreeMap::new();
    for (i, m) in ops.iter().enumerate() {
        for (f, l) in m.face_ids.iter().enumerate() {
            index.insert(*l, (i, f));
        }
    }
    let (mut passed_through, mut created) = (0, 0);
    for (t, o) in r.triangles.iter().zip(&r.origin) {
        if !is_degenerate(t.map(|v| r.positions[v as usize])) {
            continue;
        }
        let Some(&(i, f)) = index.get(o) else {
            created += 1;
            continue;
        };
        let m = ops[i];
        if is_degenerate(m.triangles[f].map(|v| m.positions[v as usize])) {
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
            "test harness (manifold-rust and dashu at opt-level 3 without debug assertions; C++ Manifold and oneTBB Release (-O3); mf-boolean at opt-level 2)"
        } else {
            "release"
        },
    })
}

fn results_path() -> PathBuf {
    match std::env::var_os("MF_ARTIFACTS") {
        Some(dir) => PathBuf::from(dir).join("p1-m2"),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../artifacts/phase1/p1-m2"),
    }
    .join("qualify.json")
}

/// Writes `value` under `kernels.<kernel>.<key>` (other keys kept).
fn record(kernel: &str, kernel_id: &str, key: &str, value: Value) {
    let path = results_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut doc: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    if doc.get("kernels").is_none() {
        // A Task 7 file: start the Task 7b layout.
        doc = json!({});
    }
    doc["task"] = json!("Phase 1 Task 7b: kernel re-qualification (D1, ADR-0002)");
    doc["machine"] = machine();
    doc["kernels"][kernel]["id"] = json!(kernel_id);
    doc["kernels"][kernel][key] = value;
    std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    println!("recorded kernels.{kernel}.{key} in {}", path.display());
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".into())
}

/// The product pipeline after the kernel: the degenerate post-pass, then
/// D4's check. Times both.
struct Checked {
    cleanup: Result<DegenerateCleanup, SolidError>,
    verdict: Result<(), SolidError>,
    cleanup_s: f64,
    validate_s: f64,
}

impl Checked {
    fn json(&self) -> Value {
        json!({
            "cleanup": match &self.cleanup {
                Ok(c) => json!({"flipped": c.flipped, "collapsed": c.collapsed, "remaining": c.remaining}),
                Err(e) => json!(e.to_string()),
            },
            "cleanup_s": self.cleanup_s,
            "validate_s": self.validate_s,
            "verdict": match &self.verdict {
                Ok(()) => json!("valid"),
                Err(e) => json!({"code": e.code(), "message": e.to_string()}),
            },
        })
    }
}

fn check(r: &mut KernelResult, a: &KernelMesh, b: &KernelMesh, ctx: &RunContext) -> Checked {
    let t = Instant::now();
    let cleanup = remove_created_degenerates(r, [a, b], ctx);
    let cleanup_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let verdict = match &cleanup {
        Ok(_) => validate_boolean(r, [a, b], ctx),
        Err(e) => Err(e.clone()),
    };
    Checked {
        cleanup,
        verdict,
        cleanup_s,
        validate_s: t.elapsed().as_secs_f64(),
    }
}

// ---------------------------------------------------------------------------
// Gates 2 and 3 on mid-size cases (fast)
// ---------------------------------------------------------------------------

#[test]
fn qualify_determinism() {
    let a = sphere_of(20_000, [0.0; 3], 0);
    let b = sphere_of(20_000, [0.5, 0.3, 0.2], 1);
    for (name, k) in kernels() {
        let mut cases = BTreeMap::new();
        for op in [BoolOp::Union, BoolOp::Subtract, BoolOp::Intersect] {
            let run = |threads| {
                on_pool(Some(threads), || {
                    hash(&k.boolean(&a, &b, op, &RunContext::none()).unwrap())
                })
            };
            let n = threads_n().max(2);
            let hashes = [run(1), run(n), run(n), run(1)];
            assert!(
                hashes.iter().all(|h| *h == hashes[0]),
                "{name} {op:?}: {hashes:?}"
            );
            cases.insert(
                format!("{op:?}"),
                json!({"threads": [1, n, n, 1], "hash": hashes[0]}),
            );
        }
        record(
            name,
            k.id(),
            "determinism_20k",
            json!({"passes": true, "triangles_per_operand": a.triangles.len(), "cases": cases}),
        );
    }
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
    for (name, k) in kernels() {
        let mut out = BTreeMap::new();
        let mut failures = Vec::new();
        for (case, a, b) in &cases {
            let hits = [self_hits(a), self_hits(b)];
            for op in [BoolOp::Union, BoolOp::Subtract, BoolOp::Intersect] {
                let mut r = k.boolean(a, b, op, &RunContext::none()).unwrap();
                let c = check(&mut r, a, b, &RunContext::none());
                if let Err(e) = &c.verdict {
                    failures.push(format!("{case} {op:?}: {e}"));
                }
                let p = provenance(a, b, &r, [&hits[0], &hits[1]]);
                if !p.passes() {
                    failures.push(format!("{case} {op:?}: provenance {p:?}"));
                }
                let mut entry = p.json();
                entry["checked"] = c.json();
                out.insert(format!("{case}/{op:?}"), entry);
            }
        }
        let passes = failures.is_empty();
        record(
            name,
            k.id(),
            "provenance",
            json!({"passes": passes, "failures": failures, "cases": out}),
        );
        assert!(passes, "{name}: {}", failures.join("\n"));
    }
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

const WINGED_VICTORY: &str = "messy-winged-victory-376253.obj";
const ARCHAEOPTERYX: &str = "archaeopteryx.obj";

fn fixtures_dir() -> PathBuf {
    std::env::var_os("MF_FIXTURES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(mf_fixtures::real::dir)
}

fn require_fixtures(files: &[&str]) {
    let dir = fixtures_dir();
    let missing: Vec<_> = files.iter().filter(|f| !dir.join(f).is_file()).collect();
    assert!(
        missing.is_empty(),
        "real models missing from {} (set MF_FIXTURES_DIR, or run scripts/fetch-fixtures.sh): {missing:?}",
        dir.display()
    );
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

fn import(file: &str, operand: u8) -> KernelMesh {
    let path = fixtures_dir().join(file);
    let model = mf_io::read_model(&path)
        .unwrap_or_else(|e| panic!("{}: could not import: {e}", path.display()));
    let mesh = &model
        .objects
        .first()
        .unwrap_or_else(|| panic!("{file}: no objects"))
        .mesh;
    let (p, t) = mesh.to_arrays();
    KernelMesh::labelled(p, t, operand)
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
    require_fixtures(&MESSY);
    let dir = fixtures_dir();
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
    let kernels = kernels();

    let mut models: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    let mut failures: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut computed: BTreeMap<&str, usize> = BTreeMap::new();
    let mut refused: BTreeMap<&str, BTreeMap<String, usize>> = BTreeMap::new();
    for file in files {
        let a = import(file, 0);
        let hits_a = self_hits(&a);
        println!(
            "{file}: {} triangles, {} intersect their own model",
            a.triangles.len(),
            hits_a.iter().filter(|&&h| h).count()
        );
        let tools = tools(&a);
        let hits_b: Vec<Vec<bool>> = tools.iter().map(|(_, _, b)| self_hits(b)).collect();
        for (name, k) in &kernels {
            let mut ops = Vec::new();
            for ((tool, op, b), hits_b) in tools.iter().zip(&hits_b) {
                let (op, label) = (*op, format!("{name} {file} {tool}"));
                let load = load_average();
                let (r, wall_s, peak, stop) = guarded(&label, |ctx| k.boolean(&a, b, op, ctx));
                let mut entry = json!({
                    "tool": tool,
                    "op": format!("{op:?}"),
                    "tool_triangles": b.triangles.len(),
                    "wall_s": wall_s,
                    "load_average": load,
                    "peak_heap_bytes": peak,
                });
                let fail = failures.entry(name).or_default();
                match (r, stop) {
                    (Err(panic), _) => {
                        entry["outcome"] = json!("panicked");
                        entry["message"] = json!(panic);
                        fail.push(format!("{label}: panicked: {panic}"));
                    }
                    (Ok(Err(KernelError::Cancelled)), Stop::Hang) => {
                        entry["outcome"] = json!("timed_out");
                        fail.push(format!("{label}: still running after {wall_s:.0} s"));
                    }
                    (Ok(Err(KernelError::Cancelled)), Stop::Heap) => {
                        entry["outcome"] = json!("heap_limit");
                        fail.push(format!(
                            "{label}: passed the {:.0} GB heap limit",
                            heap_limit() as f64 / 1e9
                        ));
                    }
                    (Ok(Err(e)), _) => {
                        entry["outcome"] = json!("refused");
                        entry["code"] = json!(e.code());
                        entry["message"] = json!(e.to_string());
                        *refused
                            .entry(name)
                            .or_default()
                            .entry(e.code().to_string())
                            .or_default() += 1;
                    }
                    (Ok(Ok(mut result)), _) => {
                        *computed.entry(name).or_default() += 1;
                        entry["kernel_triangles"] = json!(result.triangles.len());
                        let (c, _, _, c_stop) = guarded(&format!("{label} check"), |ctx| {
                            check(&mut result, &a, b, ctx)
                        });
                        match (c, c_stop) {
                            (Ok(c), Stop::None) => {
                                entry["checked"] = c.json();
                                match &c.verdict {
                                    Ok(()) => entry["outcome"] = json!("valid"),
                                    Err(e) => {
                                        if let SolidError::DegenerateFace { .. } = e {
                                            entry["degenerate"] =
                                                degenerate_sources(&a, b, &result);
                                        }
                                        entry["outcome"] = json!("invalid");
                                        entry["code"] = json!(e.code());
                                        entry["message"] = json!(e.to_string());
                                        fail.push(format!("{label}: invalid output: {e}"));
                                    }
                                }
                            }
                            (Ok(_), s) => {
                                entry["outcome"] = json!("validation_stopped");
                                fail.push(format!("{label}: validation stopped ({s:?})"));
                            }
                            (Err(panic), _) => {
                                entry["outcome"] = json!("validator_panicked");
                                fail.push(format!("{label}: validator panicked: {panic}"));
                            }
                        }
                        entry["triangles"] = json!(result.triangles.len());
                        let p = provenance(&a, b, &result, [&hits_a, hits_b]);
                        if !p.passes() {
                            fail.push(format!("{label}: provenance {p:?}"));
                        }
                        entry["provenance"] = p.json();
                        entry["hash"] = json!(hash(&result));
                        // Gate 2 on real geometry, where it is affordable.
                        if a.triangles.len() <= 500_000 {
                            let again = on_pool(Some(1), || {
                                k.boolean(&a, b, op, &RunContext::none()).map(|mut r| {
                                    check(&mut r, &a, b, &RunContext::none());
                                    r
                                })
                            });
                            let same = again.as_ref().map(hash).ok() == Some(hash(&result));
                            entry["same_hash_at_1_thread"] = json!(same);
                            if !same {
                                fail.push(format!("{label}: different output at 1 thread"));
                            }
                        }
                    }
                }
                println!("  {name} {tool}: {entry}");
                ops.push(entry);
            }
            models.entry(name).or_default().push(json!({
                "file": file,
                "triangles": a.triangles.len(),
                "self_intersecting_faces": hits_a.iter().filter(|&&h| h).count(),
                "ops": ops,
            }));
        }
    }
    let mut all = Vec::new();
    for (name, k) in &kernels {
        let fail = failures.remove(name).unwrap_or_default();
        record(
            name,
            k.id(),
            "corpus",
            json!({
                "passes": fail.is_empty(),
                "partial": only.is_some(),
                "computed": computed.get(name).copied().unwrap_or(0),
                "refused": refused.remove(name).unwrap_or_default(),
                "failures": fail,
                "hang_limit_s": hang_limit().as_secs(),
                "heap_limit_bytes": heap_limit(),
                "models": models.remove(name).unwrap_or_default(),
            }),
        );
        all.extend(fail);
    }
    assert!(all.is_empty(), "corpus gate failed:\n{}", all.join("\n"));
    assert!(
        only.is_none(),
        "MF_QUALIFY_ONLY ran a subset: not a qualification"
    );
}

// ---------------------------------------------------------------------------
// Gates 4 and 5 through D5 windows (heavy)
// ---------------------------------------------------------------------------

/// A prepared window case: the window operand cut from the target, the
/// tool, and what the end-to-end time adds to the kernel's.
struct Window {
    name: &'static str,
    target_triangles: usize,
    window: KernelMesh,
    tool: KernelMesh,
    op: BoolOp,
    limit_s: f64,
    gated: bool,
    /// How the window was closed (see `prepare_windows`).
    closure: &'static str,
    /// Seconds for the linear pass over the whole target that finds the
    /// faces in the window box (the first step of D5's clip).
    scan_s: f64,
    prep: Value,
}

/// Faces of `m` whose bounds meet the box: D5's window scan, timed.
fn scan(m: &KernelMesh, lo: [f64; 3], hi: [f64; 3]) -> (usize, f64) {
    let t = Instant::now();
    let n = m
        .triangles
        .par_iter()
        .filter(|t| {
            let c = t.map(|v| m.positions[v as usize]);
            (0..3).all(|k| {
                c.iter().map(|p| p[k]).fold(f64::INFINITY, f64::min) <= hi[k]
                    && c.iter().map(|p| p[k]).fold(f64::NEG_INFINITY, f64::max) >= lo[k]
            })
        })
        .count();
    (n, t.elapsed().as_secs_f64())
}

fn grow(lo: [f64; 3], hi: [f64; 3], margin: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    (
        [0, 1, 2].map(|k| lo[k] - margin[k]),
        [0, 1, 2].map(|k| hi[k] + margin[k]),
    )
}

/// A closed target's window: `target ∩ box`, computed by `prep` (not
/// timed). The faces it keeps are the target's, clipped at the box; the
/// rest are the box's faces inside the target (D5's caps).
fn window_by_intersection(
    prep: &dyn Kernel,
    target: &KernelMesh,
    lo: [f64; 3],
    hi: [f64; 3],
) -> Result<(KernelMesh, Value), String> {
    let cut = boxed(lo, hi, 1);
    let t = Instant::now();
    let mut r = prep
        .boolean(target, &cut, BoolOp::Intersect, &RunContext::none())
        .map_err(|e| format!("window ∩ box: {e}"))?;
    let c = check(&mut r, target, &cut, &RunContext::none());
    if let Err(e) = &c.verdict {
        return Err(format!("window ∩ box is invalid: {e}"));
    }
    // Gate 3 checks the timed kernel against its own input, the window:
    // each window triangle is its own face. (D5 keeps the window → target
    // map apart, to drop the caps and weld; not built here.)
    let caps = r.origin.iter().filter(|o| o.0 == 1).count();
    let n = r.triangles.len();
    Ok((
        KernelMesh::labelled(r.positions, r.triangles, 0),
        json!({"kernel": prep.id(), "prep_s": t.elapsed().as_secs_f64(), "checked": c.json(),
               "window_faces_from_target": n - caps, "cap_faces": caps}),
    ))
}

/// An open target's window (D15's local closure, stood in for): the
/// connected patch of faces wholly inside the box around `seed`, closed into
/// a prism by a copy translated `depth` against its mean normal and side
/// walls along its boundary. Valid exactly when the patch is a disk and a
/// height field along that normal; checked with `validate_solid`.
fn window_by_prism(
    target: &KernelMesh,
    lo: [f64; 3],
    hi: [f64; 3],
    seed: [f64; 3],
    depth: f64,
) -> Result<(KernelMesh, Value), String> {
    let t = Instant::now();
    let inside = |v: u32| {
        let p = target.positions[v as usize];
        (0..3).all(|k| lo[k] <= p[k] && p[k] <= hi[k])
    };
    let faces: Vec<usize> = (0..target.triangles.len())
        .filter(|&f| target.triangles[f].iter().all(|&v| inside(v)))
        .collect();
    // Connected (by shared edges) component nearest the seed.
    let mut by_edge: BTreeMap<(u32, u32), Vec<usize>> = BTreeMap::new();
    for &f in &faces {
        let t = target.triangles[f];
        for i in 0..3 {
            let (u, v) = (t[i], t[(i + 1) % 3]);
            by_edge.entry((u.min(v), u.max(v))).or_default().push(f);
        }
    }
    let start = *faces
        .iter()
        .min_by(|&&f, &&g| {
            let d = |f: usize| norm(sub(target.positions[target.triangles[f][0] as usize], seed));
            d(f).total_cmp(&d(g))
        })
        .ok_or("no faces in the window")?;
    let mut keep = std::collections::BTreeSet::from([start]);
    let mut stack = vec![start];
    while let Some(f) = stack.pop() {
        let t = target.triangles[f];
        for i in 0..3 {
            let (u, v) = (t[i], t[(i + 1) % 3]);
            for &g in &by_edge[&(u.min(v), u.max(v))] {
                if keep.insert(g) {
                    stack.push(g);
                }
            }
        }
    }
    let mut normal = [0.0; 3];
    for &f in &keep {
        let c = target.triangles[f].map(|v| target.positions[v as usize]);
        let n = cross(sub(c[1], c[0]), sub(c[2], c[0]));
        for k in 0..3 {
            normal[k] += n[k];
        }
    }
    let len = norm(normal);
    let d = normal.map(|x| x / len);
    let mut index: BTreeMap<u32, u32> = BTreeMap::new();
    let mut positions = Vec::new();
    for &f in &keep {
        for v in target.triangles[f] {
            index.entry(v).or_insert_with(|| {
                positions.push(target.positions[v as usize]);
                (positions.len() - 1) as u32
            });
        }
    }
    let n = positions.len() as u32;
    let bottom: Vec<[f64; 3]> = positions
        .iter()
        .map(|p| [0, 1, 2].map(|k| p[k] - depth * d[k]))
        .collect();
    positions.extend(bottom);
    let mut triangles = Vec::new();
    let mut face_ids = Vec::new();
    let mut directed = std::collections::BTreeSet::new();
    let keep_order: Vec<usize> = keep.iter().copied().collect();
    for &f in &keep {
        let t = target.triangles[f].map(|v| index[&v]);
        triangles.push(t);
        face_ids.push((0, triangles.len() as u32 - 1));
        for i in 0..3 {
            directed.insert((t[i], t[(i + 1) % 3]));
        }
    }
    let push_cap = |t: [u32; 3], triangles: &mut Vec<[u32; 3]>, ids: &mut Vec<(u8, u32)>| {
        triangles.push(t);
        ids.push((0, triangles.len() as u32 - 1));
    };
    for &f in &keep {
        let [a, b, c] = target.triangles[f].map(|v| index[&v]);
        push_cap([a + n, c + n, b + n], &mut triangles, &mut face_ids);
    }
    for &(u, v) in &directed {
        if !directed.contains(&(v, u)) {
            push_cap([v, u, u + n], &mut triangles, &mut face_ids);
            push_cap([v, u + n, v + n], &mut triangles, &mut face_ids);
        }
    }
    // Valid as D4 would judge it: the scan's own self-intersections inside
    // the patch (and their copies in the translated patch) are the input's;
    // anything else (a fold, a pinch, a wall through the patch) refuses the
    // window.
    let checked = KernelResult {
        positions: positions.clone(),
        triangles: triangles.clone(),
        origin: (0..triangles.len())
            .map(|i| match i {
                // The patch, and its translated copy: the scan's own
                // self-intersections recur in both.
                i if i < keep.len() => target.face_ids[keep_order[i]],
                i if i < 2 * keep.len() => target.face_ids[keep_order[i - keep.len()]],
                i => (2, i as u32),
            })
            .collect(),
    };
    validate_boolean(
        &checked,
        [target, &KernelMesh::default()],
        &RunContext::none(),
    )
    .map_err(|e| format!("the prism window is not a valid solid: {e}"))?;
    Ok((
        KernelMesh {
            positions,
            triangles,
            face_ids,
        },
        json!({"prep_s": t.elapsed().as_secs_f64(), "patch_faces": keep.len(), "depth": depth}),
    ))
}

/// The vertex of `m` with the largest coordinate along `axis`.
fn extreme_vertex(m: &KernelMesh, axis: usize) -> [f64; 3] {
    *m.positions
        .iter()
        .max_by(|p, q| p[axis].total_cmp(&q[axis]))
        .unwrap()
}

/// The window cases of gate 5, prepared once for every kernel. Window
/// operands are prepared by the C++ kernel (unchecked) when it is compiled
/// in (fast), else by `manifold-rust`; either way the window is validated.
fn prepare_windows() -> Vec<Result<Window, (String, String)>> {
    // Unchecked: whole scans intersect themselves somewhere; the window
    // it cuts is validated (D4) before use.
    #[cfg(feature = "manifold-cpp")]
    let prep: Arc<dyn Kernel> = Arc::new(mf_boolean::ManifoldCpp::unchecked());
    #[cfg(not(feature = "manifold-cpp"))]
    let prep: Arc<dyn Kernel> = Arc::new(ManifoldRobust);
    let mut out = Vec::new();

    // A 50k-triangle sphere on a model's surface: centred on its extreme
    // vertex along +x, radius 5% of the model's diagonal; the window box is
    // the sphere's box grown by 10% of the radius.
    let part_on = |target: &KernelMesh| {
        let (lo, hi) = bbox(target);
        let r = 0.05 * norm(sub(hi, lo));
        let c = extreme_vertex(target, 0);
        let tool = sphere_n(50_000, r, c, 1);
        let (tlo, thi) = bbox(&tool);
        let (wlo, whi) = grow(tlo, thi, [0.1 * r; 3]);
        (tool, wlo, whi, c, r)
    };

    // Winged victory (5.7M, closed): window by intersection.
    let wv = import(WINGED_VICTORY, 0);
    {
        let (tool, wlo, whi, _, _) = part_on(&wv);
        let (_, scan_s) = scan(&wv, wlo, whi);
        out.push(
            window_by_intersection(prep.as_ref(), &wv, wlo, whi)
                .map(|(window, prep)| Window {
                    name: "sphere50k_union_winged_victory",
                    target_triangles: wv.triangles.len(),
                    window,
                    tool,
                    op: BoolOp::Union,
                    limit_s: 3.0,
                    gated: true,
                    closure: "target ∩ window box (D5 caps from the box faces)",
                    scan_s,
                    prep,
                })
                .map_err(|e| ("sphere50k_union_winged_victory".into(), e)),
        );
    }
    // A slab cut through winged victory: informational (the gated slab
    // case is the 1M model below).
    {
        let (lo, hi) = bbox(&wv);
        let size = sub(hi, lo);
        let mid = 0.5 * (lo[2] + hi[2]);
        let t = 0.005 * norm(size);
        let slo = [lo[0] - 0.1 * size[0], lo[1] - 0.1 * size[1], mid - t / 2.0];
        let shi = [hi[0] + 0.1 * size[0], hi[1] + 0.1 * size[1], mid + t / 2.0];
        let tool = boxed(slo, shi, 1);
        let (wlo, whi) = grow(slo, shi, [0.0, 0.0, t]);
        let (_, scan_s) = scan(&wv, wlo, whi);
        out.push(
            window_by_intersection(prep.as_ref(), &wv, wlo, whi)
                .map(|(window, prep)| Window {
                    name: "slab_cut_winged_victory",
                    target_triangles: wv.triangles.len(),
                    window,
                    tool,
                    op: BoolOp::Subtract,
                    limit_s: 10.0,
                    gated: false,
                    closure: "target ∩ window box (D5 caps from the box faces)",
                    scan_s,
                    prep,
                })
                .map_err(|e| ("slab_cut_winged_victory".into(), e)),
        );
    }
    drop(wv);

    // Archaeopteryx (1M, open, holes elsewhere): window by prism. The part
    // goes on the first extreme vertex (+x, −x, +y, −y, +z, −z) whose patch
    // closes into a valid prism; the attempts are recorded.
    let ax = import(ARCHAEOPTERYX, 0);
    {
        let (lo, hi) = bbox(&ax);
        let mut attempts = Vec::new();
        let mut found = None;
        // Seeds: the top and bottom of the model's middle along its thinnest
        // axis (the fossil's slab faces), then its extreme vertices.
        let size = sub(hi, lo);
        let thin = (0..3).min_by(|&a, &b| size[a].total_cmp(&size[b])).unwrap();
        let mid = [0, 1, 2].map(|k| 0.5 * (lo[k] + hi[k]));
        let central =
            |p: &[f64; 3]| (0..3).all(|k| k == thin || (p[k] - mid[k]).abs() <= 0.1 * size[k]);
        let mut seeds = Vec::new();
        // The detailed (+) side first: the top vertex near points of a grid
        // over the slab, then the middles, then the extremes.
        let others: Vec<usize> = (0..3).filter(|&k| k != thin).collect();
        for (u, v) in [
            (0.0, 0.0),
            (0.25, 0.0),
            (-0.25, 0.0),
            (0.0, 0.25),
            (0.0, -0.25),
            (0.25, 0.25),
            (-0.25, -0.25),
            (0.25, -0.25),
            (-0.25, 0.25),
        ] {
            let target = [u, v];
            let near = |p: &[f64; 3]| {
                (0..2).all(|i| {
                    let k = others[i];
                    (p[k] - (mid[k] + target[i] * size[k])).abs() <= 0.02 * size[k]
                })
            };
            if let Some(c) = ax
                .positions
                .iter()
                .filter(|p| near(p))
                .max_by(|p, q| p[thin].total_cmp(&q[thin]))
            {
                seeds.push((format!("grid ({u}, {v}), +{thin}"), *c));
            }
        }
        for sign in [1.0, -1.0] {
            if let Some(c) = ax
                .positions
                .iter()
                .filter(|p| central(p))
                .max_by(|p, q| (sign * p[thin]).total_cmp(&(sign * q[thin])))
            {
                seeds.push((
                    format!("middle, {}{}", if sign > 0.0 { "+" } else { "-" }, thin),
                    *c,
                ));
            }
        }
        for (axis, sign) in [
            (0, 1.0),
            (0, -1.0),
            (1, 1.0),
            (1, -1.0),
            (2, 1.0),
            (2, -1.0),
        ] {
            let c = *ax
                .positions
                .iter()
                .max_by(|p, q| (sign * p[axis]).total_cmp(&(sign * q[axis])))
                .unwrap();
            seeds.push((
                format!("extreme, {}{}", if sign > 0.0 { "+" } else { "-" }, axis),
                c,
            ));
        }
        'seeds: for (seed, c) in seeds {
            for frac in [0.05, 0.03, 0.02] {
                let r = frac * norm(sub(hi, lo));
                let tool = sphere_n(50_000, r, c, 1);
                let (tlo, thi) = bbox(&tool);
                let (wlo, whi) = grow(tlo, thi, [0.1 * r; 3]);
                match window_by_prism(&ax, wlo, whi, c, 3.0 * r) {
                    Ok((window, mut prep)) => {
                        let (_, scan_s) = scan(&ax, wlo, whi);
                        prep["seed"] =
                            json!({"seed": seed, "vertex": c, "radius_of_diagonal": frac});
                        prep["rejected_seeds"] = json!(attempts.clone());
                        found = Some((window, tool, scan_s, prep));
                        break 'seeds;
                    }
                    Err(e) => {
                        attempts.push(json!({"seed": seed, "radius_of_diagonal": frac, "error": e}))
                    }
                }
            }
        }
        out.push(match found {
            Some((window, tool, scan_s, prep)) => Ok(Window {
                name: "sphere50k_union_archaeopteryx",
                target_triangles: ax.triangles.len(),
                window,
                tool,
                op: BoolOp::Union,
                limit_s: 3.0,
                gated: true,
                closure: "patch in the window box closed as a prism (stands in for D15's local closure of an open scan)",
                scan_s,
                prep,
            }),
            None => Err((
                "sphere50k_union_archaeopteryx".into(),
                format!("no seed gave a valid prism window: {}", json!(attempts)),
            )),
        });
    }
    drop(ax);

    // A plane slab cut through a 1M model (a 1M UV sphere: no real closed
    // model of that size is in the fixtures): thickness 0.5% of the
    // diagonal through the equator, spanning the model; window = the slab
    // grown by its thickness across the cut.
    let m1 = sphere_of(1_000_000, [0.0; 3], 0);
    {
        let t = 0.005 * 2.0 * 3f64.sqrt();
        let slo = [-1.2, -1.2, -t / 2.0];
        let shi = [1.2, 1.2, t / 2.0];
        let tool = boxed(slo, shi, 1);
        let (wlo, whi) = grow(slo, shi, [0.0, 0.0, t]);
        let (_, scan_s) = scan(&m1, wlo, whi);
        out.push(
            window_by_intersection(prep.as_ref(), &m1, wlo, whi)
                .map(|(window, prep)| Window {
                    name: "slab_cut_sphere1m",
                    target_triangles: m1.triangles.len(),
                    window,
                    tool,
                    op: BoolOp::Subtract,
                    limit_s: 10.0,
                    gated: true,
                    closure: "target ∩ window box (D5 caps from the box faces)",
                    scan_s,
                    prep,
                })
                .map_err(|e| ("slab_cut_sphere1m".into(), e)),
        );
    }
    drop(m1);

    // 50k × 50k overlapping solids: the operands are the window.
    out.push(Ok(Window {
        name: "spheres_50k_x_50k",
        target_triangles: 50_000,
        window: sphere_of(50_000, [0.0; 3], 0),
        tool: sphere_of(50_000, [0.5, 0.3, 0.2], 1),
        op: BoolOp::Union,
        limit_s: 0.5,
        gated: true,
        closure: "whole operands",
        scan_s: 0.0,
        prep: json!(null),
    }));
    out
}

/// Cancel-to-stop on one case at several delays.
fn cancel_points(k: &Arc<dyn Kernel>, a: &KernelMesh, b: &KernelMesh, op: BoolOp) -> Value {
    let (a, b) = (Arc::new(a.clone()), Arc::new(b.clone()));
    let (pool, _) = JobPool::new(None);
    let mut points = Vec::new();
    let mut worst = Duration::ZERO;
    let mut cancelled_runs = 0;
    let mut wrong = Vec::new();
    for delay_ms in [2u64, 10, 25, 50, 100, 200, 400, 800, 1500, 2500] {
        let ctx = RunContext::none();
        let (tx, rx) = channel();
        let (a2, b2, ctx2, k2) = (Arc::clone(&a), Arc::clone(&b), ctx.clone(), Arc::clone(k));
        let finished = Arc::new(AtomicBool::new(false));
        let fin = Arc::clone(&finished);
        let start = Instant::now();
        pool.spawn(move || {
            let r = catch_unwind(AssertUnwindSafe(|| k2.boolean(&a2, &b2, op, &ctx2)));
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
            if !matches!(result, Ok(Err(KernelError::Cancelled))) {
                wrong.push(format!("cancel at {delay_ms} ms: {outcome}"));
            }
        }
        points.push(json!({
            "cancel_after_ms": delay_ms,
            "running_at_cancel": was_running,
            "stage_at_cancel": stage.0,
            "fraction_at_cancel": stage.1,
            "cancel_to_stop_ms": latency.as_secs_f64() * 1e3,
            "run_ms": ended.saturating_duration_since(start).as_secs_f64() * 1e3,
            "outcome": outcome,
        }));
        if !was_running {
            break;
        }
    }
    json!({
        "passes": worst <= Duration::from_millis(250) && cancelled_runs >= 3 && wrong.is_empty(),
        "limit_ms": 250,
        "worst_cancel_to_stop_ms": worst.as_secs_f64() * 1e3,
        "cancelled_runs": cancelled_runs,
        "wrong_outcomes": wrong,
        "load_average": load_average(),
        "points": points,
    })
}

#[test]
fn qualify_windows() {
    require_fixtures(&[WINGED_VICTORY, ARCHAEOPTERYX]);
    let windows = prepare_windows();
    let kernels = kernels();
    let mut all = Vec::new();
    for (name, k) in &kernels {
        let mut failures = Vec::new();
        let mut cases = BTreeMap::new();
        let mut largest: Option<&Window> = None;
        for w in &windows {
            let w = match w {
                Ok(w) => w,
                Err((case, e)) => {
                    failures.push(format!("{case}: window preparation failed: {e}"));
                    cases.insert(case.clone(), json!({"prep_error": e}));
                    continue;
                }
            };
            if w.gated
                && largest.is_none_or(|l| w.window.triangles.len() > l.window.triangles.len())
            {
                largest = Some(w);
            }
            let hits = [self_hits(&w.window), self_hits(&w.tool)];
            let mut runs = Vec::new();
            let mut hashes = Vec::new();
            let mut verdict = Value::Null;
            let mut prov = Value::Null;
            let mut kernel_walls = Vec::new();
            let mut ends = Vec::new();
            for run in 0..3 {
                let load = load_average();
                let (r, wall, peak, stop) = guarded(&format!("{name} {}", w.name), |ctx| {
                    k.boolean(&w.window, &w.tool, w.op, ctx)
                });
                let mut r = match (r, stop) {
                    (Ok(Ok(r)), _) => r,
                    (r, s) => {
                        failures.push(format!("{}: {:?} ({s:?})", w.name, r.map(|r| r.err())));
                        break;
                    }
                };
                let c = check(&mut r, &w.window, &w.tool, &RunContext::none());
                let end = w.scan_s + wall + c.cleanup_s + c.validate_s;
                kernel_walls.push(wall);
                ends.push(end);
                hashes.push(hash(&r));
                if run == 0 {
                    verdict = c.json();
                    if let Err(e) = &c.verdict {
                        failures.push(format!("{}: invalid output: {e}", w.name));
                    }
                    let p = provenance(&w.window, &w.tool, &r, [&hits[0], &hits[1]]);
                    if !p.passes() {
                        failures.push(format!("{}: provenance {p:?}", w.name));
                    }
                    prov = p.json();
                }
                runs.push(json!({
                    "kernel_s": wall,
                    "peak_heap_bytes": peak,
                    "cleanup_s": c.cleanup_s,
                    "validate_s": c.validate_s,
                    "end_to_end_s": end,
                    "result_triangles": r.triangles.len(),
                    "load_average": load,
                }));
            }
            let median = |v: &[f64]| {
                let mut s = v.to_vec();
                s.sort_by(f64::total_cmp);
                s.get(s.len() / 2).copied().unwrap_or(f64::NAN)
            };
            // 50k × 50k is gated on the kernel's time (as in Task 7); the
            // window cases on scan + kernel + post-pass + validation.
            let gated_s = if w.closure == "whole operands" {
                median(&kernel_walls)
            } else {
                median(&ends)
            };
            let one_thread = on_pool(Some(1), || {
                k.boolean(&w.window, &w.tool, w.op, &RunContext::none())
                    .map(|mut r| {
                        check(&mut r, &w.window, &w.tool, &RunContext::none());
                        hash(&r)
                    })
            });
            let same_1t = !hashes.is_empty() && one_thread.as_ref().ok() == hashes.first();
            let same_runs = hashes.iter().all(|h| Some(h) == hashes.first());
            if w.gated {
                if gated_s.is_nan() || gated_s > w.limit_s {
                    failures.push(format!("{}: {gated_s:.3} s > {} s", w.name, w.limit_s));
                }
                if !same_1t || !same_runs {
                    failures.push(format!(
                        "{}: not deterministic (runs same {same_runs}, 1 thread same {same_1t})",
                        w.name
                    ));
                }
            }
            let entry = json!({
                "gated": w.gated,
                "limit_s": w.limit_s,
                "median_gated_s": gated_s,
                "gated_on": if w.closure == "whole operands" { "kernel" } else { "scan + kernel + post-pass + validation" },
                "passes": !w.gated || (gated_s <= w.limit_s && same_1t && same_runs),
                "target_triangles": w.target_triangles,
                "window_triangles": w.window.triangles.len(),
                "tool_triangles": w.tool.triangles.len(),
                "op": format!("{:?}", w.op),
                "closure": w.closure,
                "scan_s": w.scan_s,
                "window_prep": w.prep,
                "runs": runs,
                "checked": verdict,
                "provenance": prov,
                "same_hash_across_runs": same_runs,
                "same_hash_at_1_thread": same_1t,
                "hash": hashes.first(),
                "not_measured": "D5's clip, cap triangulation and weld (Task 8); the window operand is prepared untimed",
            });
            println!("{name} {}: {entry}", w.name);
            cases.insert(w.name.to_string(), entry);
        }
        let cancel = largest.map(|w| {
            let mut c = cancel_points(k, &w.window, &w.tool, w.op);
            c["case"] = json!(w.name);
            c["window_triangles"] = json!(w.window.triangles.len());
            c
        });
        if let Some(c) = &cancel {
            println!("{name} cancel: {c}");
            if c["passes"] != json!(true) {
                failures.push(format!("cancel: {}", c["worst_cancel_to_stop_ms"]));
            }
        }
        record(
            name,
            k.id(),
            "windows",
            json!({"passes": failures.is_empty(), "failures": failures, "cases": cases}),
        );
        record(name, k.id(), "cancel", cancel.unwrap_or(Value::Null));
        all.extend(failures.into_iter().map(|f| format!("{name}: {f}")));
    }
    assert!(all.is_empty(), "{}", all.join("\n"));
}

// ---------------------------------------------------------------------------
// Informational: the whole-model 1M × 1M (heavy; not a gate since Task 7b)
// ---------------------------------------------------------------------------

#[test]
fn qualify_1m_union() {
    let a = sphere_of(1_000_000, [0.0; 3], 0);
    let b = sphere_of(1_000_000, [0.5, 0.3, 0.2], 1);
    for (name, k) in kernels() {
        let load = load_average();
        let (r, phase) = measure("union", || {
            k.boolean(&a, &b, BoolOp::Union, &RunContext::none())
        });
        let entry = match r {
            Ok(mut r) => {
                let c = check(&mut r, &a, &b, &RunContext::none());
                json!({
                    "kernel_s": phase.wall_s,
                    "peak_heap_bytes": phase.peak_bytes,
                    "result_triangles": r.triangles.len(),
                    "checked": c.json(),
                    "load_average": load,
                    "gate": "none (informational since Task 7b: D5 never sends whole 1M operands)",
                })
            }
            Err(e) => json!({"error": e.to_string(), "load_average": load}),
        };
        println!("{name} 1M × 1M: {entry}");
        record(name, k.id(), "union_1m_informational", entry);
    }
}
