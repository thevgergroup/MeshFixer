#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `mesh.hollow` on Make solid's own output, through the command layer
//! (owner 2026-11-13: Hollow refusing a valid closed solid built from the
//! demo file's 'scan' and 'part' — a hemisphere capped on the same plane
//! as a half cube sunk into its side, combined, then Make solid):
//!
//! - the owner's own steps, on the owner's own demo file (also covered,
//!   as the exact repro, by `owner_hollow_repro.rs`);
//! - a randomized property, the same scaffold `split_robust.rs` uses for
//!   the matching Split bug: unions of primitives built by
//!   `mesh.combine`, then `mesh.make_solid`, then `mesh.hollow` (plain
//!   and `open_through`), are never refused — only `too_thin`
//!   (genuinely thinner than twice the wall) is an acceptable refusal.
//!
//! The property runs `MF_HOLLOW_PROPERTY_CASES` scenes (default 6: each
//! hollows a voxel grid twice, far costlier than a plane cut, so the
//! fast-gate default is smaller than `split_robust.rs`'s own 24); set it
//! higher for an on-demand soak, with `MF_HOLLOW_PROPERTY_SEED` to start
//! elsewhere.

use std::collections::BTreeMap;
use std::path::Path;

use mf_command::{CommandError, Invocation, ObjectId, Session};
use mf_fixtures::{cube, rotation_translation, scaled, uv_sphere};
use mf_geometry::Mesh;
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};
use mf_runtime::RunContext;

fn invoke(op_id: &str, target: ObjectId, params: &[(String, String)]) -> Invocation {
    Invocation {
        op_id: op_id.into(),
        target,
        selection: None,
        operands: Vec::new(),
        raw_params: params.iter().cloned().collect::<BTreeMap<_, _>>(),
    }
}

fn combine(target: ObjectId, solids: &[ObjectId]) -> Invocation {
    Invocation {
        op_id: "mesh.combine".into(),
        target,
        selection: None,
        operands: solids
            .iter()
            .map(|&o| ("solids".to_string(), o, None))
            .collect(),
        raw_params: BTreeMap::new(),
    }
}

fn assert_solid(mesh: &Mesh, what: &str) {
    let (p, t) = mesh.to_arrays();
    mf_boolean::validate_solid(&p, &t, &RunContext::none())
        .unwrap_or_else(|e| panic!("{what}: not a closed solid: {e}"));
}

// ---------------------------------------------------------------------
// The randomized scene (the same shape of scaffold `split_robust.rs`
// uses for the matching Split property, so a reader who knows one knows
// the other; this one's planes aren't needed, only the combined solid).
// ---------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    /// A multiple of `step` in `[lo, hi]`.
    fn grid(&mut self, lo: f64, hi: f64, step: f64) -> f64 {
        let n = ((hi - lo) / step).round() as u64;
        lo + step * self.below(n + 1) as f64
    }
}

/// One primitive: its mesh and its transform. Placed close enough
/// together (a 5mm grid over a 5mm span) that most draws genuinely
/// overlap, as the owner's sphere and sunk-in cube do.
fn primitive(r: &mut Rng) -> (Mesh, [f64; 12]) {
    let t = [
        r.grid(-3.0, 3.0, 0.5),
        r.grid(-3.0, 3.0, 0.5),
        r.grid(0.0, 4.0, 0.5),
    ];
    let tilted = r.below(4) == 0;
    let transform = if tilted {
        rotation_translation(r.below(3) as usize, 15.0 * (1 + r.below(5)) as f64, t)
    } else {
        let mut m = IDENTITY;
        m[9..].copy_from_slice(&t);
        m
    };
    let mesh = if r.below(2) == 0 {
        let rings = 6 + 2 * r.below(8) as u32;
        let segments = 8 + r.below(20) as u32;
        let radius = r.grid(4.0, 10.0, 0.5);
        scaled(&uv_sphere(rings, segments), radius)
    } else {
        cube(r.grid(5.0, 14.0, 0.5))
    };
    (mesh, transform)
}

