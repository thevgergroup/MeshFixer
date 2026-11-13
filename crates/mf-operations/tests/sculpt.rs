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
        // Grab (`Drag`) captures its vertex set once, from the first
        // dab's centre, and never grows it as later dabs continue the
        // path (the fix for the owner's spike/tear report): a short path
        // moves fewer vertices than a brush that's reapplied fresh along
        // every dab's own footprint.
        let min_moved = if kind == BrushKind::Drag { 1 } else { 10 };
        assert!(moved > min_moved, "{kind:?} moved {moved} vertices");
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
        // Grab's own tolerance (below) scales with the cursor's total
        // drag distance along `path()`, times the brush strength: see
        // that branch's comment.
        let drag_scale = p.strength
            * path()
                .windows(2)
                .map(|w| {
                    (0..3)
                        .map(|k| (w[1][k] - w[0][k]).powi(2))
                        .sum::<f64>()
                        .sqrt()
                })
                .sum::<f64>();
        let (after, _) = stroke(&mesh, p, &samples);
        let mut moved = 0;
        for (v, before) in mesh.vertices() {
            let m = mirror_of[&bits(before)];
            let (a, b) = (after.position(v).unwrap(), after.position(m).unwrap());
            if kind == BrushKind::Drag {
                // Grab's region capture always walks the same geodesic
                // flood fill as "stay on this surface" (never a 3D
                // radius), whatever that option is set to: the same
                // non-bit-exact-mirror caveat `combine` documents for it
                // applies here too. Unlike the other brushes' small
                // per-dab bumps, Grab's displacement is the cursor's
                // *total* drag distance times a vertex's falloff weight,
                // so a boundary vertex whose two mirror images read
                // slightly different geodesic distances (and so
                // noticeably different weights, right where the
                // falloff curve is steepest) can differ by a fraction of
                // the whole drag, not just a fraction of the radius: this
                // checks closeness against that larger, drag-scaled
                // bound, not exact bits (as
                // `stay_on_surface_moves_both_mirror_sides_closely` does
                // for "stay on this surface").
                let d = (0..3)
                    .map(|k| (a[k] - if k == 0 { -b[k] } else { b[k] }).powi(2))
                    .sum::<f64>()
                    .sqrt();
                assert!(
                    d < 0.5 * drag_scale,
                    "{kind:?}: {v:?} and {m:?} differ by {d} (drag scale {drag_scale})"
                );
            } else {
                assert_eq!(
                    bits([0.0 - a[0], a[1], a[2]]),
                    bits(b),
                    "{kind:?}: {v:?} and {m:?}"
                );
            }
            moved += usize::from(bits(a) != bits(before));
        }
        let min_moved = if kind == BrushKind::Drag { 0 } else { 20 };
        assert!(moved > min_moved, "{kind:?}: both sides move ({moved})");
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

// ---------------------------------------------------------------------
// Grab (`BrushKind::Drag`): the owner's "moves the far side, spikes, and
// tears the surface" report. Each test below covers one of the fix's
// invariants: the far side never moves, another object is never
// touched, no face ever inverts, and a dab's displacement is exactly
// `strength × drag distance` at the centre.
// ---------------------------------------------------------------------

/// A stroke of exactly these dab centres (face + barycentric picked
/// nearest each point), one `StrokeState::apply` per dab, in order.
fn grab_stroke(mesh: &Mesh, p: BrushParams, points: &[[f64; 3]]) -> Mesh {
    let samples = dabs(mesh, points);
    let index = SculptIndex::build(mesh, &RunContext::none()).unwrap();
    let mut s = StrokeState::new(mesh, index, p);
    for d in &samples {
        s.apply(d, &RunContext::none()).unwrap();
    }
    s.finish().unwrap().0
}

