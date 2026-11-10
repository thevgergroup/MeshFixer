#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The scene context (every object's world box): `object.arrange_on_plate`
//! and dowel layout keep clear of every object on the plate, not just the
//! command's operands (D21).

use std::collections::BTreeMap;
use std::path::Path;

use mf_command::{Invocation, ObjectId, Session, Verdict, replay};
use mf_fixtures::{cube, scaled, uv_sphere};
use mf_geometry::Mesh;
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};

fn at(x: f64, y: f64, z: f64) -> [f64; 12] {
    let mut t = IDENTITY;
    t[9..].copy_from_slice(&[x, y, z]);
    t
}

fn session(dir: &Path, objects: Vec<(&str, Mesh, [f64; 12])>) -> (Session, Vec<ObjectId>) {
    let path = dir.join("doc.3mf");
    let model = Model {
        unit: Unit::Millimeter,
        objects: objects
            .into_iter()
            .map(|(n, mesh, transform)| ModelObject {
                name: n.into(),
                mesh,
                transform,
            })
            .collect(),
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, _) = s.import_model(read_model(&path).unwrap());
    (s, ids)
}

fn invocation(op: &str, target: ObjectId, params: &[(&str, &str)]) -> Invocation {
    Invocation {
        op_id: op.into(),
        target,
        selection: None,
        operands: Vec::new(),
        raw_params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
    }
}

/// World footprints and min z of every object.
fn placed(s: &Session) -> Vec<(String, [f64; 4], f64)> {
    s.document()
        .objects()
        .iter()
        .map(|o| {
            let t = o.transform;
            let mut f = [
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ];
            let mut z = f64::INFINITY;
            for (_, p) in o.mesh().unwrap().vertices() {
                let w = [
                    t[0] * p[0] + t[3] * p[1] + t[6] * p[2] + t[9],
                    t[1] * p[0] + t[4] * p[1] + t[7] * p[2] + t[10],
                    t[2] * p[0] + t[5] * p[1] + t[8] * p[2] + t[11],
                ];
                f = [
                    f[0].min(w[0]),
                    f[1].min(w[1]),
                    f[2].max(w[0]),
                    f[3].max(w[1]),
                ];
                z = z.min(w[2]);
            }
            (o.name.clone(), f, z)
        })
        .collect()
}

fn assert_apart(all: &[(String, [f64; 4], f64)]) {
    for (i, (a, f, _)) in all.iter().enumerate() {
        for (b, g, _) in &all[i + 1..] {
            let apart = f[2] <= g[0] || g[2] <= f[0] || f[3] <= g[1] || g[3] <= f[1];
            assert!(apart, "{a} {f:?} overlaps {b} {g:?}");
        }
    }
}

#[test]
fn dowels_avoid_every_object_on_the_plate() {
    let dir = tempfile::tempdir().unwrap();
    // A block sitting exactly where the first dowels would go (the plate's
    // −x −y corner).
    let (mut s, ids) = session(
        dir.path(),
        vec![
            ("scan", scaled(&uv_sphere(48, 96), 30.0), at(0.0, 0.0, 30.0)),
            ("block", cube(40.0), at(-128.0, -128.0, 0.0)),
        ],
    );
    s.execute(invocation(
        "mesh.split_plane",
        ids[0],
        &[("pegs", "dowels"), ("plane_point", "0,0,30.37")],
    ))
    .unwrap();
    let all = placed(&s);
    assert_eq!(all.iter().filter(|o| o.0.contains("dowel")).count(), 2);
    let others: Vec<_> = all
        .iter()
        .filter(|o| o.0.contains("dowel") || o.0 == "block")
        .cloned()
        .collect();
    assert_apart(&others);
}

#[test]
fn arrange_on_plate_moves_an_object_clear_of_the_others_and_replays_exact() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = session(
        dir.path(),
        vec![
            ("a", cube(20.0), at(-128.0, -128.0, 0.0)),
            ("b", cube(20.0), at(-120.0, -120.0, 5.0)),
        ],
    );
    s.execute(invocation("object.arrange_on_plate", ids[1], &[]))
        .unwrap();
    let all = placed(&s);
    assert_apart(&all);
    assert!(all[1].2.abs() < 1e-9, "rests on the plate: {}", all[1].2);
    let log = dir.path().join("log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert!(report.results.iter().all(|(_, v)| *v == Verdict::Exact));
}
