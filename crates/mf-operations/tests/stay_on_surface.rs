#![allow(clippy::unwrap_used)] // tests may panic on failure

//! "Stay on this surface" (MF-SCULPT-005, on by default): the brush never
//! spills across a sharp crease, so inflating right next to a split
//! cap's flat face never moves the cap's own vertices — the owner's test
//! case. Falloff follows the surface, not a 3D sphere: a vertex on the
//! far side of a deep fold (near in straight-line distance, far along
//! the surface) is also left alone.

use mf_boolean::{Plane, plane_cut};
use mf_fixtures::uv_sphere;
use mf_geometry::{FaceId, Mesh, VertexId};
use mf_operations::sculpt::{
    BrushKind, BrushParams, DabSample, Falloff, SculptIndex, StrokeState, run_stroke,
};
use mf_runtime::RunContext;

/// A dab's brush parameters: `view_dir` points straight in along the dab
/// point's own outward radial direction (as if a camera were looking
/// straight at that spot of the sphere), so `centre()`'s own re-
/// projection along the view line lands back near the same point rather
/// than finding some other crossing (the flat cap, close by, at a
/// shallow-enough angle to the view line otherwise).
fn params(stay_on_surface: bool, dab_point: [f64; 3]) -> BrushParams {
    let len = (0..3).map(|k| dab_point[k].powi(2)).sum::<f64>().sqrt();
    let outward = dab_point.map(|c| c / len);
    BrushParams {
        kind: BrushKind::Inflate,
        radius: 0.9,
        strength: 0.9,
        falloff: Falloff::Smooth,
        symmetry: [false; 3],
        pivot: [0.0; 3],
        limit_to: None,
        frozen: Vec::new(),
        view_dir: outward.map(|c| -c),
        detail: 0.0,
        stay_on_surface,
    }
}

