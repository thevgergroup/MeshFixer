#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Surgical repair on a real damaged scan (MF-OP-003, phase-0 benchmark):
//! the Smithsonian Archaeopteryx scan in `fixtures/SOURCES.md`, fetched by
//! `scripts/fetch-fixtures.sh`. Skips (per policy) when not fetched.

mod common;

use std::collections::{BTreeMap, HashMap};

use common::{edge_faces, untouched_outside, vector_area};
use mf_geometry::{Mesh, inspect};
use mf_operations::{Target, find};
use mf_spec::{Params, builtin};

const SCAN: &str = "archaeopteryx.obj";

fn scan(test: &str) -> Option<Mesh> {
    let path = mf_fixtures::real::path(SCAN, test)?;
    Some(mf_io::read_path(&path).unwrap().mesh)
}

fn params() -> Params {
    builtin::get("mesh.fill_holes")
        .unwrap()
        .validate(&BTreeMap::new())
        .unwrap()
}

/// Slit threshold: a millionth of the smallest real hole's area here
/// (~1e-8 m²), far above rounding noise in a millimetre-quantised scan.
const SLIT_AREA: f64 = 1e-14;

#[test]
fn real_scan_has_holes_and_islands() {
    let Some(m) = scan("real_scan_has_holes_and_islands") else {
        return;
    };
    let d = inspect(&m);
    assert!(
        (500_000..=5_000_000).contains(&d.triangle_count),
        "{}",
        d.triangle_count
    );
    assert!(d.boundary_loops.len() >= 50, "{}", d.boundary_loops.len());
    assert!(d.components >= 2, "{}", d.components);
    assert_eq!(d.non_manifold_edges, 0);
}

#[test]
fn fill_each_small_hole_of_real_scan_untouched_outside() {
    let Some(m) = scan("fill_each_small_hole_of_real_scan_untouched_outside") else {
        return;
    };
    let op = find("mesh.fill_holes").unwrap();
    let params = params();
    let before_edges = edge_faces(&m);
    let loops: Vec<u32> = m
        .topology()
        .boundary_loops
        .iter()
        .filter(|l| l.edge_count <= 200)
        .map(|l| l.id)
        .collect();
    assert!(loops.len() >= 50, "{}", loops.len());
    let topo = m.topology();
    let diag = m.bounding_box().unwrap().diagonal();
    let min_area = 1e-12 * diag * diag;
    let mut slits = 0;
    for id in loops {
        // One command per hole, each on the imported mesh. A slit (collinear
        // crack) is refused by name rather than "filled" with zero-area
        // triangles; everything else closes.
        let out = match op.execute(
            &m,
            &Target::Loops(vec![id]),
            &params,
            &mf_operations::RunContext::none(),
        ) {
            Ok(out) => out,
            Err(mf_operations::OpError::Failed { code, message }) => {
                assert_eq!(code, "loop_untriangulable", "loop {id}: {message}");
                let l = topo.boundary_loops.iter().find(|l| l.id == id).unwrap();
                let area = vector_area(&m, &l.vertices);
                assert!(area < SLIT_AREA, "loop {id} refused but has area {area}");
                slits += 1;
                continue;
            }
            Err(e) => panic!("loop {id}: {e}"),
        };
        let (after, prov) = out.change.unwrap();
        // Untouched outside the target: every input face keeps its ID and
        // indices, every input vertex its exact position, nothing removed;
        // the loop closes and the patch is manifold where it meets the scan.
        if let Err(e) = untouched_outside(&m, &before_edges, &after, &prov) {
            panic!("loop {id}: {e}");
        }
        // No new face is degenerate (by inspect's threshold).
        for (f, _) in &prov.created_faces {
            let t = after.face(*f).unwrap();
            let area = vector_area(&after, &t);
            assert!(area >= min_area, "loop {id}: face {f:?} has area {area}");
        }
    }
    assert!(slits <= 10, "{slits} slits");
}

fn unit_normal(m: &Mesh, t: [u32; 3]) -> [f64; 3] {
    let [a, b, c] = t.map(|v| m.corner_position(v));
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    n.map(|x| x / len)
}

