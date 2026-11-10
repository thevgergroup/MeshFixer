#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Strict determinism (MF-CMD-005): same input, parameters and engine give
//! byte-identical output regardless of thread count.

use std::collections::BTreeMap;

use mf_command::{Geometry, Invocation, Session};
use mf_fixtures::{DamageSpec, damaged_sphere, uv_sphere};

fn run_all_ops(threads: usize) -> Vec<(String, String)> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    pool.install(|| {
        let mesh = damaged_sphere(&DamageSpec::for_triangles(80_000, 9));
        let mut out = Vec::new();
        for op in mf_operations::registry() {
            // Operations that need another object have their own
            // determinism through replay (tests/objects.rs).
            let spec = mf_spec::builtin::get(op.id()).unwrap();
            if spec.operands.iter().any(|o| o.required) {
                continue;
            }
            // A split needs a closed model and a plane through it: a bias
            // cut with a gap through a closed sphere.
            let (input, raw_params) = if op.id() == "mesh.split_plane" {
                (
                    uv_sphere(160, 250),
                    BTreeMap::from([
                        ("plane_point".to_string(), "0.01,0.02,0.1".to_string()),
                        ("plane_normal".to_string(), "0.3,0.2,1".to_string()),
                        ("cut_gap".to_string(), "0.05".to_string()),
                    ]),
                )
            } else if op.id() == "object.add_part" {
                (
                    mesh.clone(),
                    BTreeMap::from([("part_id".to_string(), "torus".to_string())]),
                )
            } else {
                (mesh.clone(), BTreeMap::new())
            };
            let mut s = Session::new();
            let id = s.add_geometry(Geometry::Mesh(input), "fixture".into(), None);
            s.execute(Invocation {
                operands: Vec::new(),
                selection: None,
                op_id: op.id().into(),
                target: id,
                raw_params,
            })
            .unwrap();
            // Objects an operation created (a split's bottom part).
            for o in s.document().objects().iter().filter(|o| o.id != id) {
                out.push((
                    format!("{} created {}", op.id(), o.name),
                    o.geometry.content_hash().to_hex(),
                ));
            }
            // An object operation may move or remove the object: its
            // placement is part of the result (and "removed" is one).
            let Some(object) = s.document().object(id) else {
                out.push((op.id().to_owned(), "removed".into()));
                continue;
            };
            let hash = object.geometry.content_hash().to_hex();
            let placement = format!("{:?}", object.transform.map(f64::to_bits));
            // Fresh topology per run so cached results can't mask differences.
            let topo = format!("{:?}", object.mesh().unwrap().topology().boundary_loops);
            out.push((
                op.id().to_owned(),
                format!("{hash}/{placement}/{}", blake3::hash(topo.as_bytes())),
            ));
        }
        out
    })
}

#[test]
fn ops_identical_at_1_and_n_threads() {
    assert_eq!(run_all_ops(1), run_all_ops(8));
}