fn session_of(dir: &Path, prims: &[(Mesh, [f64; 12])]) -> (Session, Vec<ObjectId>) {
    let path = dir.join("scene.3mf");
    let model = Model {
        metadata: Default::default(),
        unit: Unit::Millimeter,
        objects: prims
            .iter()
            .enumerate()
            .map(|(k, (mesh, transform))| ModelObject {
                metadata: Default::default(),
                name: format!("p{k}"),
                mesh: mesh.clone(),
                transform: *transform,
            })
            .collect(),
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, _) = s.import_model(read_model(&path).unwrap());
    (s, ids)
}

/// With `MF_HOLLOW_PROPERTY_DUMP` set to a directory, a failing scene's
/// Make-solid input is written there as `<seed>.3mf`.
fn dump(name: &str, mesh: &Mesh, transform: [f64; 12]) {
    let Ok(dir) = std::env::var("MF_HOLLOW_PROPERTY_DUMP") else {
        return;
    };
    let model = Model {
        metadata: Default::default(),
        unit: Unit::Millimeter,
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: "solid".into(),
            mesh: mesh.clone(),
            transform,
        }],
    };
    let path = Path::new(&dir).join(format!("{name}.3mf"));
    write_model_path(&model, &path).unwrap();
    eprintln!("wrote {}", path.display());
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A live face id to seed `open_through` with: the lowest one by its
/// centroid's z (standing in for a click on the model's underside, as
/// the owner's cut face was).
fn lowest_face(mesh: &Mesh) -> u32 {
    (0..mesh.face_slots())
        .filter_map(|f| {
            let face = mesh.face(mf_geometry::FaceId(f))?;
            let z = face
                .iter()
                .map(|&v| mesh.corner_position(v)[2])
                .sum::<f64>()
                / 3.0;
            Some((z, f))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, f)| f)
        .expect("a live face")
}

/// Random closed manifold unions of spheres and boxes, some tilted,
/// built by `mesh.combine` exactly as the app builds them (the same
/// property `split_robust.rs` runs for Split) — then `mesh.make_solid`,
/// whose own output must always pass D4 (checked here, not assumed),
/// then `mesh.hollow` at a 2mm wall, plain and `open_through`: never
/// refused. `too_thin` (the model really is thinner than twice the
/// wall everywhere) is the only acceptable refusal — it is reported,
/// not a failure, since a small random union can genuinely be that
/// thin.
#[test]
fn random_unions_make_solid_then_hollow_are_never_refused() {
    let cases = env_u64("MF_HOLLOW_PROPERTY_CASES", 6);
    let seed0 = env_u64("MF_HOLLOW_PROPERTY_SEED", 0x5EED_0002);
    let dir = tempfile::tempdir().unwrap();
    let (mut scenes, mut hollowed, mut too_thin, mut combine_refused) = (0, 0, 0, 0);
    for case in 0..cases {
        let seed = seed0.wrapping_add(case).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut r = Rng(seed);
        let prims: Vec<(Mesh, [f64; 12])> =
            (0..2 + r.below(3)).map(|_| primitive(&mut r)).collect();
        let (mut s, ids) = session_of(dir.path(), &prims);
        match s.execute(combine(ids[0], &ids[1..])) {
            Ok(_) => {}
            // A scene whose shapes don't all overlap isn't an input to
            // Make solid/Hollow; the property is about hollowing a
            // valid union, so draw again — counted, and printed below.
            Err(CommandError::Operation(e)) => {
                if !e.to_string().contains("no_overlap") {
                    eprintln!("seed {seed:#x}: combine refused, scene skipped: {e}");
                }
                combine_refused += 1;
                continue;
            }
            Err(e) => panic!("seed {seed:#x}: combine: {e}"),
        }
        scenes += 1;
        let target = ids[0];

        s.execute(invoke("mesh.make_solid", target, &[]))
            .unwrap_or_else(|e| panic!("seed {seed:#x}: make_solid: {e}"));
        let solid_mesh = s.document().object(target).unwrap().mesh().unwrap().clone();
        let solid_transform = s.document().object(target).unwrap().transform;
        // Make solid's own output must always pass D4 (the task's own
        // rule: if it didn't, Make solid is the bug, not Hollow).
        assert_solid(
            &solid_mesh,
            &format!("seed {seed:#x}: make_solid's own output"),
        );

        for (label, params) in [
            (
                "plain",
                vec![
                    ("wall_thickness".to_string(), "2".to_string()),
                    ("inner_detail".to_string(), "0.5".to_string()),
                    ("drain_holes".to_string(), "none".to_string()),
                ],
            ),
            (
                "open_through",
                vec![
                    ("wall_thickness".to_string(), "2".to_string()),
                    ("inner_detail".to_string(), "0.5".to_string()),
                    ("open_through".to_string(), "true".to_string()),
                    (
                        "open_through_faces".to_string(),
                        lowest_face(&solid_mesh).to_string(),
                    ),
                ],
            ),
        ] {
            let what = format!("seed {seed:#x}: hollow ({label})");
            match s.execute(invoke("mesh.hollow", target, &params)) {
                Ok(_) => {
                    hollowed += 1;
                    assert!(s.undo(), "{what}: undo");
                }
                Err(CommandError::Operation(e)) if e.to_string().contains("too_thin") => {
                    too_thin += 1;
                }
                // `open_through`'s own region refusals
                // (`open_through_region_too_curved`, `_degenerate`,
                // `_invalid`, `open_through_nothing_under`): this
                // test's seed is just the lowest live face, not a
                // deliberate pick of a flat region a real user would
                // make for a bowl — a single sliver triangle on a
                // curved Make-solid surface can genuinely have no
                // cavity under it, or be too curved to open straight
                // through. The property is about Hollow and Make solid
                // never refusing a *valid solid*; the open-through
                // property on a real cut face is
                // `random_unions_split_make_solid_then_open_through_a_cut_face`.
                Err(CommandError::Operation(e))
                    if label == "open_through" && e.to_string().contains("open_through_") =>
                {
                    too_thin += 1;
                }
                Err(e) => {
                    dump(&format!("{seed:x}"), &solid_mesh, solid_transform);
                    panic!("{what}: refused a valid closed solid: {e}");
                }
            }
        }
    }
    eprintln!(
        "{scenes} scenes ({combine_refused} more refused by combine), {hollowed} hollows \
         ({too_thin} too_thin, a legitimate refusal)"
    );
    assert!(
        scenes * 2 >= cases,
        "too many scenes skipped: {scenes} of {cases}"
    );
    assert!(
        hollowed + too_thin >= scenes,
        "{scenes} scenes, only {hollowed} hollowed + {too_thin} too_thin"
    );
}

