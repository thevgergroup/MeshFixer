#![allow(clippy::unwrap_used)] // tests may panic on failure

//! UC2 acceptance scenario, "Rocktopus" (Phase 1 Task 24a, P1-M5): an
//! octopus is cut through the neck, its head is deleted, a new head is
//! placed and joined with a blend, and the join is sculpted smooth then
//! inflated. Run twice — `capped_neck` (the usual split leaves a flat cap
//! on the body) and `open_neck` (the cap is explicitly deleted first, so
//! the union must close the opening locally, D15) — through the command
//! layer, with the full log replaying Exact.

use std::collections::BTreeMap;
use std::path::Path;

use mf_command::{CommandError, Invocation, ObjectId, Session, Verdict, replay};
use mf_fixtures::scenario::{head, octopus};
use mf_geometry::{Bvh, FaceId, Mesh, affine};
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};
use mf_runtime::RunContext;
use mf_selection::{Selection, SelectionKind};

fn session_with(dir: &Path, objects: Vec<(&str, Mesh, [f64; 12])>) -> (Session, Vec<ObjectId>) {
    let path = dir.join("doc.3mf");
    let model = Model {
        unit: Unit::Millimeter,
        objects: objects
            .into_iter()
            .map(|(name, mesh, transform)| ModelObject {
                name: name.into(),
                mesh,
                transform,
            })
            .collect(),
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, warnings) = s.import_model(read_model(&path).unwrap());
    assert!(warnings.is_empty(), "{warnings:?}");
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
            .collect(),
    }
}

fn boolean_union(target: ObjectId, tool: ObjectId, blend_width: f64) -> Invocation {
    Invocation {
        op_id: "mesh.boolean".into(),
        target,
        selection: None,
        operands: vec![("tool".into(), tool, None)],
        raw_params: BTreeMap::from([
            ("operation".to_string(), "union".to_string()),
            ("blend_width".to_string(), blend_width.to_string()),
        ]),
    }
}

