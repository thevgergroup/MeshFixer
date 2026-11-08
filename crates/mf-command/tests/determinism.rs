#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Strict determinism (MF-CMD-005): same input, parameters and engine give
//! byte-identical output regardless of thread count.

use std::collections::BTreeMap;

use mf_command::{Geometry, Invocation, Session};
use mf_fixtures::{DamageSpec, damaged_sphere};

fn run_all_ops(threads: usize) -> Vec<(String, String)> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    pool.install(|| {
        let mesh = damaged_sphere(&DamageSpec::for_triangles(80_000, 9));
        let mut out = Vec::new();
        for op in mf_operations::registry() {
            let mut s = Session::new();
            let id = s.add_geometry(Geometry::Mesh(mesh.clone()), "fixture".into(), None);
            s.execute(Invocation {
                selection: None,
                op_id: op.id().into(),
                target: id,
                raw_params: BTreeMap::new(),
            })
            .unwrap();
            let hash = s
                .document()
                .object(id)
                .unwrap()
                .geometry
                .content_hash()
                .to_hex();
            // Fresh topology per run so cached results can't mask differences.
            let topo = format!(
                "{:?}",
                s.document()
                    .object(id)
                    .unwrap()
                    .mesh()
                    .unwrap()
                    .topology()
                    .boundary_loops
            );
            out.push((
                op.id().to_owned(),
                format!("{hash}/{}", blake3::hash(topo.as_bytes())),
            ));
        }
        out
    })
}

#[test]
fn ops_identical_at_1_and_n_threads() {
    assert_eq!(run_all_ops(1), run_all_ops(8));
}
