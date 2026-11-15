#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The four canonical Hollow acceptance tests (owner 2026-11-14), through
//! the command layer:
//!
//! 1. a single parametric shape (a Box, and a Cylinder, from the Parts
//!    generators);
//! 2. combined parametric shapes (Box ∪ Cylinder, `mesh.combine`);
//! 3. a solid made from combined parametric shapes (Make solid on 2);
//! 4. an organic shape: the Stanford bunny, Meshmixer's own classic
//!    Hollow test (Repair, filling the holes in its base, then Hollow).
//!
//! Each is hollowed three ways: plain, with Auto drain holes, and opened
//! through a flat cut face (Split first). Every result is closed and
//! manifold; its wall, measured, is the setting within a cell; the outer
//! surface is unchanged apart from the openings; an open-through rim
//! stays at the cut's own height. The bunny's Repair plus Hollow takes
//! 2 s or less in an optimised build.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use mf_command::{CommitOutcome, Invocation, ObjectId, Session};
use mf_fixtures::hollow_check::{Opening, check, flat_at, stood_bunny};
use mf_geometry::Mesh;
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};
use mf_operations::shapes::{ShapeToAdd, shapes_param};
use mf_runtime::RunContext;

const WALL: f64 = 2.0;

/// One case at a time: the bunny's timing is its own, never shared with
/// another case's hollow running alongside it.
static ALONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn alone() -> std::sync::MutexGuard<'static, ()> {
    ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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

fn report_of(outcome: CommitOutcome) -> serde_json::Value {
    match outcome {
        CommitOutcome::NewRevision { report } | CommitOutcome::NoChange { report } => report,
        CommitOutcome::Report(report) => report,
    }
}