/// Angles (degrees) between the normals of a patch face and each face
/// across one of its edges: (the largest anywhere, the mean over the rim
/// edges, where the patch meets the scan).
fn patch_dihedrals(
    before_faces: &HashMap<(u32, u32), Vec<[u32; 3]>>,
    after: &Mesh,
    prov: &mf_geometry::Provenance,
) -> (f64, f64) {
    let mut by_edge: BTreeMap<(u32, u32), Vec<[u32; 3]>> = BTreeMap::new();
    for (f, _) in &prov.created_faces {
        let t = after.face(*f).unwrap();
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            by_edge.entry((a.min(b), a.max(b))).or_default().push(t);
        }
    }
    let angle = |t: [u32; 3], o: [u32; 3]| {
        let (n, m) = (unit_normal(after, t), unit_normal(after, o));
        (n[0] * m[0] + n[1] * m[1] + n[2] * m[2])
            .clamp(-1.0, 1.0)
            .acos()
            .to_degrees()
    };
    let (mut worst, mut rim_sum, mut rim_count) = (0.0f64, 0.0, 0);
    for (e, faces) in &by_edge {
        for (i, &t) in faces.iter().enumerate() {
            for &o in &faces[i + 1..] {
                worst = worst.max(angle(t, o));
            }
            for &o in before_faces.get(e).into_iter().flatten() {
                let a = angle(t, o);
                worst = worst.max(a);
                rim_sum += a;
                rim_count += 1;
            }
        }
    }
    (worst, rim_sum / rim_count.max(1) as f64)
}

/// MF-OP-002 fill styles on the real scan: every hole fills in both styles,
/// untouched outside, the solve converges on each, and `smooth` meets the
/// scan more gently than `flat` (the angle between a patch face and its
/// neighbours: see the gate below).
#[test]
fn real_scan_holes_fill_smoothly() {
    let Some(m) = scan("real_scan_holes_fill_smoothly") else {
        return;
    };
    let op = find("mesh.fill_holes").unwrap();
    let style = |s: &str| {
        builtin::get("mesh.fill_holes")
            .unwrap()
            .validate(&BTreeMap::from([("fill_style".into(), s.into())]))
            .unwrap()
    };
    let (flat, smooth) = (style("flat"), style("smooth"));
    let before_edges = edge_faces(&m);
    let mut before_faces: HashMap<(u32, u32), Vec<[u32; 3]>> = HashMap::new();
    for (_, t) in m.faces() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            before_faces
                .entry((a.min(b), a.max(b)))
                .or_default()
                .push(t);
        }
    }
    let loops: Vec<(u32, usize)> = m
        .topology()
        .boundary_loops
        .iter()
        // Every hole the default limit fills.
        .filter(|l| l.edge_count <= 500)
        .map(|l| (l.id, l.edge_count))
        .collect();
    // (loop, edges, new vertices, faired vertices, time, flat and smooth
    // dihedrals: largest, mean at the rim).
    type Row = (
        u32,
        usize,
        usize,
        u64,
        std::time::Duration,
        (f64, f64),
        (f64, f64),
    );
    let mut rows: Vec<Row> = Vec::new();
    for (id, edges) in loops {
        let run = |params: &Params| {
            let start = std::time::Instant::now();
            let out = op.execute(
                &m,
                &Target::Loops(vec![id]),
                params,
                &mf_operations::RunContext::none(),
            );
            (out, start.elapsed())
        };
        let (flat_out, _) = run(&flat);
        let flat_out = match flat_out {
            Ok(out) => out,
            // A slit: refused by name in every style (see above).
            Err(mf_operations::OpError::Failed {
                code: "loop_untriangulable",
                ..
            }) => {
                continue;
            }
            Err(e) => panic!("loop {id} (flat): {e}"),
        };
        let (smooth_out, took) = run(&smooth);
        let smooth_out = smooth_out.unwrap_or_else(|e| panic!("loop {id} (smooth): {e}"));
        let faired = smooth_out.report["filled_loops"][0]["faired_vertices"]
            .as_u64()
            .unwrap_or(0);
        let (fa, fp) = flat_out.change.unwrap();
        let (sa, sp) = smooth_out.change.unwrap();
        if let Err(e) = untouched_outside(&m, &before_edges, &sa, &sp) {
            panic!("loop {id} (smooth): {e}");
        }
        let f = patch_dihedrals(&before_faces, &fa, &fp);
        let s = patch_dihedrals(&before_faces, &sa, &sp);
        rows.push((id, edges, sp.created_vertices.len(), faired, took, f, s));
    }
    assert!(rows.len() >= 50, "{}", rows.len());
    for (id, edges, new, faired, took, f, s) in &rows {
        eprintln!(
            "loop {id}: {edges} edges, {new} new vertices, {faired} faired, {took:?}; \
             max dihedral flat {:.2}°, smooth {:.2}°; mean at the rim flat {:.2}°, smooth {:.2}°",
            f.0, s.0, f.1, s.1
        );
    }
    // The scan's holes are small (3–12 edges): refinement adds new
    // vertices to only a few, and only those can be faired; the rest are
    // the same patch in every style. Where fairing works on a noisy rim it
    // lowers the sharpest angle overall, but one rim edge can end up a
    // little sharper (by under 3°: loops 34, 70 and 73 here). The gate,
    // owner-approved 2026-11-09 (MF-ISSUE-054): no hole gets 3° or more
    // sharper, and the scan as a whole gets smoother.
    let faired: Vec<_> = rows.iter().filter(|r| r.2 > 0).collect();
    let worse: Vec<_> = faired
        .iter()
        .filter(|r| r.6.0 > r.5.0 + 1e-9)
        .map(|r| (r.0, r.5.0, r.6.0))
        .collect();
    eprintln!(
        "{} holes, {} faired; sharper with smooth at (loop, flat°, smooth°): {worse:?}",
        rows.len(),
        faired.len()
    );
    for (id, flat, smooth) in &worse {
        assert!(
            smooth - flat < 3.0,
            "loop {id}: smooth {smooth:.2}° vs flat {flat:.2}°"
        );
    }
    let sum = |f: &dyn Fn(&Row) -> f64| -> f64 { faired.iter().map(|r| f(r)).sum() };
    let (flat_max, smooth_max) = (sum(&|r| r.5.0), sum(&|r| r.6.0));
    let (flat_rim, smooth_rim) = (sum(&|r| r.5.1), sum(&|r| r.6.1));
    eprintln!(
        "sum over faired holes: max dihedral flat {flat_max:.1}°, smooth {smooth_max:.1}°; \
         mean rim angle flat {flat_rim:.1}°, smooth {smooth_rim:.1}°"
    );
    assert!(smooth_max < flat_max, "{smooth_max} vs {flat_max}");
    assert!(smooth_rim < flat_rim, "{smooth_rim} vs {flat_rim}");
}

