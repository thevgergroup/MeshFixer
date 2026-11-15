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
/// bottom of the hemisphere (its downward-facing faces within 1 mm of
/// its lowest point, standing in for the owner's click on that smooth
/// region; a single sliver triangle there is too small to open through,
/// refused by name) must also never refuse a valid Make-solid output.
#[test]
fn hollow_open_through_after_make_solid_on_combined_hemisphere_and_half_cube() {
    let mut s = session_from_demo();
    let solid = hemisphere_plus_half_cube_solid(&mut s);
    let mesh = s.document().object(solid).unwrap().mesh().unwrap().clone();
    let bottom = mesh.bounding_box().unwrap().min[2];
    let picked: Vec<String> = mesh
        .faces()
        .filter(|(_, t)| {
            let p = t.map(|v| mesh.corner_position(v));
            let (a, b) = (
                [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]],
                [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]],
            );
            let n = [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            len > 0.0 && -n[2] >= 0.9 * len && p.iter().all(|q| q[2] <= bottom + 1.0)
        })
        .map(|(f, _)| f.0.to_string())
        .collect();
    assert!(
        picked.len() > 2,
        "the hemisphere's bottom: {} faces",
        picked.len()
    );
    let seed = picked.join(",");

    let result = s.execute(invoke(
        "mesh.hollow",
        solid,
        &[
            ("wall_thickness", "2"),
            ("inner_detail", "0.5"),
            ("open_through", "true"),
            ("open_through_faces", &seed),
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

// ---------------------------------------------------------------------
// Owner 2026-11-14, take 3: open through the flat cut face of
// scan (bottom) + part (bottom), each split on its own.
// ---------------------------------------------------------------------

/// `name`'s world bounding-box centre: Split's default plane point for a
/// lone selection (the GUI's own "selection's bounding-box centre").
fn world_centre(s: &Session, name: &str) -> [f64; 3] {
    let o = s.document().object(id_of(s, name)).unwrap();
    let bb = o.mesh().unwrap().bounding_box().unwrap();
    std::array::from_fn(|k| (bb.min[k] + bb.max[k]) / 2.0 + o.transform[9 + k])
}

/// Split `name` alone at its own default horizontal plane, Keep Bottom,
/// Cap on. Returns the plane's world height.
fn split_keep_bottom(s: &mut Session, name: &str) -> f64 {
    let c = world_centre(s, name);
    let target = id_of(s, name);
    s.execute(invoke(
        "mesh.split_plane",
        target,
        &[
            ("plane_point", &format!("{},{},{}", c[0], c[1], c[2])),
            ("plane_normal", "0,0,1"),
            ("keep", "below"),
            ("cap", "true"),
        ],
    ))
    .unwrap_or_else(|e| panic!("split of {name} failed: {e}"));
    c[2]
}

/// The owner's exact steps 1-4: Split 'scan' (Keep Bottom), Make solid
/// on 'scan (bottom)', Split 'part' the same way, Combine the two into
/// 'part (bottom)'. Returns the combined object and the cut's own local
/// height in it.
fn owner_take3_combined(s: &mut Session) -> (ObjectId, f64) {
    let cut_z = split_keep_bottom(s, "scan");
    let scan = id_of(s, "scan (bottom)");
    s.execute(invoke("mesh.make_solid", scan, &[]))
        .unwrap_or_else(|e| panic!("make_solid failed: {e}"));
    let part_cut_z = split_keep_bottom(s, "part");
    assert_eq!(cut_z, part_cut_z, "both cut at the same height");
    let part = id_of(s, "part (bottom)");
    s.execute(combine(part, &[scan]))
        .unwrap_or_else(|e| panic!("combine failed: {e}"));
    let o = s.document().object(part).unwrap();
    (part, cut_z - o.transform[11])
}

/// Every live face lying flat on the cut plane, facing up: the flat cut
/// face the owner clicked (a disc with the cube's tab joined on).
fn cut_face(mesh: &mf_geometry::Mesh, cut_z: f64) -> Vec<u32> {
    mesh.faces()
        .filter(|(_, t)| {
            let p = t.map(|v| mesh.corner_position(v));
            let n = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0]);
            n > 0.0 && p.iter().all(|q| (q[2] - cut_z).abs() < 1e-6)
        })
        .map(|(f, _)| f.0)
        .collect()
}

/// Owner 2026-11-14, take 3: Hollow (2 mm wall) with Open through on the
/// flat cut face is refused (`open_through_region_too_curved`: the
/// picked outline, shrunk inward by the wall, crossed itself at the
/// tab's neck). The opening must instead follow the cavity: a single
/// closed, manifold shell, open at the cut face, with the rim still at
/// the cut height.
#[test]
fn hollow_open_through_the_flat_cut_face_of_scan_and_part_split_separately() {
    let mut s = session_from_demo();
    let (solid, cut_z) = owner_take3_combined(&mut s);
    let mesh = s.document().object(solid).unwrap().mesh().unwrap().clone();
    let faces = cut_face(&mesh, cut_z);
    assert!(faces.len() > 2, "the cut face: {} faces", faces.len());
    let list = faces
        .iter()
        .map(|f| f.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let result = s.execute(invoke(
        "mesh.hollow",
        solid,
        &[
            ("wall_thickness", "2"),
            ("open_through", "true"),
            ("open_through_faces", &list),
        ],
    ));
    match result {
        Ok(_) => {}
        Err(e) => panic!("Hollow (open_through) on the flat cut face was refused: {e}"),
    }
    let shell = s.document().object(solid).unwrap().mesh().unwrap().clone();
    let topo = shell
        .topology_with(&mf_runtime::RunContext::none())
        .unwrap();
    assert_eq!(topo.boundary_edges, 0, "closed");
    assert!(
        topo.non_manifold_edges.is_empty() && topo.inconsistent_edges == 0,
        "{topo:?}"
    );
    assert_eq!(topo.components.len(), 1, "one open bowl");
    let top = shell.bounding_box().unwrap().max[2];
    assert!((top - cut_z).abs() < 1e-9, "rim at {top}, cut at {cut_z}");
    // Open: the cut face's own middle is gone (no face left at the cut
    // height over the cavity), yet the rim remains there.
    let left = cut_face(&shell, cut_z).len();
    assert!(left > 0, "the rim is still at the cut height");
    let before: f64 = area_at(&mesh, cut_z);
    let after: f64 = area_at(&shell, cut_z);
    assert!(
        after < 0.5 * before,
        "the opening removed too little of the cut face: {after} of {before}"
    );
}

fn area_at(mesh: &mf_geometry::Mesh, cut_z: f64) -> f64 {
    let ids = cut_face(mesh, cut_z);
    ids.iter()
        .map(|&f| {
            let p = mesh
                .face(mf_geometry::FaceId(f))
                .unwrap()
                .map(|v| mesh.corner_position(v));
            0.5 * ((p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                - (p[1][1] - p[0][1]) * (p[2][0] - p[0][0]))
        })
        .sum()
}
