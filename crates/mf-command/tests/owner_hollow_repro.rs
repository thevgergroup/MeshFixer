#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Reproduction of the owner's report (2026-11-13): Hollow fails on a
//! complex shape built from the demo file
//! `artifacts/phase1/demo/boolean-demo.3mf` ('scan' a sphere, 'part' a
//! cube sunk into its side):
//!
//! 1. Split 'scan' and 'part' together, Keep Bottom, Cap on (a hemisphere
//!    and a half cube).
//! 2. Combine them (the half cube joins the hemisphere).
//! 3. Make solid on the result.
//! 4. Hollow it (2 mm wall), both plain and `open_through` on the cut
//!    face.
//!
//! This test MUST fail (the hollow call returns an error) before any fix
//! lands, capturing the owner's exact refusal through the command layer.

use std::collections::BTreeMap;
use std::path::Path;

use mf_command::{CommandError, CommitOutcome, Invocation, ObjectId, Session};
use mf_io::read_model;

// The owner's demo file is `artifacts/phase1/demo/boolean-demo.3mf`
// (untracked, main checkout only); the identical file (same content) is
// fixtured in-repo at `fixtures/owner/boolean-demo.3mf` for tests, as
// `mf-desktop`'s `core_workflows.rs` already does for this same demo.
const DEMO: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/owner/boolean-demo.3mf"
);

fn session_from_demo() -> Session {
    let model = read_model(Path::new(DEMO)).unwrap();
    let mut s = Session::new();
    s.import_model(model);
    s
}

fn id_of(s: &Session, name: &str) -> ObjectId {
    s.document()
        .objects()
        .iter()
        .find(|o| o.name == name)
        .unwrap_or_else(|| panic!("no object named {name:?}"))
        .id
}

fn invoke(op_id: &str, target: ObjectId, params: &[(&str, &str)]) -> Invocation {
    Invocation {
        op_id: op_id.into(),
        target,
        selection: None,
        operands: Vec::new(),
        raw_params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn combine(target: ObjectId, solids: &[ObjectId]) -> Invocation {
    Invocation {
        op_id: "mesh.combine".into(),
        target,
        selection: None,
        operands: solids
            .iter()
            .map(|&o| ("solids".to_string(), o, None))
            .collect(),
        raw_params: BTreeMap::new(),
    }
}

/// Split 'scan' and 'part' together, Keep Bottom, Cap on — the owner's
/// step 1. Returns the two new "(bottom)" object ids.
fn split_both_keep_bottom(s: &mut Session) -> (ObjectId, ObjectId) {
    let scan = id_of(s, "scan");
    let part = id_of(s, "part");
    for target in [scan, part] {
        let outcome = s
            .execute(invoke(
                "mesh.split_plane",
                target,
                &[
                    ("plane_point", "2.5,0,20"),
                    ("plane_normal", "0,0,1"),
                    ("keep", "below"),
                    ("cap", "true"),
                ],
            ))
            .unwrap_or_else(|e| panic!("split of target {target:?} failed: {e}"));
        assert!(
            matches!(outcome, CommitOutcome::NewRevision { .. }),
            "split did not commit a new revision"
        );
    }
    (id_of(s, "scan (bottom)"), id_of(s, "part (bottom)"))
}

/// Steps 1-3 of the owner's exact repro: split both, combine, make solid.
/// Returns the resulting solid's object id.
fn hemisphere_plus_half_cube_solid(s: &mut Session) -> ObjectId {
    let (hemisphere, half_cube) = split_both_keep_bottom(s);
    let outcome = s
        .execute(combine(hemisphere, &[half_cube]))
        .unwrap_or_else(|e| panic!("combine failed: {e}"));
    assert!(
        matches!(outcome, CommitOutcome::NewRevision { .. }),
        "combine did not commit a new revision"
    );
    let combined = hemisphere; // combine keeps the target's id
    let outcome = s
        .execute(invoke("mesh.make_solid", combined, &[]))
        .unwrap_or_else(|e| panic!("make_solid failed: {e}"));
    assert!(
        matches!(outcome, CommitOutcome::NewRevision { .. }),
        "make_solid did not commit a new revision"
    );
    combined
}

/// Plain Hollow (2 mm wall) on the owner's Make solid output must never
/// refuse a valid closed solid.
#[test]
fn hollow_after_make_solid_on_combined_hemisphere_and_half_cube() {
    let mut s = session_from_demo();
    let solid = hemisphere_plus_half_cube_solid(&mut s);
    // Sanity: Make solid's own output must already be a valid D4 solid.
    let mesh = s.document().object(solid).unwrap().mesh().unwrap().clone();
    let (p, t) = mesh.to_arrays();
    mf_boolean::validate_solid(&p, &t, &mf_runtime::RunContext::none())
        .unwrap_or_else(|e| panic!("Make solid's own output isn't a valid solid: {e}"));

    let result = s.execute(invoke(
        "mesh.hollow",
        solid,
        &[
            ("wall_thickness", "2"),
            ("inner_detail", "0.5"),
            ("drain_holes", "none"),
        ],
    ));
    match result {
        Ok(_) => {}
        Err(CommandError::Operation(e)) => {
            panic!("Hollow refused a Make-solid-produced closed solid: {e}");
        }
        Err(e) => panic!("Hollow failed: {e}"),
    }
}

/// The owner's step 4, the other way they tried it: `open_through` on the
/// face at the bottom of the cut (the lowest-z live face, standing in for
/// the owner's click on that region) must also never refuse a valid
/// Make-solid output.
#[test]
fn hollow_open_through_after_make_solid_on_combined_hemisphere_and_half_cube() {
    let mut s = session_from_demo();
    let solid = hemisphere_plus_half_cube_solid(&mut s);
    let mesh = s.document().object(solid).unwrap().mesh().unwrap().clone();
    let seed = (0..mesh.face_slots())
        .filter_map(|f| {
            let face = mesh.face(mf_geometry::FaceId(f))?;
            let z = face
                .iter()
                .map(|&v| mesh.corner_position(v)[2])
                .sum::<f64>()
                / 3.0;
            Some((z, f))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, f)| f)
        .expect("a live face");

    let result = s.execute(invoke(
        "mesh.hollow",
        solid,
        &[
            ("wall_thickness", "2"),
            ("inner_detail", "0.5"),
            ("open_through", "true"),
            ("open_through_faces", &seed.to_string()),
        ],
    ));
    match result {
        Ok(_) => {}
        Err(CommandError::Operation(e)) => {
            panic!("Hollow (open_through) refused a Make-solid-produced closed solid: {e}");
        }
        Err(e) => panic!("Hollow (open_through) failed: {e}"),
    }
}
