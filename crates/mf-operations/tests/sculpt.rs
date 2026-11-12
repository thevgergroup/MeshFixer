#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The stroke engine (Phase 1 Task 17, MF-SCULPT-001, MF-SCULPT-003):
//! brushes, falloff, symmetry, masks, and `mesh.sculpt_stroke`.

use std::collections::{BTreeMap, HashMap};

use mf_document::ObjectId;
use mf_fixtures::uv_sphere;
use mf_geometry::{FaceId, Mesh};
use mf_io::threemf::IDENTITY;
use mf_operations::sculpt::{
    BrushKind, BrushParams, DabSample, Falloff, SculptIndex, StrokeState, run_stroke,
};
use mf_operations::{DocOutput, ObjectChange, OperandInput, RunContext, Target, find};

/// A UV sphere snapped to a 2⁻²⁰ grid, so it's exactly symmetric across
/// x = 0 (checked).
fn symmetric_sphere() -> Mesh {
    let (p, t) = uv_sphere(32, 64).to_arrays();
    let q = (1u64 << 20) as f64;
    let p: Vec<[f64; 3]> = p
        .iter()
        .map(|v| v.map(|c| (c * q).round() / q + 0.0))
        .collect();
    let set: std::collections::HashSet<[u64; 3]> = p.iter().map(|v| v.map(f64::to_bits)).collect();
    assert!(
        p.iter()
            .all(|v| set.contains(&[(0.0 - v[0]).to_bits(), v[1].to_bits(), v[2].to_bits()])),
        "the fixture is exactly symmetric"
    );
    Mesh::new(p, t).unwrap()
}

fn params(kind: BrushKind) -> BrushParams {
    BrushParams {
        kind,
        radius: 0.3,
        strength: 0.8,
        falloff: Falloff::Smooth,
        symmetry: [false; 3],
        pivot: [0.0; 3],
        limit_to: None,
        frozen: Vec::new(),
        view_dir: [-1.0, 0.0, 0.0],
        detail: 0.0,
        // This file's existing tests (symmetry, masks…) were written
        // against the straight-line falloff; `stay_on_surface.rs` covers
        // the geodesic, crease-aware one.
        stay_on_surface: false,
    }
}

/// Dabs at the centroids of the faces nearest each of `points`.
fn dabs(mesh: &Mesh, points: &[[f64; 3]]) -> Vec<DabSample> {
    points
        .iter()
        .map(|q| {
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
        })
        .collect()
}

/// A short path across the +x side of the sphere.
fn path() -> Vec<[f64; 3]> {
    (0..4)
        .map(|i| {
            let a = -0.3 + 0.2 * i as f64;
            [a.cos() * 0.9, a.sin() * 0.9, 0.2]
        })
        .collect()
}

fn stroke(mesh: &Mesh, p: BrushParams, samples: &[DabSample]) -> (Mesh, Vec<[f64; 3]>) {
    let index = SculptIndex::build(mesh, &RunContext::none()).unwrap();
    let mut s = StrokeState::new(mesh, index, p);
    for d in samples {
        s.apply(d, &RunContext::none()).unwrap();
    }
    let centres = s.centres().to_vec();
    (s.finish().unwrap().0, centres)
}

fn bits(p: [f64; 3]) -> [u64; 3] {
    p.map(f64::to_bits)
}

#[test]
fn each_brush_moves_only_vertices_within_radius() {
    let mesh = symmetric_sphere();
    let samples = dabs(&mesh, &path());
    for kind in BrushKind::ALL {
        let p = params(kind);
        let r = p.radius;
        let (after, centres) = stroke(&mesh, p, &samples);
        let mut moved = 0;
        for (v, before) in mesh.vertices() {
            let now = after.position(v).unwrap();
            let far = centres
                .iter()
                .all(|c| (0..3).map(|k| (before[k] - c[k]).powi(2)).sum::<f64>() >= r * r);
            if far {
                assert_eq!(bits(now), bits(before), "{kind:?} moved {v:?} outside");
            }
            moved += usize::from(bits(now) != bits(before));
        }
        assert!(moved > 10, "{kind:?} moved {moved} vertices");
    }
}

#[test]
fn frozen_faces_never_move() {
    let mesh = symmetric_sphere();
    let samples = dabs(&mesh, &path());
    // Freeze every face with a corner at y > 0.
    let frozen: Vec<FaceId> = mesh
        .faces()
        .filter(|(_, t)| t.iter().any(|&v| mesh.corner_position(v)[1] > 0.0))
        .map(|(f, _)| f)
        .collect();
    let mut p = params(BrushKind::Inflate);
    p.frozen = frozen.clone();
    let (after, _) = stroke(&mesh, p, &samples);
    let mut moved = 0;
    for &f in &frozen {
        for v in mesh.face(f).unwrap() {
            let id = mf_geometry::VertexId(v);
            assert_eq!(
                bits(after.position(id).unwrap()),
                bits(mesh.position(id).unwrap())
            );
        }
    }
    for (v, p) in mesh.vertices() {
        moved += usize::from(bits(after.position(v).unwrap()) != bits(p));
    }
    assert!(moved > 10, "the rest still moves");
}