/// A session holding `objects` (name, mesh, transform), read back from a
/// 3MF exactly as File > Open would.
fn session_of(dir: &Path, objects: Vec<(&str, Mesh, [f64; 12])>) -> (Session, Vec<ObjectId>) {
    let path = dir.join("scene.3mf");
    let model = Model {
        metadata: Default::default(),
        unit: Unit::Millimeter,
        objects: objects
            .into_iter()
            .map(|(name, mesh, transform)| ModelObject {
                metadata: Default::default(),
                name: name.into(),
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

fn at(x: f64, y: f64, z: f64) -> String {
    let mut t = IDENTITY;
    t[9] = x;
    t[10] = y;
    t[11] = z;
    t.map(|v| v.to_string()).join(",")
}

/// A Parts-library shape (`object.add_shape`), placed at `place`; its
/// new object's id.
fn add_shape(
    s: &mut Session,
    anchor: ObjectId,
    id: &str,
    params: &[(&str, &str)],
    place: [f64; 3],
) -> ObjectId {
    let shape = mf_parts::find_generator(id).unwrap();
    let raw: BTreeMap<String, String> = params
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    let record = mf_parts::new_record(shape, &raw).unwrap();
    let before: Vec<ObjectId> = s.document().objects().iter().map(|o| o.id).collect();
    s.execute(Invocation {
        op_id: "object.add_shape".into(),
        target: anchor,
        selection: None,
        operands: Vec::new(),
        raw_params: BTreeMap::from([
            (
                "shapes".to_owned(),
                shapes_param(&[ShapeToAdd {
                    generator: record,
                    name: shape.name().into(),
                    hole: false,
                    attach_to: None,
                    transform: None,
                    group: None,
                }]),
            ),
            ("transform".to_owned(), at(place[0], place[1], place[2])),
            ("group".to_owned(), String::new()),
        ]),
    })
    .unwrap();
    s.document()
        .objects()
        .iter()
        .map(|o| o.id)
        .find(|id| !before.contains(id))
        .unwrap()
}

/// A session with a far-away anchor object (Parts places shapes "on" a
/// target, as the app's Parts panel does on the active object).
fn parts_session(dir: &Path) -> (Session, ObjectId) {
    let (s, ids) = session_of(
        dir,
        vec![("anchor", mf_fixtures::cube(2.0), {
            let mut t = IDENTITY;
            t[9] = 500.0;
            t
        })],
    );
    (s, ids[0])
}

/// The object's mesh, and its transform's translation (every object here
/// is translated only).
fn mesh_of(s: &Session, id: ObjectId) -> (Mesh, [f64; 3]) {
    let o = s.document().object(id).unwrap();
    (
        o.mesh().unwrap().clone(),
        [o.transform[9], o.transform[10], o.transform[11]],
    )
}

/// Hollow `id` three ways (plain, Auto drain holes, open through a flat
/// cut face after Split at world height `cut_z`, keeping `keep`), each
/// checked, each undone after.
fn hollow_three_ways(s: &mut Session, id: ObjectId, what: &str, cut_z: f64, keep: &str) {
    let (before, t) = mesh_of(s, id);

    // Plain.
    let report = report_of(
        s.execute(invoke("mesh.hollow", id, &[("wall_thickness", "2")]))
            .unwrap_or_else(|e| panic!("{what}: plain hollow refused: {e}")),
    );
    let cell = report["cell_size"].as_f64().unwrap();
    let (out, _) = mesh_of(s, id);
    check(
        &format!("{what}, plain"),
        &before,
        &out,
        WALL,
        cell,
        &Opening::None,
    );
    assert!(s.undo());

    // Auto drain holes.
    let report = report_of(
        s.execute(invoke(
            "mesh.hollow",
            id,
            &[("wall_thickness", "2"), ("drain_holes", "auto")],
        ))
        .unwrap_or_else(|e| panic!("{what}: hollow with auto drain holes refused: {e}")),
    );
    let holes: Vec<([f64; 3], [f64; 3])> = report["holes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            let v = |k: &str| -> [f64; 3] {
                let a = h[k].as_array().unwrap();
                std::array::from_fn(|i| a[i].as_f64().unwrap())
            };
            (v("at"), v("axis"))
        })
        .collect();
    assert!(!holes.is_empty(), "{what}: auto placed no drain hole");
    let diameter = report["holes"][0]["diameter"].as_f64().unwrap();
    let (out, _) = mesh_of(s, id);
    check(
        &format!("{what}, auto drain holes"),
        &before,
        &out,
        WALL,
        cell,
        &Opening::Holes(holes, diameter),
    );
    assert!(s.undo());

    // Split, then open through the cut face.
    s.execute(invoke(
        "mesh.split_plane",
        id,
        &[
            ("plane_point", &format!("0,0,{cut_z}")),
            ("plane_normal", "0,0,1"),
            ("keep", keep),
            ("cap", "true"),
        ],
    ))
    .unwrap_or_else(|e| panic!("{what}: split refused: {e}"));
    let id = s
        .document()
        .objects()
        .iter()
        .find(|o| {
            o.name
                .ends_with(if keep == "below" { "(bottom)" } else { "(top)" })
        })
        .map(|o| o.id)
        .unwrap();
    let (cut, _) = mesh_of(s, id);
    let (z, side) = (cut_z - t[2], if keep == "below" { 1.0 } else { -1.0 });
    let faces = flat_at(&cut, z, side);
    assert!(faces.len() > 1, "{what}: no cut face at z = {z}");
    let list = faces
        .iter()
        .map(|f| f.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let report = report_of(
        s.execute(invoke(
            "mesh.hollow",
            id,
            &[
                ("wall_thickness", "2"),
                ("open_through", "true"),
                ("open_through_faces", &list),
            ],
        ))
        .unwrap_or_else(|e| panic!("{what}: open through the cut face refused: {e}")),
    );
    let cell = report["cell_size"].as_f64().unwrap();
    let (out, _) = mesh_of(s, id);
    check(
        &format!("{what}, open through the cut face"),
        &cut,
        &out,
        WALL,
        cell,
        &Opening::Through {
            z,
            side,
            faces: &faces,
        },
    );
    assert!(s.undo());
    assert!(s.undo());
}

/// Case 1: A single parametric shape: a Box, and a Cylinder.
#[test]
fn hollow_acceptance_a_single_parametric_box_and_cylinder() {
    let _alone = alone();
    let dir = tempfile::tempdir().unwrap();
    let (mut s, anchor) = parts_session(dir.path());
    let boxed = add_shape(
        &mut s,
        anchor,
        "shape.box",
        &[("width", "30"), ("depth", "24"), ("height", "20")],
        [0.0, 0.0, 0.0],
    );
    hollow_three_ways(&mut s, boxed, "box", 12.25, "below");
    let cylinder = add_shape(
        &mut s,
        anchor,
        "shape.cylinder",
        &[("diameter", "26"), ("height", "20")],
        [80.0, 0.0, 0.0],
    );
    hollow_three_ways(&mut s, cylinder, "cylinder", 12.25, "below");
}

/// Box ∪ Cylinder: a box with a cylinder standing up out of its top.
fn box_and_cylinder(s: &mut Session, anchor: ObjectId) -> ObjectId {
    let boxed = add_shape(
        s,
        anchor,
        "shape.box",
        &[("width", "40"), ("depth", "40"), ("height", "20")],
        [0.0, 0.0, 0.0],
    );
    let bb = s
        .document()
        .object(boxed)
        .unwrap()
        .mesh()
        .unwrap()
        .bounding_box()
        .unwrap();
    let centre = [(bb.min[0] + bb.max[0]) / 2.0, (bb.min[1] + bb.max[1]) / 2.0];
    let cylinder = add_shape(
        s,
        anchor,
        "shape.cylinder",
        &[("diameter", "24"), ("height", "40")],
        [centre[0], centre[1], 0.0],
    );
    s.execute(Invocation {
        op_id: "mesh.combine".into(),
        target: boxed,
        selection: None,
        operands: vec![("solids".to_string(), cylinder, None)],
        raw_params: BTreeMap::new(),
    })
    .unwrap_or_else(|e| panic!("combine: {e}"));
    boxed
}

/// Case 2: Combined parametric shapes: Box ∪ Cylinder, opened through the
/// cylinder's own cut face above the box.
#[test]
fn hollow_acceptance_combined_box_and_cylinder() {
    let _alone = alone();
    let dir = tempfile::tempdir().unwrap();
    let (mut s, anchor) = parts_session(dir.path());
    let id = box_and_cylinder(&mut s, anchor);
    hollow_three_ways(&mut s, id, "box ∪ cylinder", 30.25, "below");
}

/// Case 3: A solid made from combined parametric shapes: Make solid on Box ∪
/// Cylinder, then Hollow.
#[test]
fn hollow_acceptance_make_solid_of_combined_box_and_cylinder() {
    let _alone = alone();
    let dir = tempfile::tempdir().unwrap();
    let (mut s, anchor) = parts_session(dir.path());
    let id = box_and_cylinder(&mut s, anchor);
    s.execute(invoke("mesh.make_solid", id, &[]))
        .unwrap_or_else(|e| panic!("make solid: {e}"));
    hollow_three_ways(&mut s, id, "make solid of box ∪ cylinder", 30.25, "below");
}

/// The Stanford bunny, scaled to 100 mm across and stood up (its scan is
/// y-up, in metres), in a session of its own; `None` (a recorded skip)
/// if it hasn't been fetched.
fn bunny_session(dir: &Path, test: &str) -> Option<(Session, ObjectId)> {
    let path = mf_fixtures::real::path("stanford-bunny.obj", test)?;
    let model = read_model(&path).unwrap();
    let stood = stood_bunny(&model.objects.into_iter().next().unwrap().mesh);
    let (s, ids) = session_of(dir, vec![("bunny", stood, IDENTITY)]);
    Some((s, ids[0]))
}

/// Case 4: An organic shape: the Stanford bunny, Repair (its base holes
/// filled) then Hollow, three ways, the open-through one on a cut taken
/// off its base. Repair plus Hollow takes 2 s or less optimised.
#[test]
fn hollow_acceptance_the_stanford_bunny() {
    let _alone = alone();
    let dir = tempfile::tempdir().unwrap();
    let Some((mut s, id)) = bunny_session(dir.path(), "hollow_acceptance_the_stanford_bunny")
    else {
        return;
    };
    let start = Instant::now();
    let fill = report_of(
        s.execute(invoke("mesh.fill_holes", id, &[]))
            .unwrap_or_else(|e| panic!("bunny: repair refused: {e}")),
    );
    let repaired = start.elapsed();
    let (mesh, _) = mesh_of(&s, id);
    let topo = mesh.topology_with(&RunContext::none()).unwrap();
    assert_eq!(topo.boundary_edges, 0, "repair left holes: {fill}");
    let started = Instant::now();
    s.execute(invoke("mesh.hollow", id, &[("wall_thickness", "2")]))
        .unwrap_or_else(|e| panic!("bunny: hollow refused: {e}"));
    let hollowed = started.elapsed();
    let total = start.elapsed();
    eprintln!("bunny: repair {repaired:?}, hollow {hollowed:?}, total {total:?}");
    if !cfg!(debug_assertions) {
        assert!(
            total.as_secs_f64() <= 2.0,
            "bunny: repair plus hollow took {total:?} (repair {repaired:?}, hollow {hollowed:?})"
        );
    }
    assert!(s.undo());
    let bb = mesh.bounding_box().unwrap();
    hollow_three_ways(&mut s, id, "bunny", bb.min[2] + 8.25, "above");
}
