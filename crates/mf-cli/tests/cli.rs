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
    write_path(&m, &p, Format::StlBinary).unwrap();
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
    write_path(&m, &input, Format::StlBinary).unwrap();
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