/// MF-ISSUE-040: filling every hole of the real scan in one whole-object
/// run keeps the scan consistently oriented: no edge is walked the same
/// way by two faces, before or after (every patch is wound to match the
/// faces it meets).
#[test]
fn real_scan_fill_keeps_orientation_consistent() {
    let Some(m) = scan("real_scan_fill_keeps_orientation_consistent") else {
        return;
    };
    let before = inspect(&m);
    assert_eq!(before.inconsistent_orientation_edges, 0, "{before:?}");
    let out = find("mesh.fill_holes")
        .unwrap()
        .execute(
            &m,
            &Target::WholeObject,
            &params(),
            &mf_operations::RunContext::none(),
        )
        .unwrap();
    assert!(
        !out.report["filled_loops"].as_array().unwrap().is_empty(),
        "{}",
        out.report
    );
    let (after, _) = out.change.unwrap();
    let d = inspect(&after);
    assert_eq!(d.inconsistent_orientation_edges, 0, "{d:?}");
    assert_eq!(d.non_manifold_edges, 0);
    assert_ne!(
        d.volume_status,
        mf_geometry::VolumeStatus::InconsistentOrientation,
        "{d:?}"
    );
}

#[test]
fn real_scan_render_before_after() {
    let test = "real_scan_render_before_after";
    if !mf_render::offscreen::gpu_available() {
        mf_fixtures::skip::gpu_unavailable(test);
        return;
    }
    let Some(m) = scan(test) else {
        return;
    };
    let out = find("mesh.fill_holes")
        .unwrap()
        .execute(
            &m,
            &Target::WholeObject,
            &params(),
            &mf_operations::RunContext::none(),
        )
        .unwrap();
    let (after, _) = out.change.unwrap();
    let d = inspect(&after);
    // Every hole closes except the slits, which are reported by name.
    let skipped = out.report["skipped_loops"].as_array().unwrap();
    assert!(
        skipped.iter().all(|s| s["reason"] == "untriangulable"),
        "{}",
        out.report
    );
    assert_eq!(d.boundary_loops.len(), skipped.len(), "{}", out.report);
    assert_eq!(d.non_manifold_edges, 0);
    assert_eq!(d.degenerate_triangles, inspect(&m).degenerate_triangles);
    for (name, mesh) in [("before", &m), ("after", &after)] {
        let image = mf_render::offscreen::render_image(mesh, 1600, 1200, true).unwrap();
        if let Ok(dir) = std::env::var("MF_ARTIFACTS") {
            let path = std::path::Path::new(&dir).join(format!("real-scan-{name}.png"));
            image.save(&path).unwrap();
        }
    }
}