#[test]
fn symmetry_x_mirrors_displacement_exactly() {
    let mesh = symmetric_sphere();
    let samples = dabs(&mesh, &path());
    let mirror_of: HashMap<[u64; 3], mf_geometry::VertexId> = mesh
        .vertices()
        .map(|(v, p)| (bits([0.0 - p[0], p[1], p[2]]), v))
        .collect();
    for kind in BrushKind::ALL {
        let mut p = params(kind);
        p.symmetry = [true, false, false];
        let (after, _) = stroke(&mesh, p, &samples);
        let mut moved = 0;
        for (v, before) in mesh.vertices() {
            let m = mirror_of[&bits(before)];
            let (a, b) = (after.position(v).unwrap(), after.position(m).unwrap());
            assert_eq!(
                bits([0.0 - a[0], a[1], a[2]]),
                bits(b),
                "{kind:?}: {v:?} and {m:?}"
            );
            moved += usize::from(bits(a) != bits(before));
        }
        assert!(moved > 20, "{kind:?}: both sides move ({moved})");
    }
}

/// "Stay on this surface" on a smooth, unbroken sphere (no creases to
/// stop at): both sides of the mirror still move about the same amount,
/// to a loose tolerance (not bit for bit, and not necessarily close to
/// machine precision: `uv_sphere`'s quad diagonals all run the same way
/// in absolute terms, not mirrored, so the two sides' *triangulations*
/// aren't true mirrors of each other even though every vertex position
/// is; a geodesic flood fill can read that real difference in a way
/// straight-line distance never could. The combine step always
/// evaluates each mirror image independently with this option on —
/// see `combine`'s own doc comment — rather than risk mirroring a
/// wrong number bit for bit), and the stroke still does something.
#[test]
fn stay_on_surface_moves_both_mirror_sides_closely() {
    let mesh = symmetric_sphere();
    let samples = dabs(&mesh, &path());
    let mirror_of: HashMap<[u64; 3], mf_geometry::VertexId> = mesh
        .vertices()
        .map(|(v, p)| (bits([0.0 - p[0], p[1], p[2]]), v))
        .collect();
    let mut p = params(BrushKind::Inflate);
    p.symmetry = [true, false, false];
    p.stay_on_surface = true;
    let radius = p.radius;
    let (after, _) = stroke(&mesh, p, &samples);
    let mut moved = 0;
    let mut max_diff = 0.0_f64;
    for (v, before) in mesh.vertices() {
        let m = mirror_of[&bits(before)];
        let (a, b) = (after.position(v).unwrap(), after.position(m).unwrap());
        let d = (0..3)
            .map(|k| (a[k] - if k == 0 { -b[k] } else { b[k] }).powi(2))
            .sum::<f64>()
            .sqrt();
        max_diff = max_diff.max(d);
        moved += usize::from(bits(a) != bits(before));
    }
    assert!(moved > 20, "both sides move ({moved})");
    assert!(
        max_diff < 0.25 * radius,
        "the two mirror sides should move closely: worst difference {max_diff}"
    );
}