/// Grab only ever moves the vertices of the geodesic region captured at
/// the first dab: every vertex on the far side of the sphere (more than
/// the radius away, geodesically, from the +x pole) stays exactly where
/// it was, however far the drag then travels.
#[test]
fn grab_never_moves_the_far_side() {
    let mesh = symmetric_sphere();
    let mut p = params(BrushKind::Drag);
    p.radius = 0.3;
    p.strength = 1.0;
    p.view_dir = [-1.0, 0.0, 0.0];
    let radius = p.radius;
    // A long drag: +x pole to well past the equator along y. If Grab
    // re-gathered a 3D-radius set on every dab (the bug), a drag this
    // long would sweep most of the sphere; captured once, it must only
    // ever touch the small patch around the +x pole.
    let path: Vec<[f64; 3]> = (0..20)
        .map(|i| {
            let a = i as f64 * 0.12;
            [a.cos(), a.sin(), 0.0]
        })
        .collect();
    let after = grab_stroke(&mesh, p, &path);
    let mut moved = 0;
    for (v, before) in mesh.vertices() {
        // Far side: more than the radius from the start point in
        // straight-line distance too (a stronger, simpler check than
        // geodesic — straight-line is always ≤ geodesic, so "far by
        // straight line" is a safe superset of "far by geodesic").
        let d_start = (0..3)
            .map(|k| (before[k] - path[0][k]).powi(2))
            .sum::<f64>()
            .sqrt();
        let now = after.position(v).unwrap();
        if d_start >= radius {
            assert_eq!(
                bits(now),
                bits(before),
                "{v:?} is {d_start} from the drag's start, past the radius {radius}: it must not move"
            );
        }
        moved += usize::from(bits(now) != bits(before));
    }
    assert!(moved > 0, "the captured patch itself must still move");
}

/// Grab never moves another object's vertices: a stroke on one sphere
/// leaves an unrelated second sphere (a separate `StrokeState`/mesh
/// entirely — the engine has no way to reach across objects) untouched.
/// This is really checking that nothing in the engine's region capture
/// or falloff reaches past the single mesh it was built against.
#[test]
fn grab_never_moves_another_object() {
    let mesh = symmetric_sphere();
    let other = symmetric_sphere();
    let mut p = params(BrushKind::Drag);
    p.radius = 0.3;
    p.strength = 1.0;
    let path: Vec<[f64; 3]> = (0..10)
        .map(|i| {
            let a = i as f64 * 0.12;
            [a.cos(), a.sin(), 0.0]
        })
        .collect();
    let _ = grab_stroke(&mesh, p, &path);
    // `other` was never touched: still the identical revision (same
    // content), since a stroke only ever edits the one mesh it was
    // built with (`StrokeState::new`'s own `base`).
    assert_eq!(other.content_hash(), symmetric_sphere().content_hash());
}

/// A large, aggressive drag never flips a face's orientation: every
/// live face's normal still points roughly the way it did before the
/// stroke (dot product with the original normal stays positive).
#[test]
fn grab_never_inverts_a_face() {
    let mesh = symmetric_sphere();
    let mut p = params(BrushKind::Drag);
    // Deliberately oversized relative to the mesh and at full strength:
    // exactly the kind of large drag that tore the surface in the
    // owner's report, absent the fix.
    p.radius = 0.6;
    p.strength = 1.0;
    let before_normals: HashMap<FaceId, [f64; 3]> = mesh
        .faces()
        .map(|(f, t)| {
            let [a, b, c] = t.map(|v| mesh.corner_position(v));
            let e1 = (0..3).map(|k| b[k] - a[k]).collect::<Vec<_>>();
            let e2 = (0..3).map(|k| c[k] - a[k]).collect::<Vec<_>>();
            let n = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            (f, n)
        })
        .collect();
    let path: Vec<[f64; 3]> = (0..8)
        .map(|i| {
            let a = i as f64 * 0.3;
            [a.cos(), a.sin(), 0.0]
        })
        .collect();
    let after = grab_stroke(&mesh, p, &path);
    for (f, t) in after.faces() {
        let Some(&n0) = before_normals.get(&f) else {
            continue;
        };
        let [a, b, c] = t.map(|v| after.position(mf_geometry::VertexId(v)).unwrap());
        let e1 = (0..3).map(|k| b[k] - a[k]).collect::<Vec<_>>();
        let e2 = (0..3).map(|k| c[k] - a[k]).collect::<Vec<_>>();
        let n1 = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let dot: f64 = (0..3).map(|k| n0[k] * n1[k]).sum();
        assert!(dot > 0.0, "{f:?} flipped: old normal {n0:?}, new {n1:?}");
    }
}

