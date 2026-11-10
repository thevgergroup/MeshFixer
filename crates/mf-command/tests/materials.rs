#![allow(clippy::unwrap_used)] // tests may panic on failure

//! MF-ISSUE-091: per-face materials survive every operation, through
//! provenance, and replay reproduces them (the hashes include them).

use std::collections::BTreeMap;
use std::path::Path;

use mf_command::{Invocation, ObjectId, Session, Verdict, replay};
use mf_fixtures::{cube, translated, uv_sphere};
use mf_geometry::{FaceId, Material, Mesh};
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};

fn red() -> Material {
    Material {
        name: "PLA red".into(),
        color: [255, 0, 0, 255],
    }
}
fn blue() -> Material {
    Material {
        name: "PLA blue".into(),
        color: [0, 0, 255, 255],
    }
}
fn green() -> Material {
    Material {
        name: "PETG green".into(),
        color: [0, 255, 0, 255],
    }
}

/// Faces with a corner at z > `split` red, the rest blue.
fn two_tone(mesh: &Mesh, split: f64) -> Mesh {
    let faces: Vec<u32> = (0..mesh.face_slots())
        .map(|f| match mesh.face(FaceId(f)) {
            Some(t) if t.iter().any(|&v| mesh.corner_position(v)[2] > split) => 0,
            _ => 1,
        })
        .collect();
    mesh.with_materials(vec![red(), blue()], None, Some(faces))
}

fn one_colour(mesh: &Mesh, m: Material) -> Mesh {
    mesh.with_materials(vec![m], Some(0), None)
}

fn session_with(dir: &Path, objects: Vec<(&str, Mesh)>) -> (Session, Vec<ObjectId>) {
    let path = dir.join("doc.3mf");
    let model = Model {
        unit: Unit::Millimeter,
        objects: objects
            .into_iter()
            .map(|(name, mesh)| ModelObject {
                name: name.into(),
                mesh,
                transform: IDENTITY,
            })
            .collect(),
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, warnings) = s.import_model(read_model(&path).unwrap());
    assert!(warnings.is_empty(), "{warnings:?}");
    (s, ids)
}

fn run(
    s: &mut Session,
    op: &str,
    target: ObjectId,
    operands: Vec<(String, ObjectId, Option<mf_selection::Selection>)>,
    params: &[(&str, &str)],
) {
    s.execute(Invocation {
        op_id: op.into(),
        target,
        selection: None,
        operands,
        raw_params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
    })
    .unwrap();
}

fn mesh_of(s: &Session, id: ObjectId) -> Mesh {
    s.document().object(id).unwrap().mesh().unwrap().clone()
}

fn colour(mesh: &Mesh, f: FaceId) -> Option<Material> {
    mesh.face_material(f)
        .map(|m| mesh.materials()[m as usize].clone())
}

/// Every live face has one of `allowed`.
fn all_coloured(mesh: &Mesh, allowed: &[Material], what: &str) {
    for (f, _) in mesh.faces() {
        let c = colour(mesh, f).unwrap_or_else(|| panic!("{what}: face {f:?} lost its colour"));
        assert!(allowed.contains(&c), "{what}: face {f:?} is {c:?}");
    }
}

fn replays_exact(s: &Session, dir: &Path) {
    let log = dir.join("s.mfx.log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
    for o in s.document().objects() {
        assert_eq!(
            report
                .session
                .document()
                .object(o.id)
                .unwrap()
                .mesh()
                .unwrap()
                .content_hash(),
            o.mesh().unwrap().content_hash()
        );
    }
}

#[test]
fn materials_change_the_hash() {
    let plain = uv_sphere(8, 16);
    let a = two_tone(&plain, 0.0);
    let b = two_tone(&plain, 0.5);
    assert_ne!(plain.content_hash(), a.content_hash());
    assert_ne!(a.content_hash(), b.content_hash());
    assert_ne!(a.structure_hash(), b.structure_hash());
}

#[test]
fn split_keeps_colours_and_replays() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = session_with(
        dir.path(),
        vec![("ball", two_tone(&uv_sphere(24, 48), 0.0))],
    );
    run(
        &mut s,
        "mesh.split_plane",
        ids[0],
        vec![],
        &[("plane_point", "0,0,0.3")],
    );
    let top = mesh_of(&s, ids[0]);
    let bottom = mesh_of(&s, s.document().objects()[1].id);
    // Above z = 0.3 everything was red, cut pieces and cap included.
    all_coloured(&top, &[red()], "top");
    // Below: the cut band is red, the rest blue; the cap's boundary faces
    // are red, so the cap is too.
    all_coloured(&bottom, &[red(), blue()], "bottom");
    assert!(
        bottom
            .faces()
            .any(|(f, _)| colour(&bottom, f) == Some(blue()))
    );
    replays_exact(&s, dir.path());
}

