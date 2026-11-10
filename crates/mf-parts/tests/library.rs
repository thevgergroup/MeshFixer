#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The parts library, surface frames, placement and `object.add_part`
//! (Phase 1 Task 21, MF-MODEL-004, D16).

use std::collections::BTreeMap;

use mf_command::{Invocation, Session, Verdict, replay};
use mf_geometry::{Bvh, FaceId, Mesh};
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_parts::{Attach, Library, PartKind, PartManifest, SurfaceFrame, place, save_object_as_part};
use mf_runtime::RunContext;

fn ctx() -> RunContext {
    RunContext::none()
}

#[test]
fn builtins_are_closed_and_deterministic() {
    let (lib, warnings) = Library::load(None, &ctx());
    assert!(warnings.is_empty());
    assert_eq!(lib.parts.len(), 8);
    for part in &lib.parts {
        let id = &part.manifest.id;
        let mesh = part.mesh(&ctx()).unwrap();
        let again = part.mesh(&ctx()).unwrap();
        assert_eq!(mesh.content_hash(), again.content_hash(), "{id}");
        assert_eq!(part.content_hash().unwrap(), part.content_hash().unwrap());
        let topo = mesh.topology();
        match part.manifest.kind {
            PartKind::Solid => {
                let bvh = Bvh::build(&mesh, &ctx()).unwrap();
                let all: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
                mf_geometry::validate_solid(&mesh, &all, &bvh, &ctx())
                    .unwrap()
                    .unwrap_or_else(|d| panic!("{id}: {d}"));
            }
            PartKind::Open => {
                assert_eq!(topo.boundary_loops.len(), 1, "{id}");
                assert!(topo.non_manifold_edges.is_empty() && topo.inconsistent_edges == 0);
            }
        }
        // Each sits on z = 0, growing along +z (its attach normal).
        let bb = mesh.bounding_box().unwrap();
        assert!(bb.min[2].abs() < 1e-9 && bb.max[2] > 1.0, "{id}: {bb:?}");
        assert_eq!(part.manifest.attach.normal, [0.0, 0.0, 1.0]);
    }
    // Hashes tell parts apart.
    let hashes: std::collections::BTreeSet<String> = lib
        .parts
        .iter()
        .map(|p| p.content_hash().unwrap())
        .collect();
    assert_eq!(hashes.len(), lib.parts.len());
}

fn manifest(id: &str) -> PartManifest {
    PartManifest {
        id: id.into(),
        name: "Little cube".into(),
        version: 1,
        licence: "CC0-1.0".into(),
        source: "test".into(),
        kind: PartKind::Solid,
        attach: Attach {
            point: [0.5, 0.5, 0.0],
            normal: [0.0, 0.0, 1.0],
            up: [0.0, 1.0, 0.0],
        },
        default_scale: 1.0,
        mesh: String::new(),
    }
}

#[test]
fn malformed_user_part_is_a_named_warning() {
    let dir = tempfile::tempdir().unwrap();
    let cube = mf_fixtures::cube(1.0);
    let saved = save_object_as_part(&cube, &manifest("little-cube"), dir.path()).unwrap();
    assert!(saved.join("part.yaml").is_file());
    // Saving the same id again is refused.
    assert!(save_object_as_part(&cube, &manifest("little-cube"), dir.path()).is_err());
    // A malformed manifest, one with a missing mesh, and a duplicate id.
    std::fs::create_dir_all(dir.path().join("broken")).unwrap();
    std::fs::write(dir.path().join("broken/part.yaml"), "id: [oops\n").unwrap();
    std::fs::create_dir_all(dir.path().join("no-mesh")).unwrap();
    let mut m = manifest("no-mesh");
    m.mesh = "gone.stl".into();
    std::fs::write(
        dir.path().join("no-mesh/part.yaml"),
        serde_saphyr::to_string(&m).unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("zz-dup")).unwrap();
    std::fs::copy(saved.join("part.yaml"), dir.path().join("zz-dup/part.yaml")).unwrap();
    std::fs::copy(saved.join("part.3mf"), dir.path().join("zz-dup/part.3mf")).unwrap();

    let (lib, warnings) = Library::load(Some(dir.path()), &ctx());
    let names: Vec<String> = warnings
        .iter()
        .map(|w| w.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["broken", "no-mesh", "zz-dup"], "{warnings:?}");
    assert!(warnings[0].reason.contains("malformed"), "{warnings:?}");
    assert!(warnings[1].reason.contains("missing"), "{warnings:?}");
    assert!(warnings[2].reason.contains("already"), "{warnings:?}");
    let part = lib.get("little-cube").unwrap();
    assert!(!part.is_builtin());
    assert_eq!(part.mesh(&ctx()).unwrap().face_count(), 12);
}