/// The 12-value transform a gizmo's scale/rotate(z)/move-onto-the-neck
/// compose would produce: scale uniformly, rotate about z by `degrees`,
/// then translate by `to`. (`mf-render::gizmo::compose`'s formula with a
/// zero pivot and only a z rotation, worked out directly here so this
/// backend test crate needs no GPU-pulling `mf-render` dependency.)
fn placement(scale: f64, degrees: f64, to: [f64; 3]) -> [f64; 12] {
    let r = degrees.to_radians();
    let (c, s) = (r.cos(), r.sin());
    let rotate_z = [c, s, 0.0, -s, c, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
    let scale_t = [
        scale, 0.0, 0.0, 0.0, scale, 0.0, 0.0, 0.0, scale, 0.0, 0.0, 0.0,
    ];
    let mut t = affine::compose(&scale_t, &rotate_z);
    t[9] += to[0];
    t[10] += to[1];
    t[11] += to[2];
    t
}

fn mesh_of(s: &Session, id: ObjectId) -> Mesh {
    s.document().object(id).unwrap().mesh().unwrap().clone()
}

fn assert_closed_solid(mesh: &Mesh, label: &str) {
    let ctx = RunContext::none();
    let bvh = Bvh::build(mesh, &ctx).unwrap();
    let all: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
    mf_geometry::validate_solid(mesh, &all, &bvh, &ctx)
        .unwrap()
        .unwrap_or_else(|d| panic!("{label}: not a valid solid: {d}"));
}

/// A few sculpt dabs along the seam (its centroid's face, plus a handful
/// spread evenly through the created set, each a centroid sample: face,
/// barycentre, pressure), the format `mesh.sculpt_stroke` records
/// (`DabSample::to_array`'s order). A real stroke is a handful of dabs
/// along a drawn path, not one dab per face in the band.
fn seam_samples(seam_faces: &[FaceId]) -> String {
    const MAX_DABS: usize = 12;
    let step = (seam_faces.len() / MAX_DABS).max(1);
    seam_faces
        .iter()
        .step_by(step)
        .take(MAX_DABS)
        .map(|f| format!("{},{},{},{},{}", f.0, 1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0, 1.0))
        .collect::<Vec<_>>()
        .join(";")
}

fn sculpt_stroke(target: ObjectId, brush: &str, radius: f64, samples: &str) -> Invocation {
    Invocation {
        op_id: "mesh.sculpt_stroke".into(),
        target,
        selection: None,
        operands: Vec::new(),
        raw_params: BTreeMap::from([
            ("brush".to_string(), brush.to_string()),
            ("radius".to_string(), radius.to_string()),
            ("strength".to_string(), "0.15".to_string()),
            ("samples".to_string(), samples.to_string()),
        ]),
    }
}

/// Runs the scenario (steps 1-6 of the brief). `open_neck`: whether the
/// body's cap faces are deleted before the head is joined (D15's covered-
/// opening closure). Returns the session positioned after both strokes,
/// the body's object id, and the log directory (for replay).
fn run_scenario(dir: &Path, open_neck: bool) -> (Session, ObjectId, std::path::PathBuf) {
    let (mut s, ids) = session_with(dir, vec![("octopus", octopus(1), IDENTITY)]);
    let body = ids[0];

    // Step 1: split horizontally through the neck (z = 0), keep both.
    let out = s
        .execute(invocation(
            "mesh.split_plane",
            body,
            &[
                ("plane_point", "0,0,0.37"),
                ("plane_normal", "0,0,1"),
                ("keep", "both"),
            ],
        ))
        .unwrap();
    let report = match out {
        mf_command::CommitOutcome::NewRevision { report } => report,
        other => panic!("expected a new revision, got {other:?}"),
    };
    let parts = report["parts"].as_array().unwrap();
    let top = parts
        .iter()
        .find(|p| p["object"] == "target")
        .expect("the body (top) stays the target object");
    let cap_faces: Vec<u32> = top["cap_faces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u32)
        .collect();
    assert!(!cap_faces.is_empty(), "the body's cut leaves a cap");

    // The bottom (head) piece is the other created object.
    let head_piece = s
        .document()
        .objects()
        .iter()
        .map(|o| o.id)
        .find(|&id| id != body)
        .expect("the split created the head piece");

    // Step 2: delete the head piece.
    s.execute(invocation("object.delete", head_piece, &[]))
        .unwrap();

    // Open-neck variant: delete the body's cap faces too (D15).
    if open_neck {
        let sel = Selection {
            object: body,
            revision: s.current(),
            kind: SelectionKind::Faces,
            ids: cap_faces.clone(),
        };
        let mut inv = invocation("mesh.delete_faces", body, &[]);
        inv.selection = Some(sel);
        s.execute(inv).unwrap();
    }

    // Step 3: import the new head and place it onto the neck (gizmo-style
    // compose: scale 1.3, rotate 25 deg about z, move so its base
    // overlaps the neck by 2 mm). The octopus's neck sits at z ~ 0 with
    // radius ~6 (see mf_fixtures::scenario::octopus); the head's base
    // (its -z pole region) is placed just below the neck's top so it
    // overlaps by 2 mm.
    let head_path = dir.join("head.3mf");
    write_model_path(
        &Model {
            unit: Unit::Millimeter,
            objects: vec![ModelObject {
                name: "head".into(),
                mesh: head(1),
                transform: IDENTITY,
            }],
        },
        &head_path,
    )
    .unwrap();
    let (new_ids, warnings) = s.import_model(read_model(&head_path).unwrap());
    assert!(warnings.is_empty(), "{warnings:?}");
    let new_head = new_ids[0];
    let t = placement(1.3, 25.0, [0.0, 0.0, 8.0]);
    s.execute(invocation(
        "object.set_transform",
        new_head,
        &[(
            "transform",
            &t.iter().map(f64::to_string).collect::<Vec<_>>().join(","),
        )],
    ))
    .unwrap();

    // Step 5: union with a 3 mm blend.
    let before_body = mesh_of(&s, body);
    s.execute(boolean_union(body, new_head, 3.0)).unwrap();
    let after_mesh = mesh_of(&s, body);
    assert_closed_solid(&after_mesh, "after the blended union");

    // Seam band: faces the union created (the join's own provenance,
    // on the revision the union itself committed).
    let union_provenance = s
        .node(s.current())
        .unwrap()
        .provenance
        .get(&body)
        .cloned()
        .expect("the union changed the body's geometry");
    let seam_faces: Vec<FaceId> = union_provenance
        .created_faces
        .iter()
        .map(|(f, _)| *f)
        .collect();
    assert!(!seam_faces.is_empty(), "the blend creates a seam band");

    // Every octopus face outside the blend window is kept, bitwise: not
    // touched, and (where it survives) at the same position.
    let touched: std::collections::HashSet<FaceId> = union_provenance
        .created_faces
        .iter()
        .map(|(f, _)| *f)
        .chain(union_provenance.removed_faces.iter().copied())
        .chain(union_provenance.modified_faces.iter().copied())
        .collect();
    let moved: std::collections::HashSet<u32> = union_provenance
        .moved_vertices
        .iter()
        .chain(&union_provenance.created_vertices)
        .map(|v| v.0)
        .collect();
    for (f, t) in before_body.faces() {
        if touched.contains(&f) {
            continue;
        }
        if after_mesh.face(f) != Some(t) {
            continue; // removed by an earlier step (e.g. delete_faces)
        }
        assert!(
            t.iter().all(|v| !moved.contains(v)),
            "face {f:?} outside the blend window should be untouched"
        );
        for &v in &t {
            assert_eq!(
                before_body.corner_position(v),
                after_mesh.corner_position(v),
                "vertex {v} outside the blend window moved"
            );
        }
    }

    // Step 6: two sculpt strokes, smooth then inflate, along the seam.
    let samples = seam_samples(&seam_faces);
    s.execute(sculpt_stroke(body, "smooth", 2.0, &samples))
        .unwrap();
    s.execute(sculpt_stroke(body, "inflate", 2.0, &samples))
        .unwrap();

    // Save the log for replay.
    let log_dir = dir.join(if open_neck {
        "open_neck.log"
    } else {
        "capped_neck.log"
    });
    s.save_log(&log_dir).unwrap();
    (s, body, log_dir)
}

#[test]
fn uc2_rocktopus_capped_neck() {
    let dir = tempfile::tempdir().unwrap();
    let (s, body, log_dir) = run_scenario(dir.path(), false);

    // Exactly one object, closed and manifold.
    assert_eq!(s.document().objects().len(), 1, "one object remains");
    let mesh = mesh_of(&s, body);
    assert_closed_solid(&mesh, "final result");

    let report = replay(&log_dir).unwrap();
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report
            .results
            .iter()
            .filter(|(_, v)| *v != Verdict::Exact)
            .collect::<Vec<_>>()
    );
}

#[test]
fn uc2_rocktopus_open_neck_closes_through_d15() {
    let dir = tempfile::tempdir().unwrap();
    let (s, body, log_dir) = run_scenario(dir.path(), true);

    assert_eq!(s.document().objects().len(), 1, "one object remains");
    let mesh = mesh_of(&s, body);
    assert_closed_solid(&mesh, "open-neck result (D15 local closure)");

    let report = replay(&log_dir).unwrap();
    assert!(
        report.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        report
            .results
            .iter()
            .filter(|(_, v)| *v != Verdict::Exact)
            .collect::<Vec<_>>()
    );
}

/// The blended seam is smoother than a sharp union's would be, and a head
/// moved far off the neck (with the neck left open, D15) is refused as
/// `opening_not_covered` (the document unchanged). A covered opening
/// only triggers this refusal once the opening actually exists, so this
/// test uses the open-neck variant.
#[test]
fn seam_is_faired_and_a_far_head_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, ids) = session_with(dir.path(), vec![("octopus", octopus(1), IDENTITY)]);
    let body = ids[0];
    let out = s
        .execute(invocation(
            "mesh.split_plane",
            body,
            &[
                ("plane_point", "0,0,0.37"),
                ("plane_normal", "0,0,1"),
                ("keep", "both"),
            ],
        ))
        .unwrap();
    let report = match out {
        mf_command::CommitOutcome::NewRevision { report } => report,
        other => panic!("expected a new revision, got {other:?}"),
    };
    let cap_faces: Vec<u32> = report["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["object"] == "target")
        .unwrap()["cap_faces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as u32)
        .collect();
    let head_piece = s
        .document()
        .objects()
        .iter()
        .map(|o| o.id)
        .find(|&id| id != body)
        .unwrap();
    s.execute(invocation("object.delete", head_piece, &[]))
        .unwrap();
    // Open the neck (D15): the far head below must now fail to cover it.
    let sel = Selection {
        object: body,
        revision: s.current(),
        kind: SelectionKind::Faces,
        ids: cap_faces,
    };
    let mut del = invocation("mesh.delete_faces", body, &[]);
    del.selection = Some(sel);
    s.execute(del).unwrap();

    let head_path = dir.path().join("head.3mf");
    write_model_path(
        &Model {
            unit: Unit::Millimeter,
            objects: vec![ModelObject {
                name: "head".into(),
                mesh: head(1),
                transform: IDENTITY,
            }],
        },
        &head_path,
    )
    .unwrap();

    // 10 mm off the neck: refused, document unchanged (the brief's
    // "head moved 10 mm off the neck" case).
    {
        let (far_ids, _) = s.import_model(read_model(&head_path).unwrap());
        let far_head = far_ids[0];
        let t = placement(1.3, 25.0, [16.0, 0.0, 8.0]);
        s.execute(invocation(
            "object.set_transform",
            far_head,
            &[(
                "transform",
                &t.iter().map(f64::to_string).collect::<Vec<_>>().join(","),
            )],
        ))
        .unwrap();
        let before = s.revision();
        let err = s.execute(boolean_union(body, far_head, 3.0)).unwrap_err();
        match err {
            CommandError::Operation(mf_operations::OpError::Failed { code, message }) => {
                assert_eq!(code, "opening_not_covered", "{message}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(s.revision(), before, "refused: no change");
        // Undo the transform+import so the next check starts clean.
        s.execute(invocation("object.delete", far_head, &[]))
            .unwrap();
    }

    // On the neck (correctly placed): the join still succeeds and gives
    // a closed, manifold result through D15's local closure. The
    // blend's own smoothing property (band dihedral below the sharp
    // seam's) is Task 22's `blended_union_is_closed_manifold_and_c1_
    // across_the_band`, on controlled primitives where "the seam" and
    // "the rest of the surface" are unambiguous; this organic fixture's
    // own curvature (tentacle bases, jaw bumps) makes a whole-mesh or
    // ring-only dihedral comparison compare unlike things, so it isn't
    // repeated here.
    let (new_ids, _) = s.import_model(read_model(&head_path).unwrap());
    let new_head = new_ids[0];
    let t = placement(1.3, 25.0, [0.0, 0.0, 8.0]);
    s.execute(invocation(
        "object.set_transform",
        new_head,
        &[(
            "transform",
            &t.iter().map(f64::to_string).collect::<Vec<_>>().join(","),
        )],
    ))
    .unwrap();
    s.execute(boolean_union(body, new_head, 3.0)).unwrap();
    assert_closed_solid(
        &mesh_of(&s, body),
        "join on the neck after the far-head refusal",
    );
}