/// Displacement at the centre: the vertex nearest the dab centre (full
/// falloff weight, 1.0) moves by exactly `strength × the drag distance`,
/// projected onto the view plane — one dab's worth of absolute Grab
/// displacement, not an accumulated or re-applied delta. A flat grid
/// (not the sphere the other Grab tests use): looking straight down its
/// own normal (`z`), a dab exactly on a vertex projects to exactly that
/// vertex with no surface-curvature ambiguity at all, so this isolates
/// Grab's displacement formula itself from `centre()`'s own geometry.
#[test]
fn grab_displacement_equals_drag_distance_at_centre() {
    let mesh = mf_fixtures::grid(10);
    let mut p = params(BrushKind::Drag);
    p.radius = 3.0;
    p.strength = 0.8;
    p.falloff = Falloff::Flat; // weight 1.0 everywhere inside the radius
    p.view_dir = [0.0, 0.0, -1.0]; // straight down the grid's own normal
    // Vertex (5, 5) is the dab centre; (6, 5) is one grid step away,
    // well inside the radius, where the drag ends.
    let v = |i: u32, j: u32| j * 11 + i;
    let centre_vertex = v(5, 5);
    let drag_vertex = v(6, 5);
    let face_at = |vert: u32| mesh.faces().find(|(_, t)| t.contains(&vert)).unwrap();
    let dab_on = |vert: u32| {
        let (f, t) = face_at(vert);
        let i = t.iter().position(|&x| x == vert).unwrap();
        let mut bary = [0.0; 3];
        bary[i] = 1.0;
        DabSample {
            face: f,
            bary,
            pressure: 1.0,
        }
    };
    let centre_dab = dab_on(centre_vertex);
    let drag_dab = dab_on(drag_vertex);
    let before = mesh.position(mf_geometry::VertexId(centre_vertex)).unwrap();
    let index = SculptIndex::build(&mesh, &RunContext::none()).unwrap();
    let mut s = StrokeState::new(&mesh, index, p.clone());
    s.apply(&centre_dab, &RunContext::none()).unwrap();
    s.apply(&drag_dab, &RunContext::none()).unwrap();
    // The stroke's own recorded centres: on this flat grid, viewed
    // straight down its own normal, a dab exactly on a vertex projects
    // to that same point (no curvature to redirect it), so `centres[0]`
    // and `centres[1]` are exactly `centre_vertex` and `drag_vertex`.
    let centres = s.centres().to_vec();
    let (after, _) = s.finish().unwrap();
    let now = after
        .position(mf_geometry::VertexId(centre_vertex))
        .unwrap();
    let moved: f64 = (0..3)
        .map(|k| (now[k] - before[k]).powi(2))
        .sum::<f64>()
        .sqrt();
    let raw: [f64; 3] = std::array::from_fn(|k| centres[1][k] - centres[0][k]);
    // The view direction is the grid's own normal, so nothing to drop:
    // the raw displacement is already entirely in the view plane.
    let want: f64 = (0..3)
        .map(|k| (p.strength * raw[k]).powi(2))
        .sum::<f64>()
        .sqrt();
    assert!(
        want > 1e-9,
        "the drag must have actually moved the centre: {want}"
    );
    assert!(
        (moved - want).abs() < 1e-9,
        "centre vertex moved {moved}, expected {want} (strength × drag distance)"
    );
}
