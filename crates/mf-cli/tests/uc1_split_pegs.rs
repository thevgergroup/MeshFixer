#![allow(clippy::unwrap_used)] // tests may panic on failure

//! UC1 acceptance (Phase 1 Task 16a): split a model horizontally,
//! vertically and on a 30° bias, each with dowels (holes 5 and 8 deep)
//! from the "Default" print fit profile, end to end through the CLI;
//! export 3MF; measure the parts and dowels on the meshes; replay Exact
//! with a different active profile.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use mf_geometry::{Mesh, inspect};
use mf_io::threemf::Unit;
use mf_io::{Format, read_model, write_path};

fn meshfixer(config: &Path, args: &[&str]) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_meshfixer"))
        .args(args)
        .env("MESHFIXER_CONFIG_DIR", config)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// A settings folder with "Default" (the built-in values) and "Loose".
fn config(dir: &Path, active: &str) -> PathBuf {
    let config = dir.join(format!("config-{active}"));
    std::fs::create_dir_all(config.join("meshfixer")).unwrap();
    std::fs::write(
        config.join("meshfixer").join("fit-profiles.yaml"),
        format!(
            "active: {active}\nprofiles:\n  - name: Default\n    peg_clearance: 0.2\n    length_allowance: 0.4\n    hinge_gap: 0.4\n    clasp_interference: 0.3\n  - name: Loose\n    peg_clearance: 0.5\n    length_allowance: 1.0\n    hinge_gap: 0.6\n    clasp_interference: 0.2\n"
        ),
    )
    .unwrap();
    config
}

fn world_footprint(m: &Mesh, t: &[f64; 12]) -> [f64; 4] {
    let bb = m.bounding_box().unwrap();
    let mut f = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for c in 0..8 {
        let p = [0, 1, 2].map(|a| {
            if c & (1 << a) != 0 {
                bb.max[a]
            } else {
                bb.min[a]
            }
        });
        let x = t[0] * p[0] + t[3] * p[1] + t[6] * p[2] + t[9];
        let y = t[1] * p[0] + t[4] * p[1] + t[7] * p[2] + t[10];
        f = [f[0].min(x), f[1].min(y), f[2].max(x), f[3].max(y)];
    }
    f
}

fn world_min_z(m: &Mesh, t: &[f64; 12]) -> f64 {
    m.vertices()
        .map(|(_, p)| t[2] * p[0] + t[5] * p[1] + t[8] * p[2] + t[11])
        .fold(f64::INFINITY, f64::min)
}

