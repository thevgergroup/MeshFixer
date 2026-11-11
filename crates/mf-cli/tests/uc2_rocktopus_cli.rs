#![allow(clippy::unwrap_used)] // tests may panic on failure

//! UC2 "Rocktopus" through the CLI (Phase 1 Task 24a): `mesh.split_plane`
//! and `mesh.boolean` with `blend_width` run end to end through the
//! `meshfixer` binary (not just the command layer directly). The full
//! octopus-cut-delete-import-place pipeline needs several objects and a
//! dynamically created object (the split's bottom piece) in between
//! steps, which `meshfixer run` (one target object per invocation) can't
//! express in a single call; this test instead drives the CLI in two
//! calls — a split, then a blended union against a second object already
//! in the file — covering both of this task's new operations through the
//! binary. The full multi-step scenario (split, delete, import, place,
//! union+blend, sculpt) is covered end to end at the command layer in
//! `mf-command/tests/uc2_rocktopus.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use mf_fixtures::scenario::{head, octopus};
use mf_geometry::inspect;
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};

fn meshfixer(args: &[&str]) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_meshfixer"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// A split-off octopus body (its neck cap already on), through the
/// command layer, written to a fresh 3MF for the CLI steps.
fn split_body(dir: &Path) -> PathBuf {
    let in_path = dir.join("octopus.3mf");
    write_model_path(
        &Model {
            unit: Unit::Millimeter,
            objects: vec![ModelObject {
                name: "octopus".into(),
                mesh: octopus(1),
                transform: IDENTITY,
            }],
        },
        &in_path,
    )
    .unwrap();
    let out_path = dir.join("body.3mf");
    meshfixer(&[
        "run",
        in_path.to_str().unwrap(),
        "--op",
        "mesh.split_plane",
        "--param",
        "plane_point=0,0,0.37",
        "--param",
        "plane_normal=0,0,1",
        "--param",
        "keep=above",
        "-o",
        out_path.to_str().unwrap(),
    ]);
    out_path
}

#[test]
fn split_plane_and_blended_union_run_through_the_cli() {
    let dir = tempfile::tempdir().unwrap();
    let body_path = split_body(dir.path());

    // Build one input file with the split body (object 0) and the new
    // head (object 1, already placed onto the neck), for a single `run`
    // whose target is the body and whose tool operand is the head.
    let body_model = read_model(&body_path).unwrap();
    let body_mesh = body_model.objects[0].mesh.clone();
    let mut head_transform = IDENTITY;
    // Scale 1.3, rotate 25 deg about z, move onto the neck: the same
    // placement `mf-command/tests/uc2_rocktopus.rs` uses.
    let (c, s) = (25.0_f64.to_radians().cos(), 25.0_f64.to_radians().sin());
    let scale = 1.3;
    head_transform[0] = scale * c;
    head_transform[1] = scale * s;
    head_transform[3] = -scale * s;
    head_transform[4] = scale * c;
    head_transform[8] = scale;
    head_transform[11] = 8.0;
    let combined_path = dir.path().join("combined.3mf");
    write_model_path(
        &Model {
            unit: Unit::Millimeter,
            objects: vec![
                ModelObject {
                    name: "body".into(),
                    mesh: body_mesh,
                    transform: IDENTITY,
                },
                ModelObject {
                    name: "head".into(),
                    mesh: head(1),
                    transform: head_transform,
                },
            ],
        },
        &combined_path,
    )
    .unwrap();

    let out_path = dir.path().join("joined.3mf");
    let log_dir = dir.path().join("joined.mfx.log");
    meshfixer(&[
        "run",
        combined_path.to_str().unwrap(),
        "--object",
        "0",
        "--op",
        "mesh.boolean",
        "--operand",
        "tool=1",
        "--param",
        "operation=union",
        "--param",
        "blend_width=3",
        "--save-log",
        log_dir.to_str().unwrap(),
        "-o",
        out_path.to_str().unwrap(),
    ]);

    let model = read_model(&out_path).unwrap();
    assert_eq!(model.objects.len(), 1, "the head is consumed into the body");
    let report = inspect(&model.objects[0].mesh);
    assert_eq!(report.boundary_edges, 0, "closed: {report:?}");
    assert_eq!(
        report.non_manifold_edges + report.non_manifold_vertices,
        0,
        "manifold: {report:?}"
    );

    // The saved log replays.
    let replay_out = meshfixer(&["replay", log_dir.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&replay_out.stdout);
    assert!(
        stdout.contains("Exact") || stdout.lines().count() > 0,
        "{stdout}"
    );
}