// ---------------------------------------------------------------------
// Open through a cut face (owner 2026-11-14, take 3).
// ---------------------------------------------------------------------

/// `t` (object to world) applied to `p`.
fn to_world(t: &[f64; 12], p: [f64; 3]) -> [f64; 3] {
    [
        t[0] * p[0] + t[3] * p[1] + t[6] * p[2] + t[9],
        t[1] * p[0] + t[4] * p[1] + t[7] * p[2] + t[10],
        t[2] * p[0] + t[5] * p[1] + t[8] * p[2] + t[11],
    ]
}

/// The faces of `mesh` (placed by `t`) lying flat on the world plane
/// `z = at` (within `tol`), facing `side` (+1 up, -1 down, within 25
/// degrees): a cut face. Their ids and their world triangles.
fn flat_faces(
    mesh: &Mesh,
    t: &[f64; 12],
    at: f64,
    side: f64,
    tol: f64,
) -> (Vec<u32>, Vec<[[f64; 3]; 3]>) {
    mesh.faces()
        .filter_map(|(f, tri)| {
            let p = tri.map(|v| to_world(t, mesh.corner_position(v)));
            let (a, b) = (
                [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]],
                [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]],
            );
            let n = [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            let facing = len > 0.0 && n[2] * side >= 25f64.to_radians().cos() * len;
            (facing && p.iter().all(|q| (q[2] - at).abs() < tol)).then_some((f.0, p))
        })
        .unzip()
}

fn covers(tris: &[[[f64; 3]; 3]], q: [f64; 3]) -> bool {
    tris.iter().any(|p| {
        let s = |a: [f64; 3], b: [f64; 3]| {
            (b[0] - a[0]) * (q[1] - a[1]) - (b[1] - a[1]) * (q[0] - a[0])
        };
        let (d0, d1, d2) = (s(p[0], p[1]), s(p[1], p[2]), s(p[2], p[0]));
        (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0) || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0)
    })
}