fn sculpt_command(mesh: &Mesh, samples: &[DabSample], extra: &[(&str, &str)]) -> DocOutput {
    let spec = mf_spec::builtin::get("mesh.sculpt_stroke").unwrap();
    let text: Vec<String> = samples
        .iter()
        .map(|s| {
            s.to_array()
                .iter()
                .map(f64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect();
    let mut raw: BTreeMap<String, String> = [
        ("brush", "inflate"),
        ("radius", "0.3"),
        ("strength", "0.8"),
        ("view_dir", "-1,0,0"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    raw.insert("samples".into(), text.join(";"));
    for (k, v) in extra {
        raw.insert(k.to_string(), v.to_string());
    }
    let params = spec.validate(&raw).unwrap();
    find("mesh.sculpt_stroke")
        .unwrap()
        .execute_doc(
            &[OperandInput {
                slot: "target",
                object: ObjectId(1),
                name: "scan",
                mesh,
                transform: IDENTITY,
                target: Target::WholeObject,
            }],
            &params,
            &RunContext::none(),
        )
        .unwrap()
}

fn geometry(out: &DocOutput) -> Mesh {
    match out.changes.as_slice() {
        [ObjectChange::Geometry { mesh, .. }] => mesh.clone(),
        other => panic!("{other:?}"),
    }
}

/// The live stroke (dab by dab, with previews in between) and the
/// command replaying its samples give the same mesh, bit for bit.
#[test]
fn live_stroke_equals_replayed_stroke_hash() {
    let mesh = symmetric_sphere();
    let samples = dabs(&mesh, &path());
    let index = SculptIndex::build(&mesh, &RunContext::none()).unwrap();
    let mut live = StrokeState::new(&mesh, index, params(BrushKind::Inflate));
    for d in &samples {
        let delta = live.apply(d, &RunContext::none()).unwrap();
        assert!(!delta.face_chunks.is_empty());
        let _ = live.preview_mesh().content_hash();
    }
    let (live_mesh, live_prov) = live.finish().unwrap();
    // `live` uses `params()`'s `stay_on_surface: false`; match it so this
    // compares the same brush configuration (a separate test,
    // `stay_on_surface.rs`, is live vs. replayed with the option on).
    let out = sculpt_command(&mesh, &samples, &[("stay_on_surface", "false")]);
    let replayed = geometry(&out);
    assert_eq!(live_mesh.content_hash(), replayed.content_hash());
    match &out.changes[0] {
        ObjectChange::Geometry { provenance, .. } => assert_eq!(*provenance, live_prov),
        _ => unreachable!(),
    }
    // Only moved vertices: no faces created or removed.
    assert!(live_prov.created_faces.is_empty() && live_prov.removed_faces.is_empty());
    assert!(!live_prov.moved_vertices.is_empty());
}

#[test]
fn stroke_is_strict_at_1_and_n_threads() {
    let mesh = uv_sphere(200, 400);
    let samples = dabs(&mesh, &path());
    let hash = |threads: usize| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| {
                let out = sculpt_command(&mesh, &samples, &[("symmetry_x", "true")]);
                geometry(&out).content_hash()
            })
    };
    assert_eq!(hash(1), hash(8));
}

/// MF-HIST-002: the samples name faces of the input revision, which keep
/// their ids through an upstream fill elsewhere (fill only appends), so
/// the same recorded stroke re-executes after the fill and does the same
/// thing where it was drawn.
#[test]
fn samples_remap_across_an_upstream_fill() {
    let sphere = uv_sphere(32, 64);
    // A hole at the south pole, far from the stroke.
    let mut e = sphere.edit();
    for (f, t) in sphere.faces() {
        if t.iter().all(|&v| sphere.corner_position(v)[2] < -0.95) {
            e.remove_face(f).unwrap();
        }
    }
    let holed = e.finish().unwrap().0;
    let samples = dabs(&holed, &path());
    let before_fill = geometry(&sculpt_command(&holed, &samples, &[]));

    let fill = find("mesh.fill_holes").unwrap();
    let fill_params = mf_spec::builtin::get("mesh.fill_holes")
        .unwrap()
        .validate(&BTreeMap::new())
        .unwrap();
    let filled = fill
        .execute(
            &holed,
            &Target::WholeObject,
            &fill_params,
            &RunContext::none(),
        )
        .unwrap()
        .change
        .unwrap()
        .0;
    assert!(
        samples
            .iter()
            .all(|s| filled.face(s.face) == holed.face(s.face))
    );
    let after_fill = geometry(&sculpt_command(&filled, &samples, &[]));
    // Every vertex the stroke moved moved to the same place.
    for (v, p) in holed.vertices() {
        let (a, b) = (
            before_fill.position(v).unwrap(),
            after_fill.position(v).unwrap(),
        );
        if bits(a) != bits(p) {
            assert_eq!(bits(a), bits(b), "{v:?}");
        }
    }
    // A sample on a face that's gone is refused by name.
    let mut e = holed.edit();
    e.remove_face(samples[0].face).unwrap();
    let gone = e.finish().unwrap().0;
    let index = SculptIndex::build(&gone, &RunContext::none()).unwrap();
    let s = StrokeState::new(&gone, index, params(BrushKind::Inflate));
    match s.check_sample(&samples[0]) {
        Err(mf_operations::OpError::Failed { code, .. }) => assert_eq!(code, "bad_sample"),
        other => panic!("{other:?}"),
    }
}

/// `run_stroke` is the command's path; the frozen list round-trips
/// through the spec's `faces` parameter.
#[test]
fn frozen_param_round_trips() {
    let mesh = symmetric_sphere();
    let samples = dabs(&mesh, &path());
    let frozen: Vec<String> = (0..mesh.face_slots()).map(|f| f.to_string()).collect();
    let out = sculpt_command(&mesh, &samples, &[("frozen", &frozen.join(","))]);
    assert!(out.changes.is_empty(), "everything frozen: nothing moves");
    let (m, _) = run_stroke(
        &mesh,
        params(BrushKind::Smooth),
        &samples,
        &RunContext::none(),
    )
    .unwrap();
    assert_ne!(m.content_hash(), mesh.content_hash());
}
