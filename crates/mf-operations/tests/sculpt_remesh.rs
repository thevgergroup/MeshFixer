#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Local dynamic remeshing (Phase 1 Task 18, MF-SCULPT-002).

use std::collections::{BTreeMap, HashSet};

use mf_fixtures::uv_sphere;
use mf_geometry::{FaceId, Mesh, Provenance};
use mf_operations::RunContext;
use mf_operations::sculpt::{
    BrushKind, BrushParams, DabSample, Falloff, SculptIndex, StrokeState, remesh_band,
};

/// Faces of `mesh` with every corner within `r` of `c`.
fn band(mesh: &Mesh, c: [f64; 3], r: f64) -> Vec<FaceId> {
    mesh.faces()
        .filter(|(_, t)| {
            t.iter().all(|&v| {
                let p = mesh.corner_position(v);
                (0..3).map(|k| (p[k] - c[k]).powi(2)).sum::<f64>() < r * r
            })
        })
        .map(|(f, _)| f)
        .collect()
}

/// Edges of `faces` in `mesh` whose two faces are both in `faces`, away
/// from the region's fixed rim (neither end on a face outside it): the
/// rim's edges can't change, so the faces next to them are a transition.
fn interior_edges(mesh: &Mesh, faces: &HashSet<FaceId>) -> Vec<(u32, u32)> {
    let rim: HashSet<u32> = mesh
        .faces()
        .filter(|(f, _)| !faces.contains(f))
        .flat_map(|(_, t)| t)
        .collect();
    let mut count: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    for &f in faces {
        let t = mesh.face(f).unwrap();
        for i in 0..3 {
            let (a, b) = (t[i], t[(i + 1) % 3]);
            *count.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    count
        .into_iter()
        .filter(|&((a, b), n)| n == 2 && !rim.contains(&a) && !rim.contains(&b))
        .map(|(e, _)| e)
        .collect()
}

fn edge_len(mesh: &Mesh, (a, b): (u32, u32)) -> f64 {
    let (p, q) = (mesh.corner_position(a), mesh.corner_position(b));
    (0..3).map(|k| (p[k] - q[k]).powi(2)).sum::<f64>().sqrt()
}

/// The region after a remesh: its faces that are still there, plus the
/// faces the remesh made or changed.
fn new_region(after: &Mesh, prov: &Provenance, before: &[FaceId]) -> HashSet<FaceId> {
    prov.created_faces
        .iter()
        .map(|(f, _)| *f)
        .chain(prov.modified_faces.iter().copied())
        .chain(before.iter().copied())
        .filter(|&f| after.face(f).is_some())
        .collect()
}

const C: [f64; 3] = [1.0, 0.0, 0.0];

#[test]
fn remesh_keeps_edges_within_target_band() {
    let sphere = uv_sphere(48, 96); // edges ≈ 0.065
    let faces = band(&sphere, C, 0.5);
    for target in [0.02, 0.15] {
        let (after, prov) = remesh_band(&sphere, &faces, target, &RunContext::none()).unwrap();
        let region = new_region(&after, &prov, &faces);
        let edges = interior_edges(&after, &region);
        assert!(edges.len() > 50, "target {target}: {} edges", edges.len());
        let lens: Vec<f64> = edges.iter().map(|&e| edge_len(&after, e)).collect();
        let longest = lens.iter().copied().fold(0.0, f64::max);
        let within = lens
            .iter()
            .filter(|&&l| l >= 0.5 * target && l <= 4.0 / 3.0 * target)
            .count();
        eprintln!(
            "target {target}: {} edges, longest {longest:.4}, {:.0}% within [0.5, 4/3]",
            lens.len(),
            100.0 * within as f64 / lens.len() as f64
        );
        // A few edges stay long where splitting would make a sliver.
        let long = lens.iter().filter(|&&l| l > 4.0 / 3.0 * target).count();
        assert!(
            long * 20 <= lens.len(),
            "{long} long edges of {}",
            lens.len()
        );
        assert!(within * 10 >= lens.len() * 8, "{within} of {}", lens.len());
    }
}

#[test]
fn remesh_touches_nothing_outside_the_footprint() {
    let sphere = uv_sphere(48, 96);
    let faces: HashSet<FaceId> = band(&sphere, C, 0.5).into_iter().collect();
    let list: Vec<FaceId> = faces.iter().copied().collect();
    for target in [0.02, 0.15] {
        let (after, _) = remesh_band(&sphere, &list, target, &RunContext::none()).unwrap();
        for (f, t) in sphere.faces() {
            if faces.contains(&f) {
                continue;
            }
            assert_eq!(after.face(f), Some(t), "face {f:?}");
            for v in t {
                assert_eq!(
                    after.corner_position(v).map(f64::to_bits),
                    sphere.corner_position(v).map(f64::to_bits)
                );
            }
        }
    }
}

#[test]
fn remesh_keeps_the_mesh_manifold_and_closed() {
    let sphere = uv_sphere(48, 96);
    let faces = band(&sphere, C, 0.5);
    for target in [0.02, 0.06, 0.15] {
        let (after, _) = remesh_band(&sphere, &faces, target, &RunContext::none()).unwrap();
        let (p, t) = after.to_arrays();
        mf_boolean::validate_solid(&p, &t, &RunContext::none())
            .unwrap_or_else(|e| panic!("target {target}: {e}"));
    }
}

#[test]
fn remesh_provenance_is_total() {
    let sphere = uv_sphere(48, 96);
    let faces = band(&sphere, C, 0.5);
    for target in [0.02, 0.15] {
        let (after, prov) = remesh_band(&sphere, &faces, target, &RunContext::none()).unwrap();
        let created: HashSet<FaceId> = prov.created_faces.iter().map(|(f, _)| *f).collect();
        let modified: HashSet<FaceId> = prov.modified_faces.iter().copied().collect();
        let removed: HashSet<FaceId> = prov.removed_faces.iter().copied().collect();
        for (f, t) in after.faces() {
            let kept = sphere.face(f).is_some();
            assert!(created.contains(&f) ^ kept, "face {f:?}");
            if kept && sphere.face(f) != Some(t) {
                assert!(modified.contains(&f), "face {f:?} changed unlisted");
            }
        }
        for (f, _) in sphere.faces() {
            assert!(after.face(f).is_some() ^ removed.contains(&f), "face {f:?}");
        }
        for (_, s) in &prov.created_faces {
            assert!(!s.faces.is_empty());
            assert!(s.faces.iter().all(|&g| sphere.face(g).is_some()));
        }
        // Refining creates faces; coarsening removes them.
        assert!(if target < 0.05 {
            !created.is_empty()
        } else {
            !removed.is_empty()
        });
    }
}

/// With adaptive detail on, a stroke refines the surface under it and the
/// result stays a closed, manifold solid; frozen faces are untouched.
#[test]
fn stroke_with_detail_refines_under_the_brush() {
    let sphere = uv_sphere(48, 96);
    let index = SculptIndex::build(&sphere, &RunContext::none()).unwrap();
    let frozen: Vec<FaceId> = sphere
        .faces()
        .filter(|(_, t)| t.iter().any(|&v| sphere.corner_position(v)[1] > 0.1))
        .map(|(f, _)| f)
        .collect();
    let params = BrushParams {
        kind: BrushKind::Inflate,
        radius: 0.3,
        strength: 0.5,
        falloff: Falloff::Smooth,
        symmetry: [false; 3],
        pivot: [0.0; 3],
        limit_to: None,
        frozen: frozen.clone(),
        view_dir: [-1.0, 0.0, 0.0],
        detail: 0.1,
        stay_on_surface: false,
    };
    let near = |q: [f64; 3]| {
        sphere
            .faces()
            .min_by(|a, b| {
                let d = |t: [u32; 3]| {
                    let p = sphere.corner_position(t[0]);
                    (0..3).map(|k| (p[k] - q[k]).powi(2)).sum::<f64>()
                };
                d(a.1).total_cmp(&d(b.1))
            })
            .unwrap()
            .0
    };
    let mut s = StrokeState::new(&sphere, index, params);
    for y in [-0.2, -0.1, 0.0] {
        let dab = DabSample {
            face: near([1.0, y, 0.0]),
            bary: [1.0 / 3.0; 3],
            pressure: 1.0,
        };
        s.apply(&dab, &RunContext::none()).unwrap();
    }
    let stats = s.remesh_stats();
    let (after, prov) = s.finish().unwrap();
    eprintln!(
        "{stats:?}: {} → {} faces",
        sphere.face_count(),
        after.face_count()
    );
    assert!(stats.splits > 0);
    assert!(after.face_count() > sphere.face_count());
    let (p, t) = after.to_arrays();
    mf_boolean::validate_solid(&p, &t, &RunContext::none()).unwrap();
    for f in frozen {
        let t = sphere.face(f).unwrap();
        assert_eq!(after.face(f), Some(t));
        for v in t {
            assert_eq!(
                after.corner_position(v).map(f64::to_bits),
                sphere.corner_position(v).map(f64::to_bits)
            );
        }
    }
    assert!(!prov.created_faces.is_empty());
}