/// Splits `input` three ways and checks UC1's assertions on each.
/// `centre` is in mm (the plane's point is given in mm and converted);
/// `k` is document units per mm (lengths measured on the meshes are in
/// document units).
fn uc1(input: &Path, dir: &Path, prefix: &[&str], centre: [f64; 3], k: f64) {
    let default = config(dir, "Default");
    let loose = config(dir, "Loose");
    let (s, c) = (0.5f64, 30f64.to_radians().cos());
    let cuts = [
        ("horizontal", [0.0, 0.0, 1.0]),
        ("vertical", [1.0, 0.0, 0.0]),
        ("bias", [s, 0.0, c]),
    ];
    for (name, n) in cuts {
        // Just off the centre, so the cut doesn't run along a ring of the
        // tessellation.
        let p = [
            centre[0] + 0.37 * n[0],
            centre[1] + 0.37 * n[1],
            centre[2] + 0.37 * n[2],
        ];
        let out = dir.join(format!("{name}.3mf"));
        let log = dir.join(format!("{name}.mfx.log"));
        let point = format!("plane_point={},{},{}", p[0], p[1], p[2]);
        let normal = format!("plane_normal={},{},{}", n[0], n[1], n[2]);
        let mut args: Vec<&str> = vec!["run", input.to_str().unwrap()];
        args.extend_from_slice(prefix);
        args.extend_from_slice(&[
            "--op",
            "mesh.split_plane",
            "--param",
            &point,
            "--param",
            &normal,
            "--param",
            "pegs=dowels",
            "--param",
            "hole_depth_above=5",
            "--param",
            "hole_depth_below=8",
            "--fit-profile",
            "Default",
            "--save-log",
            log.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ]);
        meshfixer(&default, &args);
        let model = read_model(&out).unwrap();
        let dowels: Vec<_> = model
            .objects
            .iter()
            .filter(|o| o.name.contains("dowel"))
            .collect();
        assert!(!dowels.is_empty(), "{name}: no dowels");
        assert_eq!(
            model.objects.len(),
            2 + dowels.len(),
            "{name}: two parts and the dowels"
        );
        let mut prints: Vec<([f64; 4], &str)> = Vec::new();
        for o in &model.objects {
            let d = inspect(&o.mesh);
            assert!(
                d.is_closed && d.is_manifold,
                "{name}: {} isn't closed and manifold",
                o.name
            );
            assert_eq!(d.inconsistent_orientation_edges, 0, "{name}: {}", o.name);
            if o.name.contains("dowel") {
                // Upright: its length along z, its axis through its
                // vertices' mean.
                let bb = o.mesh.bounding_box().unwrap();
                assert!(
                    (bb.max[2] - bb.min[2] - k * (5.0 + 8.0 - 0.4)).abs() < k * 1e-6,
                    "{name}: length"
                );
                let n = o.mesh.vertex_count() as f64;
                let c = o
                    .mesh
                    .vertices()
                    .fold([0.0, 0.0], |a, (_, p)| [a[0] + p[0] / n, a[1] + p[1] / n]);
                let r = o
                    .mesh
                    .vertices()
                    .map(|(_, p)| ((p[0] - c[0]).powi(2) + (p[1] - c[1]).powi(2)).sqrt())
                    .fold(0.0, f64::max);
                assert!(
                    (2.0 * r - k * (5.0 - 0.2)).abs() < k * 1e-6,
                    "{name}: diameter {}",
                    2.0 * r
                );
                assert!(
                    world_min_z(&o.mesh, &o.transform).abs() < k * 1e-9,
                    "{name}: on the plate"
                );
                let f = world_footprint(&o.mesh, &o.transform);
                for (g, other) in &prints {
                    let apart = f[2] <= g[0] || g[2] <= f[0] || f[3] <= g[1] || g[3] <= f[1];
                    assert!(apart, "{name}: {} overlaps {other}", o.name);
                }
                prints.push((f, &o.name));
            } else {
                prints.insert(0, (world_footprint(&o.mesh, &o.transform), &o.name));
            }
        }
        // The recorded parameters hold the profile's values.
        let commands = std::fs::read_to_string(log.join("commands.jsonl")).unwrap();
        let recorded = |key: &str| -> f64 {
            let line = commands.lines().last().unwrap();
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            fn find(v: &serde_json::Value, key: &str) -> Option<f64> {
                match v {
                    serde_json::Value::Object(m) => m
                        .get(key)
                        .and_then(|x| {
                            x.as_f64()
                                .or_else(|| x.get("Float").and_then(|f| f.as_f64()))
                        })
                        .or_else(|| m.values().find_map(|x| find(x, key))),
                    serde_json::Value::Array(a) => a.iter().find_map(|x| find(x, key)),
                    _ => None,
                }
            }
            find(&v, key).unwrap_or_else(|| panic!("{key} not recorded: {line}"))
        };
        assert!((recorded("peg_clearance") - k * 0.2).abs() < k * 1e-12);
        assert!((recorded("length_allowance") - k * 0.4).abs() < k * 1e-12);
        // Replay with another profile active: the recorded values win.
        let replayed = meshfixer(&loose, &["replay", log.to_str().unwrap()]);
        let text = String::from_utf8_lossy(&replayed.stdout);
        assert!(
            text.contains("0 equivalent, 0 not verified"),
            "{name}: {text}"
        );
    }
}

#[test]
fn uc1_split_horizontal_vertical_bias_with_dowels() {
    let dir = tempfile::tempdir().unwrap();
    // The procedural 60 mm sphere.
    let input = dir.path().join("sphere.stl");
    let sphere = mf_fixtures::scaled(&mf_fixtures::uv_sphere(96, 192), 30.0);
    write_path(&sphere, &input, Format::StlBinary, Unit::Millimeter).unwrap();
    uc1(&input, dir.path(), &["--units", "mm"], [0.0, 0.0, 0.0], 1.0);
}

/// The same on a real scan (heavy, on demand: `MF_UC1_SCAN` names an OBJ
/// in metres, by default the Archaeopteryx slab in `MF_FIXTURES_DIR`,
/// 495 × 410 × 28 mm). It is closed with `fill_holes`, then `make_solid`
/// at 1 mm: fill_holes leaves six pinched 3–4-edge openings on this scan
/// ("untriangulable"), which Make solid closes.
#[test]
fn uc1_on_the_real_scan() {
    let path = std::env::var_os("MF_UC1_SCAN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("MF_FIXTURES_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(mf_fixtures::real::dir)
                .join("archaeopteryx.obj")
        });
    if !path.is_file() {
        mf_fixtures::skip::fixture_missing("uc1_on_the_real_scan", &path);
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let scan = mf_io::read_path(&path).unwrap().mesh;
    let bb = scan.bounding_box().unwrap();
    uc1(
        &path,
        dir.path(),
        &[
            "--units",
            "m",
            "--op",
            "mesh.fill_holes",
            "--param",
            "max_loop_edges=1000",
            "--op",
            "mesh.make_solid",
            "--param",
            "cell_size=1",
        ],
        bb.center().map(|x| x * 1000.0),
        0.001,
    );
}