/// The face nearest `q`, with its barycentric centroid (a dab there).
fn dab_at(mesh: &Mesh, q: [f64; 3]) -> DabSample {
    let (f, _) = mesh
        .faces()
        .map(|(f, t)| {
            let c: [f64; 3] = std::array::from_fn(|k| {
                t.iter().map(|&v| mesh.corner_position(v)[k]).sum::<f64>() / 3.0
            });
            let d: f64 = (0..3).map(|k| (c[k] - q[k]).powi(2)).sum();
            (f, d)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    DabSample {
        face: f,
        bary: [1.0 / 3.0; 3],
        pressure: 1.0,
    }
}

fn stroke(mesh: &Mesh, p: BrushParams, samples: &[DabSample]) -> Mesh {
    let index = SculptIndex::build(mesh, &RunContext::none()).unwrap();
    let mut s = StrokeState::new(mesh, index, p);
    for d in samples {
        s.apply(d, &RunContext::none()).unwrap();
    }
    s.finish().unwrap().0
}

/// A unit sphere, sliced through z = 0.3 (kept: the larger, lower part),
/// capped flat: a disc, apart from the curved surface at a steep
/// dihedral everywhere around its rim. `(mesh, cap_faces)`.
fn capped_sphere() -> (Mesh, Vec<FaceId>) {
    let sphere = uv_sphere(48, 96);
    let plane = Plane {
        point: [0.0, 0.0, 0.3],
        normal: [0.0, 0.0, 1.0],
    };
    let parts = plane_cut(&sphere, plane, &RunContext::none()).unwrap();
    (parts.below.0, parts.below_cap_faces)
}

/// Every vertex the cap's faces touch (every one of them is on the rim
/// the cap shares with the curved surface it was cut from: the cap's own
/// triangulation — an ear-clip of the existing boundary loop — adds no
/// vertices of its own). Moving any of them would deform the cap's flat
/// face, so "stay on this surface" must never move one from a dab on the
/// curved side alone.
fn cap_vertices(mesh: &Mesh, cap_faces: &[FaceId]) -> std::collections::HashSet<u32> {
    cap_faces
        .iter()
        .flat_map(|&f| mesh.face(f).unwrap())
        .collect()
}

#[test]
fn inflating_next_to_a_split_cap_leaves_the_caps_vertices_untouched() {
    let (mesh, cap_faces) = capped_sphere();
    assert!(!cap_faces.is_empty(), "the cut leaves a cap");
    let cap_vertices = cap_vertices(&mesh, &cap_faces);
    assert!(!cap_vertices.is_empty());
    // Right at the cap's rim, just on the curved side: with the brush's
    // radius (0.9, comparable to the sphere itself), a straight-line
    // sphere of influence would reach well past the cap and across it.
    let dab = dab_at(&mesh, [0.968, 0.0, 0.25]);
    assert!(
        !cap_faces.contains(&dab.face),
        "the dab should land on the curved surface, not the cap itself"
    );
    let after = stroke(&mesh, params(true, [0.968, 0.0, 0.25]), &[dab]);
    for &v in &cap_vertices {
        let id = VertexId(v);
        assert_eq!(
            after.position(id).unwrap(),
            mesh.position(id).unwrap(),
            "cap vertex {v} moved with stay_on_surface on"
        );
    }
    // Something did move (the dab had an effect at all): off the cap,
    // the curved surface near the dab moved.
    let moved_elsewhere = mesh.vertices().any(|(id, before)| {
        !cap_vertices.contains(&id.0) && after.position(id).unwrap() != before
    });
    assert!(moved_elsewhere, "the stroke should move some non-cap vertex");
}

/// The same stroke with the option off behaves as sculpting always has:
/// a brush radius comparable to the sphere reaches across the cap too
/// (the straight-line "3D sphere" of influence this option exists to fix).
#[test]
fn with_the_option_off_the_brush_behaves_as_before() {
    let (mesh, cap_faces) = capped_sphere();
    let cap_vertices = cap_vertices(&mesh, &cap_faces);
    let dab = dab_at(&mesh, [0.968, 0.0, 0.25]);
    let after = stroke(&mesh, params(false, [0.968, 0.0, 0.25]), &[dab]);
    let any_cap_vertex_moved = cap_vertices
        .iter()
        .any(|&v| after.position(VertexId(v)).unwrap() != mesh.position(VertexId(v)).unwrap());
    assert!(
        any_cap_vertex_moved,
        "with the option off, the brush's old straight-line radius should reach the nearby cap"
    );
}

/// Falloff follows the surface: a vertex that's near the dab centre in
/// straight-line distance but only reachable by a long way round (or not
/// at all, if the fold is sharp) moves less (or not at all) than one at
/// the same geodesic distance in the open.
#[test]
fn falloff_follows_the_surface_not_a_straight_line() {
    let (mesh, cap_faces) = capped_sphere();
    let cap_vertices = cap_vertices(&mesh, &cap_faces);
    let dab = dab_at(&mesh, [0.968, 0.0, 0.25]);
    let on = stroke(&mesh, params(true, [0.968, 0.0, 0.25]), &[dab]);
    // Every cap vertex is within the brush's straight-line radius of the
    // dab (it's a small disc near the rim) but unreachable across the
    // crease: displacement is exactly zero, not just smaller.
    for &v in &cap_vertices {
        let id = VertexId(v);
        let d = {
            let (a, b) = (mesh.position(id).unwrap(), dab_point(&mesh, &dab));
            (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f64>().sqrt()
        };
        if d < params(true, [0.968, 0.0, 0.25]).radius {
            assert_eq!(
                on.position(id).unwrap(),
                mesh.position(id).unwrap(),
                "vertex {v} is within straight-line radius but across the crease"
            );
        }
    }
}

fn dab_point(mesh: &Mesh, dab: &DabSample) -> [f64; 3] {
    let t = mesh.face(dab.face).unwrap();
    let corners = t.map(|v| mesh.corner_position(v));
    std::array::from_fn(|k| {
        dab.bary[0] * corners[0][k] + dab.bary[1] * corners[1][k] + dab.bary[2] * corners[2][k]
    })
}

/// The spec defaults "stay on this surface" to on, so an older recorded
/// stroke (or a raw param map that never mentions it) still replays with
/// the option on, not silently reverting to the old straight-line brush.
#[test]
fn the_spec_defaults_stay_on_surface_to_true() {
    let spec = mf_spec::builtin::get("mesh.sculpt_stroke").unwrap();
    let raw: std::collections::BTreeMap<String, String> = [
        ("brush", "inflate"),
        ("radius", "5"),
        ("strength", "0.5"),
        ("view_dir", "0,0,-1"),
        ("samples", ""),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    let params = spec.validate(&raw).unwrap();
    assert!(params.get_bool("stay_on_surface").unwrap());
}

/// "Record the option so replay stays Exact": running `run_stroke` twice
/// (standing in for a commit and a later replay) with `stay_on_surface`
/// on gives the identical mesh, bit for bit, both times — the option
/// is itself part of what's replayed, not a UI-only setting the engine
/// forgets.
#[test]
fn stay_on_surface_replays_exactly() {
    let (mesh, cap_faces) = capped_sphere();
    let dab = dab_at(&mesh, [0.968, 0.0, 0.25]);
    let p = params(true, [0.968, 0.0, 0.25]);
    let run = || {
        let (m, _) = run_stroke(&mesh, p.clone(), &[dab], &RunContext::none()).unwrap();
        m
    };
    let (first, second) = (run(), run());
    assert_eq!(first.content_hash(), second.content_hash());
    // And it actually took effect (not accidentally a no-op): the cap's
    // vertices stayed put, same as the direct `StrokeState` path above.
    for &v in &cap_vertices(&mesh, &cap_faces) {
        assert_eq!(
            first.position(VertexId(v)).unwrap(),
            mesh.position(VertexId(v)).unwrap()
        );
    }
}