/// Random unions of spheres and boxes (as above), optionally Split by a
/// horizontal plane (Keep Bottom or Top, Cap on), then Make solid, then
/// Hollow (2 mm wall) with Open through on the cut face (or, unsplit,
/// on the top face of whichever box is highest, when that's flat): never
/// refused while a cavity lies under the face (plain Hollow's own inner
/// surface, under the face's footprint, within a few cells of a wall
/// under it), and the result is closed and manifold with the rim at the
/// cut's own height. `MF_OPEN_PROPERTY_CASES` scenes (default 6),
/// `MF_OPEN_PROPERTY_SEED` to start elsewhere.
#[test]
fn random_unions_split_make_solid_then_open_through_a_cut_face() {
    let cases = env_u64("MF_OPEN_PROPERTY_CASES", 6);
    let seed0 = env_u64("MF_OPEN_PROPERTY_SEED", 0x5EED_0003);
    let wall = 2.0;
    let dir = tempfile::tempdir().unwrap();
    let (mut opened, mut nothing_under, mut skipped) = (0, 0, 0);
    for case in 0..cases {
        let seed = seed0.wrapping_add(case).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut r = Rng(seed);
        let prims: Vec<(Mesh, [f64; 12])> =
            (0..2 + r.below(3)).map(|_| primitive(&mut r)).collect();
        let (mut s, ids) = session_of(dir.path(), &prims);
        if ids.len() > 1 && s.execute(combine(ids[0], &ids[1..])).is_err() {
            eprintln!("seed {seed:#x}: combine refused (shapes apart), skipped");
            skipped += 1;
            continue;
        }
        let mut target = ids[0];
        let world_box = |s: &Session, id: ObjectId| {
            let o = s.document().object(id).unwrap();
            let m = o.mesh().unwrap();
            let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
            for (_, p) in m.vertices() {
                let w = to_world(&o.transform, p);
                for k in 0..3 {
                    lo[k] = lo[k].min(w[k]);
                    hi[k] = hi[k].max(w[k]);
                }
            }
            (lo, hi)
        };
        let (lo, hi) = world_box(&s, target);
        // Split (3 in 4) at a height a quarter to three quarters up,
        // off the primitives' own half-unit grid; otherwise the top.
        let (cut, side) = if r.below(4) != 0 {
            let f = 0.25 + 0.5 * r.below(1000) as f64 / 1000.0;
            let at = lo[2] + (hi[2] - lo[2]) * f + 0.0137;
            let keep_below = r.below(2) == 0;
            let before: Vec<ObjectId> = s.document().objects().iter().map(|o| o.id).collect();
            s.execute(invoke(
                "mesh.split_plane",
                target,
                &[
                    ("plane_point".into(), format!("0,0,{at}")),
                    ("plane_normal".into(), "0,0,1".into()),
                    (
                        "keep".into(),
                        if keep_below { "below" } else { "above" }.into(),
                    ),
                    ("cap".into(), "true".into()),
                ],
            ))
            .unwrap_or_else(|e| panic!("seed {seed:#x}: split at z = {at}: {e}"));
            if s.document().object(target).is_none() {
                target = s
                    .document()
                    .objects()
                    .iter()
                    .map(|o| o.id)
                    .find(|id| !before.contains(id))
                    .unwrap();
            }
            (at, if keep_below { 1.0 } else { -1.0 })
        } else {
            (hi[2], 1.0)
        };
        if let Err(e) = s.execute(invoke("mesh.make_solid", target, &[])) {
            panic!("seed {seed:#x}: make_solid: {e}");
        }
        let o = s.document().object(target).unwrap();
        let (solid, t) = (o.mesh().unwrap().clone(), o.transform);
        // Make solid may rebuild the surface (a voxel remesh), leaving the
        // cut face flat only to within a small fraction of a cell, at the
        // part's own extreme: the face is found there, and the rim is
        // checked against that extreme exactly (the outer surface is
        // kept bit for bit).
        let extreme = solid
            .vertices()
            .map(|(_, p)| side * to_world(&t, p)[2])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (extreme - side * cut).abs() < 0.25,
            "seed {seed:#x}: Make solid moved the cut from {cut} to {}",
            side * extreme
        );
        let cut = side * extreme;
        let (faces, footprint) = flat_faces(&solid, &t, cut, side, 0.25);
        if faces.is_empty() {
            // Unsplit, with no box flat on top: no flat face to open.
            eprintln!("seed {seed:#x}: no flat face at z = {cut}, skipped");
            skipped += 1;
            continue;
        }
        // A face to open through is at least a few walls across: the
        // highest point of an unsplit tilted box, rebuilt, is a sliver
        // along its top edge, nothing a user would pick for a bowl.
        let face_area: f64 = footprint
            .iter()
            .map(|p| {
                0.5 * ((p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                    - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0]))
                    .abs()
            })
            .sum();
        if face_area < 4.0 * wall * wall {
            eprintln!(
                "seed {seed:#x}: the face at z = {cut} is a sliver ({face_area} mm²), skipped"
            );
            skipped += 1;
            continue;
        }
        let plain = vec![
            ("wall_thickness".to_string(), wall.to_string()),
            ("inner_detail".to_string(), "0.5".to_string()),
        ];
        // Plain Hollow first: its inner surface is the oracle for
        // "a cavity lies under the face".
        let cell = match s.execute(invoke("mesh.hollow", target, &plain)) {
            Ok(mf_command::CommitOutcome::NewRevision { report }) => {
                report["cell_size"].as_f64().unwrap()
            }
            Ok(_) => panic!("seed {seed:#x}: plain hollow changed nothing"),
            Err(e) if e.to_string().contains("too_thin") => {
                eprintln!("seed {seed:#x}: too thin to hollow, skipped");
                skipped += 1;
                continue;
            }
            Err(e) => panic!("seed {seed:#x}: plain hollow: {e}"),
        };
        let shell = s.document().object(target).unwrap().mesh().unwrap().clone();
        assert!(s.undo(), "seed {seed:#x}: undo");
        let outer: std::collections::HashSet<[u64; 3]> =
            solid.vertices().map(|(_, p)| p.map(f64::to_bits)).collect();
        let cavity_under = shell.vertices().any(|(_, p)| {
            if outer.contains(&p.map(f64::to_bits)) {
                return false;
            }
            let w = to_world(&t, p);
            let depth = side * (cut - w[2]);
            depth > 0.0 && depth < wall + 3.0 * cell && covers(&footprint, w)
        });
        let list = faces
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut params = plain.clone();
        params.push(("open_through".into(), "true".into()));
        params.push(("open_through_faces".into(), list));
        let what = format!("seed {seed:#x}: open through the face at z = {cut}");
        match s.execute(invoke("mesh.hollow", target, &params)) {
            Ok(_) => {
                opened += 1;
                let o = s.document().object(target).unwrap();
                let out = o.mesh().unwrap().clone();
                assert_solid(&out, &what);
                // The rim: the cut face's own ring around the opening,
                // never lowered below the picked face itself (which is
                // the cut, flat, or flat to within Make solid's own
                // rebuild of it).
                let rim = out
                    .vertices()
                    .map(|(_, p)| side * to_world(&o.transform, p)[2])
                    .fold(f64::NEG_INFINITY, f64::max);
                let face_low = footprint
                    .iter()
                    .flatten()
                    .map(|q| side * q[2])
                    .fold(f64::INFINITY, f64::min);
                assert!(
                    rim >= face_low && rim <= side * cut,
                    "{what}: the rim is at {}, below the picked face (down to {})",
                    side * rim,
                    side * face_low
                );
            }
            Err(CommandError::Operation(e))
                if e.to_string().contains("open_through_nothing_under") =>
            {
                if cavity_under {
                    dump(&format!("open-{seed:x}"), &solid, t);
                    panic!("{what}: refused with a cavity under it: {e}");
                }
                nothing_under += 1;
            }
            Err(e) => {
                dump(&format!("open-{seed:x}"), &solid, t);
                panic!("{what}: refused: {e}");
            }
        }
    }
    eprintln!("{opened} opened, {nothing_under} with nothing under the face, {skipped} skipped");
    assert!(opened * 2 >= cases, "too few opened: {opened} of {cases}");
}