#[test]
fn place_aligns_attach_normal_with_surface_normal() {
    let attach = manifest("x").attach;
    let s = std::f64::consts::FRAC_1_SQRT_2;
    let frames = [
        ([1.0, 2.0, 3.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
        ([0.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]),
        ([5.0, -1.0, 2.0], [s, 0.0, s], [0.0, 1.0, 0.0]),
    ];
    for (point, normal, tangent) in frames {
        let frame = SurfaceFrame {
            point,
            normal,
            tangent,
        };
        for (scale, spin) in [(1.0, 0.0), (2.5, 30.0), (0.5, -90.0)] {
            let m = place(&attach, &frame, scale, spin);
            let apply = |p: [f64; 3], lin: bool| -> [f64; 3] {
                std::array::from_fn(|c| {
                    m[c] * p[0]
                        + m[3 + c] * p[1]
                        + m[6 + c] * p[2]
                        + if lin { 0.0 } else { m[9 + c] }
                })
            };
            let at = apply(attach.point, false);
            let n = apply(attach.normal, true);
            for k in 0..3 {
                assert!((at[k] - point[k]).abs() < 1e-12, "{at:?} {point:?}");
                assert!((n[k] - scale * normal[k]).abs() < 1e-12, "{n:?} {normal:?}");
            }
            // The up direction is the tangent turned by the spin.
            let up = apply(attach.up, true);
            let cos = (0..3).map(|k| up[k] * tangent[k]).sum::<f64>() / scale;
            assert!((cos - spin.to_radians().cos()).abs() < 1e-12, "{cos}");
        }
    }
}

#[test]
fn surface_frame_is_stable_on_a_noisy_scan() {
    let sphere = mf_fixtures::uv_sphere(96, 192);
    // 1% radial noise, deterministic.
    let mut seed = 0x9e37_79b9_u64;
    let mut next = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    let (p, t) = sphere.to_arrays();
    let p: Vec<[f64; 3]> = p
        .iter()
        .map(|v| {
            let k = 1.0 + 0.01 * (2.0 * next() - 1.0);
            v.map(|c| c * k)
        })
        .collect();
    let noisy = Mesh::new(p, t).unwrap();
    let bvh = Bvh::build(&noisy, &ctx()).unwrap();
    let mut worst = 0.0_f64;
    for f in (0..noisy.face_slots()).step_by(997) {
        let face = FaceId(f);
        let frame = SurfaceFrame::at_indexed(&noisy, &bvh, &IDENTITY, face, [1.0 / 3.0; 3], 0.15);
        let exact = frame
            .point
            .map(|c| c / frame.point.iter().map(|x| x * x).sum::<f64>().sqrt());
        let cos: f64 = (0..3).map(|k| exact[k] * frame.normal[k]).sum();
        worst = worst.max(cos.clamp(-1.0, 1.0).acos().to_degrees());
        // The brute-force frame agrees.
        let slow = SurfaceFrame::at(&noisy, &IDENTITY, face, [1.0 / 3.0; 3], 0.15);
        assert_eq!(slow, frame);
    }
    eprintln!("worst normal error under 1% noise: {worst:.2}°");
    assert!(worst < 5.0, "{worst}°");
}

#[test]
fn add_part_replays_exact_and_verifies_part_hash() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doc.3mf");
    let model = Model {
        unit: Unit::Millimeter,
        objects: vec![ModelObject {
            name: "base".into(),
            mesh: mf_fixtures::cube(20.0),
            transform: IDENTITY,
        }],
    };
    mf_io::write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, _) = s.import_model(mf_io::read_model(&path).unwrap());
    let (lib, _) = Library::load(None, &ctx());
    let sphere = lib.get("sphere").unwrap();
    // On top of the cube, at its centre.
    let base = s.document().object(ids[0]).unwrap().mesh().unwrap().clone();
    let top = base
        .faces()
        .find(|(_, t)| t.iter().all(|&v| base.corner_position(v)[2] == 20.0))
        .unwrap()
        .0;
    let frame = SurfaceFrame::at(&base, &IDENTITY, top, [1.0 / 3.0; 3], 1.0);
    let m = place(&sphere.manifest.attach, &frame, 1.0, 0.0);
    let csv: Vec<String> = m.iter().map(f64::to_string).collect();
    s.execute(Invocation {
        op_id: "object.add_part".into(),
        target: ids[0],
        selection: None,
        operands: Vec::new(),
        raw_params: BTreeMap::from([
            ("part_id".to_string(), "sphere".to_string()),
            ("part_hash".to_string(), sphere.content_hash().unwrap()),
            ("transform".to_string(), csv.join(",")),
        ]),
    })
    .unwrap();
    let objects = s.document().objects();
    assert_eq!(objects.len(), 2);
    assert_eq!(objects[1].name, "Sphere");
    assert_eq!(objects[1].transform, m);

    let log = dir.path().join("s.mfx.log");
    s.save_log(&log).unwrap();
    let report = replay(&log).unwrap();
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report.results
    );
    // A different recorded hash: the part "changed", so replay can't verify.
    let commands = log.join("commands.jsonl");
    let text = std::fs::read_to_string(&commands).unwrap();
    std::fs::write(
        &commands,
        text.replace(&sphere.content_hash().unwrap(), "00ff"),
    )
    .unwrap();
    let verified = replay(&log).map(|r| r.all_verified()).unwrap_or(false);
    assert!(!verified, "a changed part hash must not replay as verified");
}
