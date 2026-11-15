#![allow(clippy::unwrap_used)] // tests may panic on failure

//! `mesh.combine` under random small transforms (C20, ADR-0011): shapes
//! placed where their surfaces (nearly) coincide — boxes face to face, a
//! box sunk into a sphere, and the owner's demo case of a hemisphere and
//! a half box capped on the same plane — then nudged by tiny
//! translations and rotations, from a fraction of an ulp's worth up to a
//! millimetre. Every Combine either commits a valid closed solid, or is
//! refused by name (a code the copy table words, like
//! `window_degenerate`): never a broken mesh, never an unnamed failure.
//!
//! Runs `MF_COMBINE_PROPERTY_CASES` scenes (default 24), from
//! `MF_COMBINE_PROPERTY_SEED`.

use std::collections::BTreeMap;
use std::path::Path;

use mf_command::{CommandError, Invocation, ObjectId, Session};
use mf_fixtures::{cube, rotation_translation, scaled, uv_sphere};
use mf_geometry::Mesh;
use mf_io::threemf::{Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};
use mf_operations::{OpError, boolean_refusal_copy};
use mf_runtime::RunContext;

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
    /// A small signed offset: zero, or ±10^-k for k in 1..=12.
    fn tiny(&mut self) -> f64 {
        if self.below(4) == 0 {
            return 0.0;
        }
        let sign = if self.below(2) == 0 { 1.0 } else { -1.0 };
        sign * 10f64.powi(-(1 + self.below(12) as i32))
    }
}

/// `transform` with a small random rotation about a random axis
/// (sometimes none), then moved by `t` plus a small random offset.
fn jittered(r: &mut Rng, t: [f64; 3]) -> [f64; 12] {
    let t = t.map(|x| x + r.tiny());
    let degrees = if r.below(3) == 0 {
        0.0
    } else {
        r.tiny().abs() * 10.0
    };
    rotation_translation(r.below(3) as usize, degrees, t)
}

fn invoke(op_id: &str, target: ObjectId, params: &[(&str, &str)]) -> Invocation {
    Invocation {
        op_id: op_id.into(),
        target,
        selection: None,
        operands: Vec::new(),
        raw_params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
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

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// One scene: the shapes, and whether both are first split (Keep
/// Bottom, capped) on the plane z = 0 — the owner's demo.
fn scene(r: &mut Rng) -> (Vec<(Mesh, [f64; 12])>, bool) {
    let sphere = scaled(
        &uv_sphere(12 + 2 * r.below(12) as u32, 24 + 4 * r.below(12) as u32),
        10.0,
    );
    match r.below(3) {
        // Boxes face to face (x = 10 is the first one's face).
        0 => (
            vec![
                (cube(10.0), jittered(r, [0.0; 3])),
                (cube(10.0), jittered(r, [10.0, 0.0, 0.0])),
            ],
            false,
        ),
        // A box sunk into a sphere, its top face on the sphere's equator.
        1 => (
            vec![
                (sphere, jittered(r, [0.0; 3])),
                (cube(10.0), jittered(r, [5.0, -5.0, -10.0])),
            ],
            false,
        ),
        // The owner's demo: both split on z = 0, so both are capped on
        // the same plane, then combined.
        _ => (
            vec![
                (
                    sphere,
                    [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
                ),
                (cube(10.0), jittered(r, [5.0, -5.0, -5.0])),
            ],
            true,
        ),
    }
}

#[test]
fn combine_under_small_transforms_is_valid_or_refused_by_name() {
    let cases = env_u64("MF_COMBINE_PROPERTY_CASES", 24);
    let seed0 = env_u64("MF_COMBINE_PROPERTY_SEED", 0x5EED_0C20);
    let dir = tempfile::tempdir().unwrap();
    let (mut joined, mut refused) = (0, BTreeMap::<&str, usize>::new());
    for case in 0..cases {
        let seed = seed0.wrapping_add(case).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut r = Rng(seed);
        let (prims, split) = scene(&mut r);
        let (mut s, mut ids) = session_of(dir.path(), &prims);
        if split {
            for (k, id) in ids.clone().into_iter().enumerate() {
                s.execute(invoke(
                    "mesh.split_plane",
                    id,
                    &[
                        ("plane_point", "0,0,0"),
                        ("plane_normal", "0,0,1"),
                        ("keep", "below"),
                        ("cap", "true"),
                    ],
                ))
                .unwrap_or_else(|e| panic!("seed {seed:#x}: split p{k}: {e}"));
                let name = format!("p{k} (bottom)");
                ids[k] = s
                    .document()
                    .objects()
                    .iter()
                    .find(|o| o.name == name)
                    .unwrap_or_else(|| panic!("seed {seed:#x}: no {name}"))
                    .id;
            }
        }
        match s.execute(combine(ids[0], &ids[1..])) {
            Ok(_) => {
                let mesh = s.document().object(ids[0]).unwrap().mesh().unwrap();
                let (p, t) = mesh.to_arrays();
                mf_boolean::validate_solid(&p, &t, &RunContext::none()).unwrap_or_else(|e| {
                    panic!("seed {seed:#x}: combine committed an invalid solid: {e}")
                });
                joined += 1;
            }
            Err(CommandError::Operation(OpError::Failed { code, message })) => {
                assert!(
                    boolean_refusal_copy(code).is_some(),
                    "seed {seed:#x}: refused without a named reason: {code}: {message}"
                );
                *refused.entry(code).or_default() += 1;
            }
            Err(CommandError::Operation(OpError::Refused { code, args })) => {
                assert!(
                    boolean_refusal_copy(code).is_some(),
                    "seed {seed:#x}: refused without a named reason: {code}: {:?}",
                    args
                );
                *refused.entry(code).or_default() += 1;
            }
            Err(e) => panic!("seed {seed:#x}: combine failed unnamed: {e}"),
        }
    }
    eprintln!("{joined} joined, refused by name: {refused:?}");
}
