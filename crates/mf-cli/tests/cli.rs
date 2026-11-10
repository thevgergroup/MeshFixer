#![allow(clippy::unwrap_used)] // tests may panic on failure

use std::path::Path;
use std::process::{Command, Output};

use mf_fixtures::{cube, merge, tetrahedron, translated};
use mf_io::{Format, write_path};

fn meshfixer(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_meshfixer"))
        .args(args)
        .output()
        .unwrap()
}

fn damaged(dir: &Path) -> String {
    let p = dir.join("damaged.stl");
    let m = merge(&[cube(1.0), translated(&tetrahedron(), [5.0, 0.0, 0.0])]);
    write_path(&m, &p, Format::StlBinary, mf_io::threemf::Unit::Millimeter).unwrap();
    p.to_str().unwrap().to_owned()
}

#[test]
fn inspect_json_reports_components() {
    let dir = tempfile::tempdir().unwrap();
    let out = meshfixer(&["inspect", &damaged(dir.path()), "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["components"], 2);
}

#[test]
fn inspect_human_readable() {
    let dir = tempfile::tempdir().unwrap();
    let out = meshfixer(&["inspect", &damaged(dir.path())]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("components"));
}

#[test]
fn remove_small_writes_output() {
    let dir = tempfile::tempdir().unwrap();
    let input = damaged(dir.path());
    let output = dir.path().join("fixed.obj");
    let out = meshfixer(&[
        "remove-small-components",
        &input,
        "-o",
        output.to_str().unwrap(),
        "--max-triangles",
        "5",
        "--max-relative-size",
        "100",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let fixed = mf_io::read_path(&output).unwrap();
    assert_eq!(mf_geometry::inspect(&fixed.mesh).components, 1);
}

#[test]
fn help_lists_spec_params() {
    let out = meshfixer(&["remove-small-components", "--help"]);
    let text = String::from_utf8_lossy(&out.stdout);
    let spec = mf_spec::builtin::get("mesh.remove_small_components").unwrap();
    for p in &spec.parameters {
        assert!(
            text.contains(&p.description),
            "missing '{}' in:\n{text}",
            p.description
        );
    }
}

#[test]
fn bad_param_exit_code_2() {
    let dir = tempfile::tempdir().unwrap();
    let input = damaged(dir.path());
    let out = meshfixer(&[
        "remove-small-components",
        &input,
        "-o",
        "x.stl",
        "--max-triangles",
        "0",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("max_triangles"));
}

#[test]
fn missing_file_exit_code_1() {
    let out = meshfixer(&["inspect", "/nonexistent/file.stl"]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn render_writes_png() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("out.png");
    let out = meshfixer(&[
        "render",
        &damaged(dir.path()),
        "-o",
        png.to_str().unwrap(),
        "--width",
        "80",
        "--height",
        "60",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("no GPU adapter") {
        mf_fixtures::skip::gpu_unavailable("render_writes_png");
        return;
    }
    assert!(out.status.success(), "{stderr}");
    let bytes = std::fs::read(&png).unwrap();
    assert_eq!(&bytes[1..4], b"PNG");
}

#[test]
fn gen_fixture_damaged() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("d.stl");
    let out = meshfixer(&[
        "gen-fixture",
        "damaged",
        p.to_str().unwrap(),
        "--triangles",
        "20000",
        "--seed",
        "3",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let d = mf_geometry::inspect(&mf_io::read_path(&p).unwrap().mesh);
    assert!(d.components > 1 && !d.boundary_loops.is_empty());
}

/// The demo scene (scripts/demo.sh): two objects at their own transforms,
/// written as one 3MF.
#[test]
fn gen_fixture_scene_writes_two_transformed_objects() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("scene.3mf");
    let out = meshfixer(&["gen-fixture", "scene", p.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let model = mf_io::read_model(&p).unwrap();
    let names: Vec<_> = model.objects.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(names, ["scan", "plate"]);
    assert!(
        model
            .objects
            .iter()
            .all(|o| o.transform != mf_io::threemf::IDENTITY)
    );

    // The scene is 3MF only: it has more than one object.
    let stl = dir.path().join("scene.stl");
    let out = meshfixer(&["gen-fixture", "scene", stl.to_str().unwrap()]);
    assert!(!out.status.success());
}

#[test]
fn metrics_json_written() {
    let dir = tempfile::tempdir().unwrap();
    let metrics = dir.path().join("m.json");
    let out = meshfixer(&[
        "inspect",
        &damaged(dir.path()),
        "--json",
        "--metrics",
        metrics.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&metrics).unwrap()).unwrap();
    let phases: Vec<&str> = v["phases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(phases, ["import", "topology"]);
    assert!(v["phases"][0]["peak_bytes"].as_u64().unwrap() > 0);
    assert!(v["structures"]["mesh"].as_u64().unwrap() > 0);
    assert!(v["structures"]["topology"].as_u64().unwrap() > 0);
    assert!(v["peak_heap"].as_u64().unwrap() > 0);
}

#[test]
fn run_saves_log_and_replay_verifies_it() {
    let dir = tempfile::tempdir().unwrap();
    let input = damaged(dir.path());
    let log = dir.path().join("s.mfx.log");
    let out_path = dir.path().join("out.stl");
    let out = meshfixer(&[
        "run",
        &input,
        "--op",
        "mesh.remove_small_components",
        "--param",
        "max_triangles=5",
        "--param",
        "max_relative_size=100",
        "--op",
        "mesh.remove_small_components",
        "--param",
        "max_triangles=2",
        "--param",
        "keep_largest=true",
        "--save-log",
        log.to_str().unwrap(),
        "-o",
        out_path.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out_path.exists());
    let replayed = meshfixer(&["replay", log.to_str().unwrap()]);
    assert!(
        replayed.status.success(),
        "{}",
        String::from_utf8_lossy(&replayed.stderr)
    );
    let text = String::from_utf8_lossy(&replayed.stdout);
    assert!(text.contains("Exact"), "{text}");
    assert!(
        text.contains("verdicts: 1 exact, 0 equivalent, 0 not verified"),
        "{text}"
    );

    // Tampering makes replay fail with exit code 1.
    let commands = log.join("commands.jsonl");
    let t = std::fs::read_to_string(&commands).unwrap();
    std::fs::write(
        &commands,
        t.replace("\"max_triangles\":5", "\"max_triangles\":3"),
    )
    .unwrap();
    let tampered = meshfixer(&["replay", log.to_str().unwrap()]);
    assert_eq!(
        tampered.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&tampered.stdout)
    );
}

/// MF-ISSUE-046: replay counts Exact and Equivalent verdicts separately
/// (the cross-OS CI job prints this line per OS). The fixture's log was
/// recorded with jittered positions, so every command is Equivalent.
#[test]
fn replay_counts_exact_and_equivalent_separately() {
    let log = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../mf-command/tests/data/equivalent-replay/session.mfx.log");
    let r = meshfixer(&["replay", log.to_str().unwrap()]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let text = String::from_utf8_lossy(&r.stdout);
    assert!(text.contains("command 1: Equivalent"), "{text}");
    assert!(
        text.contains("verdicts: 0 exact, 2 equivalent, 0 not verified"),
        "{text}"
    );
}

#[test]
fn run_rejects_param_before_any_op() {
    let dir = tempfile::tempdir().unwrap();
    let out = meshfixer(&[
        "run",
        &damaged(dir.path()),
        "--param",
        "max_triangles=5",
        "-o",
        "x.stl",
    ]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn select_arg_parses_and_reaches_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("three.stl");
    let m = merge(&[
        cube(1.0),
        translated(&tetrahedron(), [5.0, 0.0, 0.0]),
        translated(&tetrahedron(), [9.0, 0.0, 0.0]),
    ]);
    write_path(
        &m,
        &input,
        Format::StlBinary,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();
    let out = dir.path().join("out.stl");
    let log = dir.path().join("s.mfx.log");
    let r = meshfixer(&[
        "run",
        input.to_str().unwrap(),
        "--op",
        "mesh.remove_small_components",
        "--param",
        "max_triangles=5",
        "--select",
        "component:1",
        "--save-log",
        log.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let after = mf_io::read_path(&out).unwrap().mesh;
    assert_eq!(
        after.topology().components.len(),
        2,
        "only component 1 removed"
    );
    let commands = std::fs::read_to_string(log.join("commands.jsonl")).unwrap();
    assert!(commands.contains("\"kind\":\"component\""), "{commands}");

    // The single-operation subcommand accepts it too.
    let out2 = dir.path().join("out2.stl");
    let r = meshfixer(&[
        "remove-small-components",
        input.to_str().unwrap(),
        "-o",
        out2.to_str().unwrap(),
        "--max-triangles",
        "5",
        "--select",
        "component:2",
    ]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    assert_eq!(
        mf_io::read_path(&out2)
            .unwrap()
            .mesh
            .topology()
            .components
            .len(),
        2
    );

    // Malformed selections are usage errors.
    let bad = meshfixer(&[
        "remove-small-components",
        input.to_str().unwrap(),
        "-o",
        "x.stl",
        "--select",
        "bogus",
    ]);
    assert_eq!(bad.status.code(), Some(2));
}

#[test]
fn run_on_3mf_keeps_every_object() {
    use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.3mf");
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![
            ModelObject {
                name: "a".into(),
                mesh: merge(&[cube(1.0), translated(&tetrahedron(), [5.0, 0.0, 0.0])]),
                transform: IDENTITY,
            },
            ModelObject {
                name: "b".into(),
                mesh: tetrahedron(),
                transform: IDENTITY,
            },
        ],
    };
    mf_io::write_model_path(&model, &input).unwrap();
    let out = dir.path().join("out.3mf");
    let r = meshfixer(&[
        "remove-small-components",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--max-triangles",
        "5",
        "--max-relative-size",
        "100",
    ]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let back = mf_io::read_model(&out).unwrap();
    assert_eq!(back.objects.len(), 2, "every object is written back");
    assert_eq!(
        back.objects[0].mesh.triangle_count(),
        12,
        "the first object was repaired"
    );
    assert_eq!(
        back.objects[1].mesh.triangle_count(),
        4,
        "other objects are untouched"
    );
    let inspect = meshfixer(&["inspect", input.to_str().unwrap(), "--json"]);
    assert!(
        inspect.status.success(),
        "{}",
        String::from_utf8_lossy(&inspect.stderr)
    );
}

/// MF-ISSUE-035 (fix round 1): `run`'s STL/OBJ export applies the exported
/// object's transform, not just the desktop's. The demo scene's "scan"
/// object is centred at the origin before its transform moves it to
/// [-32, 0, 20] (`mf_fixtures::demo_scene`); exporting it to STL after an
/// operation must write it at that position, not back at the origin, and
/// must say so on stderr.
#[test]
fn run_applies_the_objects_transform_to_an_stl_export() {
    let dir = tempfile::tempdir().unwrap();
    let scene = dir.path().join("scene.3mf");
    let created = meshfixer(&["gen-fixture", "scene", scene.to_str().unwrap()]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let out = dir.path().join("out.stl");
    let r = meshfixer(&[
        "run",
        scene.to_str().unwrap(),
        "--op",
        "mesh.compact",
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    assert!(
        String::from_utf8_lossy(&r.stderr).contains("position applied"),
        "{}",
        String::from_utf8_lossy(&r.stderr)
    );

    let exported = mf_io::read_path(&out).unwrap().mesh;
    let bbox = exported.bounding_box().unwrap();
    let center = bbox.center();
    // The "scan" object's own geometry is centred near the origin; its
    // transform moves it to translation [-32, 0, 20] (demo_scene). A
    // bounding-box centre still near the origin would mean the transform
    // was never applied.
    assert!(
        (center[0] - (-32.0)).abs() < 5.0 && (center[2] - 20.0).abs() < 5.0,
        "exported bbox centre {center:?} doesn't reflect the object's transform"
    );
}

/// As above, but with a mirrored transform (negative determinant): winding
/// must flip so the exported mesh's normals stay outward.
#[test]
fn run_applies_a_mirrored_transform_and_keeps_normals_outward() {
    use mf_io::threemf::{Model, ModelObject, Unit};
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("mirrored.3mf");
    // Mirror in x, plus a translation: a position check (not just winding)
    // fails if the transform was never applied at all.
    let mirror: [f64; 12] = [
        -1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 100.0, 0.0, 0.0,
    ];
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![ModelObject {
            name: "box".into(),
            mesh: cube(1.0),
            transform: mirror,
        }],
    };
    mf_io::write_model_path(&model, &input).unwrap();

    let out = dir.path().join("out.stl");
    let r = meshfixer(&[
        "run",
        input.to_str().unwrap(),
        "--op",
        "mesh.compact",
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));

    let exported = mf_io::read_path(&out).unwrap().mesh;
    // Untransformed cube(1.0) spans x in [0, 1]; mirrored then translated
    // by 100, it spans x in [99, 100] — nowhere near [0, 1].
    let bbox = exported.bounding_box().unwrap();
    assert!(
        bbox.min[0] > 90.0 && bbox.max[0] < 110.0,
        "the mirrored transform's translation wasn't applied: bbox {bbox:?}"
    );
    let volume = mf_geometry::inspect(&exported).signed_volume.unwrap();
    assert!(
        volume > 0.0,
        "mirrored object's winding wasn't flipped on export: signed volume {volume}"
    );
}

/// A one-object 3MF (a unit cube translated by +100 in x), run through
/// `mesh.compact` with a saved log: the log replays an object that has a
/// transform.
fn translated_cube_log(dir: &Path) -> std::path::PathBuf {
    use mf_io::threemf::{Model, ModelObject, Unit};
    let input = dir.join("moved.3mf");
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![ModelObject {
            name: "box".into(),
            mesh: cube(1.0),
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 100.0, 0.0, 0.0],
        }],
    };
    mf_io::write_model_path(&model, &input).unwrap();
    let log = dir.join("moved.mfx.log");
    let r = meshfixer(&[
        "run",
        input.to_str().unwrap(),
        "--op",
        "mesh.compact",
        "--save-log",
        log.to_str().unwrap(),
        "-o",
        dir.join("run.3mf").to_str().unwrap(),
    ]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    log
}

/// `replay -o out.stl` writes the object where its transform puts it.
#[test]
fn replay_writes_an_stl_with_the_objects_transform_applied() {
    let dir = tempfile::tempdir().unwrap();
    let log = translated_cube_log(dir.path());
    let out = dir.path().join("out.stl");
    let r = meshfixer(&["replay", log.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    assert!(
        String::from_utf8_lossy(&r.stderr).contains("position applied"),
        "{}",
        String::from_utf8_lossy(&r.stderr)
    );
    let bbox = mf_io::read_path(&out).unwrap().mesh.bounding_box().unwrap();
    assert!(
        bbox.min[0] > 99.0 && bbox.max[0] < 102.0,
        "the transform wasn't applied: {bbox:?}"
    );
}

/// `replay -o out.3mf` keeps the object's transform as the object's
/// transform, with its geometry in object space: the transform is
/// neither dropped nor applied twice.
#[test]
fn replay_writes_a_threemf_that_keeps_the_transform_once() {
    let dir = tempfile::tempdir().unwrap();
    let log = translated_cube_log(dir.path());
    let out = dir.path().join("out.3mf");
    let r = meshfixer(&["replay", log.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let model = mf_io::read_model(&out).unwrap();
    assert_eq!(model.objects.len(), 1);
    let object = &model.objects[0];
    assert_eq!(
        object.transform[9..],
        [100.0, 0.0, 0.0],
        "{:?}",
        object.transform
    );
    let bbox = object.mesh.bounding_box().unwrap();
    assert!(
        bbox.min[0] > -0.5 && bbox.max[0] < 1.5,
        "object-space geometry was moved (transform applied twice): {bbox:?}"
    );
}

/// `replay -o out.stl` of a log with several objects refuses by name
/// (STL/OBJ hold one mesh) and suggests .3mf, instead of writing the first
/// object and dropping the rest; nothing is written.
#[test]
fn replay_refuses_to_write_several_objects_to_one_stl() {
    use mf_io::threemf::{Model, ModelObject, Unit};
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("pair.3mf");
    let at = |x: f64| [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, x, 0.0, 0.0];
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![
            ModelObject {
                name: "left".into(),
                mesh: cube(1.0),
                transform: at(0.0),
            },
            ModelObject {
                name: "right".into(),
                mesh: cube(1.0),
                transform: at(10.0),
            },
        ],
    };
    mf_io::write_model_path(&model, &input).unwrap();
    let log = dir.path().join("pair.mfx.log");
    let r = meshfixer(&[
        "run",
        input.to_str().unwrap(),
        "--op",
        "mesh.compact",
        "--save-log",
        log.to_str().unwrap(),
        "-o",
        dir.path().join("run.3mf").to_str().unwrap(),
    ]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));

    let out = dir.path().join("out.stl");
    let r = meshfixer(&["replay", log.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert!(
        !r.status.success(),
        "{}",
        String::from_utf8_lossy(&r.stdout)
    );
    let err = String::from_utf8_lossy(&r.stderr);
    assert!(
        err.contains("'left'") && err.contains("'right'") && err.contains(".3mf"),
        "{err}"
    );
    assert!(!out.exists(), "nothing is written");

    // The same log to .3mf keeps both.
    let out = dir.path().join("out.3mf");
    let r = meshfixer(&["replay", log.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    assert_eq!(mf_io::read_model(&out).unwrap().objects.len(), 2);
}

/// Triangular holes (3-edge loops) as (id, centroid), by id.
fn holes(path: &str) -> Vec<(u64, Vec<f64>)> {
    let out = meshfixer(&["inspect", path, "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut h: Vec<(u64, Vec<f64>)> = v["boundary_loops"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| l["edge_count"] == 3)
        .map(|l| {
            let c = l["centroid"].as_array().unwrap();
            (
                l["id"].as_u64().unwrap(),
                c.iter().map(|x| x.as_f64().unwrap()).collect(),
            )
        })
        .collect();
    h.sort_by_key(|(id, _)| *id);
    h
}

#[test]
fn select_ids_in_later_steps_refer_to_the_imported_mesh() {
    // A grid with three triangular holes. Filling the first renumbers the
    // loops after it, so the second step's IDs must be translated from the
    // imported revision, not read in the current one.
    let dir = tempfile::tempdir().unwrap();
    let n = 8;
    let cell = |i: u32, j: u32| 2 * (j * n + i);
    let mut e = mf_fixtures::grid(n).edit();
    for (i, j) in [(1, 1), (4, 1), (4, 5)] {
        e.remove_face(mf_geometry::FaceId(cell(i, j))).unwrap();
    }
    let input = dir.path().join("holes.obj");
    write_path(
        &e.finish().unwrap().0,
        &input,
        Format::Obj,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();
    let input = input.to_str().unwrap();
    let before = holes(input);
    assert_eq!(before.len(), 3);
    let out_path = dir.path().join("out.obj");
    let out = meshfixer(&[
        "run",
        input,
        "--op",
        "mesh.fill_holes",
        "--select",
        &format!("loop:{}", before[0].0),
        "--op",
        "mesh.fill_holes",
        "--select",
        &format!("loop:{}", before[1].0),
        "-o",
        out_path.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = holes(out_path.to_str().unwrap());
    assert_eq!(after.len(), 1, "{after:?}");
    assert_eq!(
        after[0].1, before[2].1,
        "the third hole is the one left open"
    );
}

/// A damaged sphere big enough that import and fill take a while (binary
/// STL, so import goes through both "reading" and "welding").
fn big_damaged(dir: &Path, triangles: usize) -> String {
    let p = dir.join("big.stl");
    let m = mf_fixtures::damaged_sphere(&mf_fixtures::DamageSpec::for_triangles(triangles, 3));
    write_path(&m, &p, Format::StlBinary, mf_io::threemf::Unit::Millimeter).unwrap();
    p.to_str().unwrap().to_owned()
}

/// `progress <stage> <fraction>` lines from stderr, in order.
fn progress_lines(stderr: &str) -> Vec<(String, f64)> {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("progress "))
        .map(|rest| {
            let (stage, f) = rest.rsplit_once(' ').unwrap();
            assert_eq!(f.len(), 5, "three decimals: {rest:?}");
            (stage.to_owned(), f.parse::<f64>().unwrap())
        })
        .collect()
}

/// MF-RT-003: `--progress` reports `progress <stage> <0.000–1.000>` on
/// stderr, at most 10 lines a second, never going backwards within a stage,
/// and the last line is 1.000.
#[test]
fn progress_lines_are_monotonic_and_end_at_one() {
    let dir = tempfile::tempdir().unwrap();
    let input = big_damaged(dir.path(), 1_000_000);
    let output = dir.path().join("filled.stl");
    let start = std::time::Instant::now();
    let out = meshfixer(&[
        "--progress",
        "fill-holes",
        &input,
        "-o",
        output.to_str().unwrap(),
    ]);
    let elapsed = start.elapsed().as_secs_f64();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let lines = progress_lines(&stderr);
    assert!(lines.len() >= 3, "too few progress lines: {stderr}");
    for (stage, f) in &lines {
        assert!((0.0..=1.0).contains(f), "{stage} {f}");
        assert!(!stage.is_empty() && !stage.contains(' '), "{stage:?}");
    }
    for pair in lines.windows(2) {
        let ((a, fa), (b, fb)) = (&pair[0], &pair[1]);
        if a == b {
            assert!(fb >= fa, "{a} went backwards: {fa} then {fb}");
        }
    }
    assert_eq!(lines.last().unwrap().1, 1.0, "{stderr}");
    // At most 10 a second (the first may come at once).
    let allowed = (elapsed * 10.0).floor() as usize + 1;
    assert!(
        lines.len() <= allowed,
        "{} lines in {elapsed:.2} s",
        lines.len()
    );
    // Import, the fill and the export each have their own stages.
    let mut stages: Vec<&str> = lines.iter().map(|(s, _)| s.as_str()).collect();
    stages.dedup();
    assert!(stages.len() >= 3, "{stages:?}");
    assert_eq!(stages.last(), Some(&mf_io::WRITING), "{stages:?}");
    // Without --progress, nothing of the sort is printed.
    let quiet = meshfixer(&["fill-holes", &input, "-o", output.to_str().unwrap()]);
    assert!(quiet.status.success());
    assert!(progress_lines(&String::from_utf8_lossy(&quiet.stderr)).is_empty());
}

/// MF-RT-004: `--threads N` sizes the job pool, and `--metrics` says how
/// many threads ran, out of how many cores, and why.
#[test]
fn threads_flag_is_reported_in_metrics() {
    let dir = tempfile::tempdir().unwrap();
    let input = damaged(dir.path());
    let output = dir.path().join("fixed.stl");
    let metrics = dir.path().join("m.json");
    let out = meshfixer(&[
        "--threads",
        "3",
        "--metrics",
        metrics.to_str().unwrap(),
        "fill-holes",
        &input,
        "-o",
        output.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&metrics).unwrap()).unwrap();
    assert_eq!(v["threads"]["requested"], 3, "{v}");
    assert_eq!(v["threads"]["threads"], 3, "{v}");
    assert!(v["threads"]["cores"].as_u64().unwrap() >= 1, "{v}");
    assert!(v["threads"]["reason"].as_str().is_some(), "{v}");

    // Without the flag the default leaves one core free.
    let out = meshfixer(&[
        "--metrics",
        metrics.to_str().unwrap(),
        "fill-holes",
        &input,
        "-o",
        output.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&metrics).unwrap()).unwrap();
    let cores = v["threads"]["cores"].as_u64().unwrap();
    assert_eq!(v["threads"]["requested"], serde_json::Value::Null, "{v}");
    assert_eq!(
        v["threads"]["threads"],
        cores.saturating_sub(1).max(1),
        "{v}"
    );
}

/// MF-RT-002 from the CLI: Ctrl-C cancels the running job, exits 130 with
/// "cancelled", and writes no output file (not even a temp file).
#[cfg(unix)]
#[test]
fn sigint_cancels_and_writes_no_output() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let dir = tempfile::tempdir().unwrap();
    let input = big_damaged(dir.path(), 1_000_000);
    let output = dir.path().join("filled.stl");
    let mut child = Command::new(env!("CARGO_BIN_EXE_meshfixer"))
        .args([
            "--progress",
            "fill-holes",
            &input,
            "-o",
            output.to_str().unwrap(),
        ])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    // Wait for the first progress line: the job is running.
    let mut seen = String::new();
    loop {
        let mut line = String::new();
        assert!(
            stderr.read_line(&mut line).unwrap() > 0,
            "exited before any progress: {seen}"
        );
        seen.push_str(&line);
        if line.starts_with("progress ") {
            break;
        }
    }
    let kill = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(kill.success());
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut stderr, &mut rest).unwrap();
    seen.push_str(&rest);
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(130), "{seen}");
    assert!(seen.lines().any(|l| l == "cancelled"), "{seen}");
    assert!(!output.exists(), "an output file was written");
    let left: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, ["big.stl"], "temp files left behind: {left:?}");
}

/// `--units` says what a unitless file's numbers measure (MF-ISSUE-045):
/// the document records it and 3MF output writes it.
#[test]
fn units_flag_sets_the_unit_written_to_3mf() {
    let dir = tempfile::tempdir().unwrap();
    let input = damaged(dir.path());
    let out = dir.path().join("out.3mf");
    let r = meshfixer(&[
        "remove-small-components",
        &input,
        "-o",
        out.to_str().unwrap(),
        "--units",
        "m",
    ]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(!stderr.contains("no units"), "{stderr}");
    assert_eq!(
        mf_io::read_model(&out).unwrap().unit,
        mf_io::threemf::Unit::Meter
    );
}

/// Without `--units`, a unitless input is read as millimetres, and the
/// warning says so and how to change it.
#[test]
fn unitless_input_without_units_is_read_as_millimetres_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let input = damaged(dir.path());
    let out = dir.path().join("out.3mf");
    let r = meshfixer(&[
        "remove-small-components",
        &input,
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(
        stderr.contains("warning:")
            && stderr.contains("no units")
            && stderr.contains("millimetres")
            && stderr.contains("--units"),
        "{stderr}"
    );
    assert_eq!(
        mf_io::read_model(&out).unwrap().unit,
        mf_io::threemf::Unit::Millimeter
    );
}

/// A 3MF says its own unit: `--units` on one is a usage error, not a
/// silent override.
#[test]
fn units_flag_on_a_3mf_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.3mf");
    mf_io::write_model_path(
        &mf_io::threemf::Model {
            unit: mf_io::threemf::Unit::Millimeter,
            objects: vec![mf_io::threemf::ModelObject {
                name: "a".into(),
                mesh: cube(1.0),
                transform: mf_io::threemf::IDENTITY,
            }],
        },
        &input,
    )
    .unwrap();
    let r = meshfixer(&["inspect", input.to_str().unwrap(), "--units", "cm"]);
    assert_eq!(
        r.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&r.stderr)
    );
    assert!(String::from_utf8_lossy(&r.stderr).contains("--units"));
}

/// Two objects in a 3MF, for the object operations.
fn two_objects(dir: &Path) -> std::path::PathBuf {
    use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
    let input = dir.join("two.3mf");
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![
            ModelObject {
                name: "a".into(),
                mesh: cube(1.0),
                transform: IDENTITY,
            },
            ModelObject {
                name: "b".into(),
                mesh: tetrahedron(),
                transform: IDENTITY,
            },
        ],
    };
    mf_io::write_model_path(&model, &input).unwrap();
    input
}

/// MF-OBJ-001, D7: `run --op object.set_transform --param transform=…`
/// then `object.drop_to_plate` writes the placement to the 3MF bit for
/// bit, leaves the other object alone, and the saved log replays exactly.
#[test]
fn run_sets_a_transform_and_drops_to_the_plate() {
    let dir = tempfile::tempdir().unwrap();
    let input = two_objects(dir.path());
    let out = dir.path().join("out.3mf");
    let log = dir.path().join("s.mfx.log");
    // 90 degrees about x (world z = y + 5), then dropped: z translation 0.
    let r = meshfixer(&[
        "run",
        input.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--op",
        "object.set_transform",
        "--param",
        "transform=1,0,0,0,0,1,0,-1,0,0.25,-7.5,5",
        "--op",
        "object.drop_to_plate",
        "--save-log",
        log.to_str().unwrap(),
    ]);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let back = mf_io::read_model(&out).unwrap();
    assert_eq!(back.objects.len(), 2);
    let want = [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, -1.0, 0.0, 0.25, -7.5, 0.0,
    ];
    assert_eq!(
        back.objects[0].transform.map(f64::to_bits),
        want.map(f64::to_bits)
    );
    assert_eq!(back.objects[1].transform, mf_io::threemf::IDENTITY);
    let replayed = meshfixer(&["replay", log.to_str().unwrap()]);
    let text = String::from_utf8_lossy(&replayed.stdout);
    assert!(replayed.status.success(), "{text}");
    assert!(text.contains("verdicts: 2 exact"), "{text}");
}

/// `--operand name=<index>` is checked against the operation's spec and
/// the input's objects before anything is written.
#[test]
fn run_operand_mistakes_are_usage_errors() {
    let dir = tempfile::tempdir().unwrap();
    let input = two_objects(dir.path());
    let out = dir.path().join("out.3mf");
    for operand in ["tool=1", "tool", "tool=x"] {
        let r = meshfixer(&[
            "run",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--op",
            "object.drop_to_plate",
            "--operand",
            operand,
        ]);
        assert_eq!(
            r.status.code(),
            Some(2),
            "{operand}: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        assert!(!out.exists());
    }
}

/// Review I3: no operation writes over its input, however the output path
/// is spelled; the input is left as it was.
#[test]
fn an_output_that_is_the_input_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let input = two_objects(dir.path());
    let before = std::fs::read(&input).unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let spelled = sub.join("..").join("two.3mf");
    for output in [input.to_str().unwrap(), spelled.to_str().unwrap()] {
        for args in [
            vec!["delete", input.to_str().unwrap(), "-o", output],
            vec![
                "run",
                input.to_str().unwrap(),
                "-o",
                output,
                "--op",
                "object.drop_to_plate",
            ],
        ] {
            let r = meshfixer(&args);
            assert_eq!(
                r.status.code(),
                Some(2),
                "{args:?}: {}",
                String::from_utf8_lossy(&r.stderr)
            );
            assert!(String::from_utf8_lossy(&r.stderr).contains("is the input file"));
        }
    }
    assert_eq!(std::fs::read(&input).unwrap(), before, "input untouched");
}

/// Review I3: `delete --object` picks the object by position or name; the
/// 3MF keeps the others. Deleting the last object refuses to write an
/// empty document, by name.
#[test]
fn delete_chooses_its_object_and_never_writes_an_empty_document() {
    let dir = tempfile::tempdir().unwrap();
    let input = two_objects(dir.path());
    for (choice, kept) in [("1", "a"), ("a", "b")] {
        let out = dir.path().join(format!("out-{choice}.3mf"));
        let r = meshfixer(&[
            "delete",
            input.to_str().unwrap(),
            "--object",
            choice,
            "-o",
            out.to_str().unwrap(),
        ]);
        assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
        let back = mf_io::read_model(&out).unwrap();
        let names: Vec<&str> = back.objects.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec![kept]);
    }
    for bad in ["2", "nope"] {
        let r = meshfixer(&[
            "delete",
            input.to_str().unwrap(),
            "--object",
            bad,
            "-o",
            dir.path().join("x.3mf").to_str().unwrap(),
        ]);
        assert_eq!(r.status.code(), Some(2), "{bad}");
    }
    // A one-object file: deleting it leaves nothing to write.
    let single = dir.path().join("single.stl");
    write_path(
        &cube(1.0),
        &single,
        Format::StlBinary,
        mf_io::threemf::Unit::Millimeter,
    )
    .unwrap();
    let out = dir.path().join("empty.3mf");
    let r = meshfixer(&[
        "delete",
        single.to_str().unwrap(),
        "--units",
        "mm",
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(r.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(stderr.contains("every object was deleted"), "{stderr}");
    assert!(!out.exists());
}