#[test]
fn boolean_union_keeps_each_parts_material() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = session_with(
        dir.path(),
        vec![
            ("body", one_colour(&cube(2.0), red())),
            (
                "knob",
                one_colour(&translated(&cube(1.0), [1.5, 0.5, 0.5]), green()),
            ),
        ],
    );
    run(
        &mut s,
        "mesh.boolean",
        ids[0],
        vec![("tool".into(), ids[1], None)],
        &[("operation", "union")],
    );
    let joined = mesh_of(&s, ids[0]);
    all_coloured(&joined, &[red(), green()], "union");
    // Faces beyond the body (x > 2) came from the knob: green.
    for (f, t) in joined.faces() {
        if t.iter().all(|&v| joined.corner_position(v)[0] > 2.0 + 1e-9) {
            assert_eq!(colour(&joined, f), Some(green()), "{f:?}");
        }
        if t.iter().all(|&v| joined.corner_position(v)[0] < 1.5 - 1e-9) {
            assert_eq!(colour(&joined, f), Some(red()), "{f:?}");
        }
    }
    replays_exact(&s, dir.path());
}

#[test]
fn sculpt_with_detail_keeps_colours() {
    let dir = tempfile::tempdir().unwrap();
    let sphere = uv_sphere(32, 64);
    let (mut s, ids) = session_with(dir.path(), vec![("ball", two_tone(&sphere, 0.0))]);
    // Dabs across the colour boundary at z = 0 on the +x side.
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
    let samples: Vec<String> = [[1.0, 0.0, -0.1], [1.0, 0.1, 0.0], [1.0, 0.2, 0.1]]
        .iter()
        .map(|&q| {
            format!(
                "{},0.3333333333333333,0.3333333333333333,0.3333333333333334,1",
                near(q).0
            )
        })
        .collect();
    run(
        &mut s,
        "mesh.sculpt_stroke",
        ids[0],
        vec![],
        &[
            ("radius", "0.3"),
            ("view_dir", "-1,0,0"),
            ("detail_mode", "adaptive"),
            ("detail", "0.1"),
            ("samples", &samples.join(";")),
        ],
    );
    let after = mesh_of(&s, ids[0]);
    assert!(
        after.face_count() > sphere.face_count(),
        "detail made faces"
    );
    all_coloured(&after, &[red(), blue()], "sculpt");
    replays_exact(&s, dir.path());
}

#[test]
fn hole_fill_takes_the_majority_of_its_neighbours() {
    let dir = tempfile::tempdir().unwrap();
    // A hole at the north pole of a sphere whose top is red: every face
    // around the hole is red.
    let sphere = uv_sphere(24, 48);
    let mut e = sphere.edit();
    for (f, t) in sphere.faces() {
        if t.iter().all(|&v| sphere.corner_position(v)[2] > 0.97) {
            e.remove_face(f).unwrap();
        }
    }
    let holed = two_tone(&e.finish().unwrap().0, 0.5);
    let (mut s, ids) = session_with(dir.path(), vec![("ball", holed)]);
    run(&mut s, "mesh.fill_holes", ids[0], vec![], &[]);
    let filled = mesh_of(&s, ids[0]);
    assert!(filled.topology().boundary_loops.is_empty());
    all_coloured(&filled, &[red(), blue()], "fill");
    for (f, t) in filled.faces() {
        if t.iter().all(|&v| filled.corner_position(v)[2] > 0.9) {
            assert_eq!(colour(&filled, f), Some(red()));
        }
    }
    replays_exact(&s, dir.path());
}

#[test]
fn make_solid_inherits_from_the_nearest_face() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = session_with(
        dir.path(),
        vec![("ball", two_tone(&uv_sphere(16, 32), 0.0))],
    );
    run(
        &mut s,
        "mesh.make_solid",
        ids[0],
        vec![],
        &[("cell_size", "0.1")],
    );
    let solid = mesh_of(&s, ids[0]);
    all_coloured(&solid, &[red(), blue()], "make_solid");
    for (f, t) in solid.faces() {
        let z: f64 = t.iter().map(|&v| solid.corner_position(v)[2]).sum::<f64>() / 3.0;
        if z > 0.3 {
            assert_eq!(colour(&solid, f), Some(red()));
        } else if z < -0.3 {
            assert_eq!(colour(&solid, f), Some(blue()));
        }
    }
    replays_exact(&s, dir.path());
}
