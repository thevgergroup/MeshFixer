#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Surgical repair on a real damaged scan (MF-OP-003, phase-0 benchmark):
//! the Smithsonian Archaeopteryx scan in `fixtures/SOURCES.md`, fetched by
//! `scripts/fetch-fixtures.sh`. Skips (per policy) when not fetched.

use std::collections::{BTreeMap, HashMap};

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

/// Faces on each undirected edge.
fn edge_faces(m: &Mesh) -> HashMap<(u32, u32), u32> {
    let mut count = HashMap::new();
    for (_, t) in m.faces() {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *count.entry((a.min(b), a.max(b))).or_insert(0) += 1;
        }
    }
    count
}

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
    for id in loops {
        // One command per hole, each on the imported mesh.
        let out = op.execute(&m, &Target::Loops(vec![id]), &params).unwrap();
        let (after, prov) = out.change.unwrap();
        // Untouched outside the target: every input face keeps its ID and
        // indices, every input vertex its exact position, nothing removed.
        assert!(prov.removed_faces.is_empty() && prov.removed_vertices.is_empty());
        assert!(prov.modified_faces.is_empty() && prov.moved_vertices.is_empty());
        for (f, t) in m.faces() {
            assert_eq!(after.face(f), Some(t), "loop {id}: face {f:?} changed");
        }
        for (v, p) in m.vertices() {
            let q = after.position(v).unwrap();
            assert_eq!(p.map(f64::to_bits), q.map(f64::to_bits), "loop {id}: {v:?}");
        }
        // The loop closes and the patch is manifold where it meets the scan:
        // no edge of a new face has more than two faces.
        let mut added: HashMap<(u32, u32), u32> = HashMap::new();
        for (f, _) in &prov.created_faces {
            let t = after.face(*f).unwrap();
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                *added.entry((a.min(b), a.max(b))).or_insert(0) += 1;
            }
        }
        for (e, n) in &added {
            let total = n + before_edges.get(e).copied().unwrap_or(0);
            assert_eq!(total, 2, "loop {id}: edge {e:?} has {total} faces");
        }
    }
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
        .execute(&m, &Target::WholeObject, &params())
        .unwrap();
    let (after, _) = out.change.unwrap();
    let d = inspect(&after);
    assert!(d.boundary_loops.is_empty(), "{}", out.report);
    assert_eq!(d.non_manifold_edges, 0);
    for (name, mesh) in [("before", &m), ("after", &after)] {
        let image = mf_render::offscreen::render_image(mesh, 1600, 1200, true).unwrap();
        if let Ok(dir) = std::env::var("MF_ARTIFACTS") {
            let path = std::path::Path::new(&dir).join(format!("real-scan-{name}.png"));
            image.save(&path).unwrap();
        }
    }
}
