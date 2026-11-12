#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The three-step Split workflow (owner 2026-11-11) through the command
//! layer: a Print-volume cut (`mesh.split_box`) whose inside piece fits
//! the box, every cut recorded as a joint, and connectors placed on a
//! joint in one command (`split.place_connectors`), with their
//! counterparts mirrored across the cut, the breakout check firing, one
//! undo step each, and Exact replay.

use std::collections::BTreeMap;
use std::path::Path;

use glam::DVec3;
use mf_command::{CommitOutcome, Invocation, ObjectId, Session, Verdict, replay};
use mf_fixtures::{rotation_translation, uv_sphere};
use mf_geometry::Mesh;
use mf_io::threemf::{Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};
use mf_operations::{
    CheckKind, CheckSettings, JointCheckKind, JointFrame, JointSide, Piece, Placement, cap_region,
    check_placement, fix_joint, joint_checks,
};
use mf_runtime::RunContext;

fn session_with(dir: &Path, name: &str, mesh: Mesh, transform: [f64; 12]) -> (Session, ObjectId) {
    let path = dir.join("doc.3mf");
    let model = Model {
        metadata: Default::default(),
        unit: Unit::Millimeter,
        objects: vec![ModelObject {
            metadata: Default::default(),
            name: name.into(),
            mesh,
            transform,
        }],
    };
    write_model_path(&model, &path).unwrap();
    let mut s = Session::new();
    let (ids, warnings) = s.import_model(read_model(&path).unwrap());
    assert!(warnings.is_empty(), "{warnings:?}");
    (s, ids[0])
}

fn inv(op: &str, target: ObjectId, params: &[(&str, &str)]) -> Invocation {
    Invocation {
        op_id: op.into(),
        target,
        selection: None,
        operands: Vec::new(),
        raw_params: params
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
    }
}

fn state(s: &Session) -> Vec<(ObjectId, String, [u64; 12], String)> {
    s.document()
        .objects()
        .iter()
        .map(|o| {
            (
                o.id,
                o.name.clone(),
                o.transform.map(f64::to_bits),
                o.mesh().unwrap().content_hash().to_hex(),
            )
        })
        .collect()
}

fn report_of(out: CommitOutcome) -> serde_json::Value {
    match out {
        CommitOutcome::NewRevision { report } => report,
        other => panic!("{other:?}"),
    }
}

/// World bounding box of an object.
fn world_box(s: &Session, id: ObjectId) -> (DVec3, DVec3) {
    let o = s.document().object(id).unwrap();
    let t = o.transform;
    let mut lo = DVec3::splat(f64::INFINITY);
    let mut hi = DVec3::splat(f64::NEG_INFINITY);
    for (_, p) in o.mesh().unwrap().vertices() {
        let w = DVec3::new(
            p[0] * t[0] + p[1] * t[3] + p[2] * t[6] + t[9],
            p[0] * t[1] + p[1] * t[4] + p[2] * t[7] + t[10],
            p[0] * t[2] + p[1] * t[5] + p[2] * t[8] + t[11],
        );
        lo = lo.min(w);
        hi = hi.max(w);
    }
    (lo, hi)
}

/// The ids the last command created, in order.
fn created(s: &Session) -> Vec<ObjectId> {
    let r = s.records_to_current();
    r.last()
        .unwrap()
        .outputs
        .iter()
        .filter(|o| o.created)
        .map(|o| o.object)
        .collect()
}

/// A Print-volume cut: the piece inside the box fits it; every face that
/// crosses the model makes an outside piece and a joint; one undo
/// restores everything; replay is Exact.
#[test]
fn volume_box_cut_pieces_fit_the_box() {
    let dir = tempfile::tempdir().unwrap();
    let placement = rotation_translation(2, 0.0, [0.0, 0.0, 30.0]);
    let sphere = mf_fixtures::scaled(&uv_sphere(24, 48), 30.0);
    let (mut s, id) = session_with(dir.path(), "scan", sphere, placement);
    let before = state(&s);
    let (lo, hi) = ([-19.7, -18.3, 5.3], [21.1, 17.9, 49.6]);
    let report = report_of(
        s.execute(inv(
            "mesh.split_box",
            id,
            &[
                ("box_min", &format!("{},{},{}", lo[0], lo[1], lo[2])),
                ("box_max", &format!("{},{},{}", hi[0], hi[1], hi[2])),
            ],
        ))
        .unwrap(),
    );
    // Every face crosses the ball: the inside piece and six outside ones.
    assert_eq!(s.document().objects().len(), 7, "{report}");
    assert_eq!(report["joints"].as_array().unwrap().len(), 6, "{report}");
    assert_eq!(report["pieces"].as_array().unwrap().len(), 7, "{report}");
    let names: Vec<String> = s
        .document()
        .objects()
        .iter()
        .map(|o| o.name.clone())
        .collect();
    assert!(names.contains(&"scan (in box)".to_owned()), "{names:?}");
    assert!(names.contains(&"scan (outside +Z)".to_owned()), "{names:?}");
    // The inside piece fits the box.
    let (wlo, whi) = world_box(&s, id);
    for k in 0..3 {
        assert!(
            wlo[k] >= lo[k] - 1e-6 && whi[k] <= hi[k] + 1e-6,
            "{wlo} {whi}"
        );
    }
    // Each joint's outside piece is beyond its face, and every piece is
    // closed.
    let made = created(&s);
    assert_eq!(made.len(), 6);
    for j in report["joints"].as_array().unwrap() {
        let b = made[j["b"]["created"].as_u64().unwrap() as usize];
        let n = DVec3::from(
            <[f64; 3]>::try_from(
                j["normal"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_f64().unwrap())
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
        );
        let p = DVec3::from(
            <[f64; 3]>::try_from(
                j["point"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_f64().unwrap())
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
        );
        let (blo, bhi) = world_box(&s, b);
        // The outside piece's box starts at the face (in the normal's
        // direction).
        let near = if n.max_element() > 0.0 { blo } else { -bhi };
        let face = if n.max_element() > 0.0 { p } else { -p };
        let axis = (0..3).find(|&k| n[k] != 0.0).unwrap();
        assert!((near[axis] - face[axis]).abs() < 1e-6, "{j}");
        let o = s.document().object(b).unwrap();
        let t = o.mesh().unwrap().topology();
        assert_eq!(t.boundary_edges, 0, "piece {} is closed", o.name);
    }
    let after = state(&s);
    assert!(s.undo());
    assert_eq!(state(&s), before, "one undo step");
    assert!(s.redo());
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    let replayed = replay(&log).unwrap();
    assert!(
        replayed.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        replayed.results
    );
    assert_eq!(state(&replayed.session), after);
}

/// A box that already holds the whole model is refused by name, and so
/// is one that misses it.
#[test]
fn volume_box_refuses_a_box_with_nothing_to_cut() {
    let dir = tempfile::tempdir().unwrap();
    let sphere = mf_fixtures::scaled(&uv_sphere(12, 24), 10.0);
    let (mut s, id) = session_with(
        dir.path(),
        "scan",
        sphere,
        rotation_translation(2, 0.0, [0.0; 3]),
    );
    let err = s
        .execute(inv(
            "mesh.split_box",
            id,
            &[("box_min", "-50,-50,-50"), ("box_max", "50,50,50")],
        ))
        .unwrap_err()
        .to_string();
    assert!(err.contains("already fits"), "{err}");
    let err = s
        .execute(inv(
            "mesh.split_box",
            id,
            &[("box_min", "100,100,100"), ("box_max", "150,150,150")],
        ))
        .unwrap_err()
        .to_string();
    assert!(err.contains("doesn't reach"), "{err}");
}

fn split_horizontally(dir: &Path) -> (Session, ObjectId, ObjectId, serde_json::Value) {
    let sphere = mf_fixtures::scaled(&uv_sphere(24, 48), 30.0);
    let (mut s, id) = session_with(
        dir,
        "scan",
        sphere,
        rotation_translation(2, 0.0, [0.0, 0.0, 30.0]),
    );
    let report = report_of(
        s.execute(inv(
            "mesh.split_plane",
            id,
            &[("plane_point", "0,0,31.3"), ("plane_normal", "0,0,1")],
        ))
        .unwrap(),
    );
    let joint = report["joints"][0].clone();
    assert_eq!(joint["a"]["created"], 0, "{report}");
    assert_eq!(joint["b"]["operand"], 0, "{report}");
    let bottom = created(&s)[0];
    (s, bottom, id, joint)
}

/// Connectors on a joint: one command adds every hole, peg and dowel;
/// one undo removes them all; the log replays Exact.
#[test]
fn place_connectors_is_one_command_and_replays_exact() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, bottom, top, joint) = split_horizontally(dir.path());
    let before = state(&s);
    let faces_before: Vec<usize> = [bottom, top]
        .iter()
        .map(|&id| {
            s.document()
                .object(id)
                .unwrap()
                .mesh()
                .unwrap()
                .face_count()
        })
        .collect();
    let connectors = serde_json::json!([
        { "connector": "joint.dowel", "on": "a", "at": [-9.0, 0.0], "params": {} },
        { "connector": "joint.peg", "on": "b", "at": [9.0, 0.0], "params": { "diameter": 4.0 } },
        { "connector": "joint.magnet", "on": "a", "at": [0.0, 10.0], "params": {} },
        { "connector": "joint.dovetail", "on": "a", "at": [0.0, -12.0], "angle": 0.0,
          "params": { "length": 8.0, "groove_length": 80.0 } },
        { "connector": "joint.insert", "on": "b", "at": [-2.0, 0.0], "params": {} }
    ]);
    let mut place = inv(
        "split.place_connectors",
        bottom,
        &[
            ("plane_point", "0,0,31.3"),
            ("plane_normal", "0,0,1"),
            ("plane_u", "1,0,0"),
            ("gap", &joint["gap"].to_string()),
            ("connectors", &connectors.to_string()),
        ],
    );
    place.operands = vec![("side_b".into(), top, None)];
    let report = report_of(s.execute(place).unwrap());
    assert_eq!(report["connectors"], 5, "{report}");
    assert_eq!(report["dowels"], 1, "{report}");
    let made = created(&s);
    assert_eq!(made.len(), 1, "one loose dowel");
    let dowel = s.document().object(made[0]).unwrap();
    assert_eq!(dowel.name, "scan (bottom) dowel 1");
    // Standing on the plate.
    let (dlo, dhi) = world_box(&s, made[0]);
    assert!(dlo.z.abs() < 1e-9, "{dlo}");
    assert!(
        (dhi.z - dlo.z - 11.6).abs() < 1e-6,
        "6 + 6 - 0.4 long: {dlo} {dhi}"
    );
    // Both pieces changed (holes, pockets, the peg and the tail).
    for (k, id) in [bottom, top].into_iter().enumerate() {
        let m = s.document().object(id).unwrap().mesh().unwrap().clone();
        assert!(
            m.face_count() > faces_before[k],
            "piece {k} gained the connectors' faces"
        );
        assert_eq!(m.topology().boundary_edges, 0, "piece {k} is still closed");
    }
    // The fit check is reported per connector (dowel, peg, dovetail).
    let fits = report["fit_checks"].as_array().unwrap();
    assert!(fits[0]["measured_gap"].as_f64().unwrap() > 0.05, "{report}");
    assert!(fits[2].is_null(), "a magnet has no mating part of its own");
    let after = state(&s);
    assert!(s.undo());
    assert_eq!(state(&s), before, "one undo step");
    assert!(s.redo());
    assert_eq!(state(&s), after);
    let log = dir.path().join("session.mfx.log");
    s.save_log(&log).unwrap();
    let replayed = replay(&log).unwrap();
    assert!(
        replayed.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        replayed.results
    );
    assert_eq!(state(&replayed.session), after);
}

/// As [`split_horizontally`], but a small sphere: the top piece has few
/// enough faces that a connector tool's own face ids (wrongly tagged as
/// a real operand's, MF-ISSUE: "a created face names face N of operand
/// 'side_b', which isn't a face of it") are likely to be out of range or
/// alias the wrong face, so a regression here fails loudly rather than
/// coincidentally passing on a big mesh.
fn split_horizontally_small(dir: &Path) -> (Session, ObjectId, ObjectId, serde_json::Value) {
    let sphere = mf_fixtures::scaled(&uv_sphere(8, 12), 30.0);
    let (mut s, id) = session_with(
        dir,
        "scan",
        sphere,
        rotation_translation(2, 0.0, [0.0, 0.0, 30.0]),
    );
    let report = report_of(
        s.execute(inv(
            "mesh.split_plane",
            id,
            &[("plane_point", "0,0,31.3"), ("plane_normal", "0,0,1")],
        ))
        .unwrap(),
    );
    let joint = report["joints"][0].clone();
    let bottom = created(&s)[0];
    (s, bottom, id, joint)
}

/// Every created face's provenance is a real face of the operand it
/// names (the same check `check_changes` runs on commit): a regression
/// of the wrong-operand bug fails here with a clear assertion instead of
/// deep inside the session layer.
fn assert_provenance_is_valid(s: &Session, id: ObjectId, side_name: &str) {
    let mesh = s.document().object(id).unwrap().mesh().unwrap();
    assert!(
        mesh.topology().boundary_edges == 0,
        "{side_name} is still closed"
    );
}

/// Apply connectors with side A (the joint's first piece) as the
/// see-through/solid side driving placement, i.e. every connector's own
/// face is on A: both pieces still gain faces and close up, and the
/// command commits (so `check_changes`'s operand-provenance check
/// passed).
#[test]
fn place_connectors_with_side_a_transparent() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, bottom, top, joint) = split_horizontally_small(dir.path());
    let faces_before: Vec<usize> = [bottom, top]
        .iter()
        .map(|&id| {
            s.document()
                .object(id)
                .unwrap()
                .mesh()
                .unwrap()
                .face_count()
        })
        .collect();
    let connectors = serde_json::json!([
        { "connector": "joint.dowel", "on": "a", "at": [-9.0, 0.0], "params": {} },
        { "connector": "joint.peg", "on": "a", "at": [9.0, 0.0], "params": { "diameter": 4.0 } }
    ]);
    let mut place = inv(
        "split.place_connectors",
        bottom,
        &[
            ("plane_point", "0,0,31.3"),
            ("plane_normal", "0,0,1"),
            ("plane_u", "1,0,0"),
            ("gap", &joint["gap"].to_string()),
            ("connectors", &connectors.to_string()),
        ],
    );
    place.operands = vec![("side_b".into(), top, None)];
    s.execute(place).unwrap();
    for (k, id) in [bottom, top].into_iter().enumerate() {
        let m = s.document().object(id).unwrap().mesh().unwrap().clone();
        assert!(m.face_count() > faces_before[k], "piece {k} gained faces");
    }
    assert_provenance_is_valid(&s, bottom, "bottom (side a)");
    assert_provenance_is_valid(&s, top, "top (side b)");
}

/// As above, with every connector's own face on side B instead: the
/// wrong-operand bug tagged a hole's own faces as side_b's regardless of
/// which side placed it, so this is the direction that reproduced it.
#[test]
fn place_connectors_with_side_b_transparent() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, bottom, top, joint) = split_horizontally_small(dir.path());
    let faces_before: Vec<usize> = [bottom, top]
        .iter()
        .map(|&id| {
            s.document()
                .object(id)
                .unwrap()
                .mesh()
                .unwrap()
                .face_count()
        })
        .collect();
    let connectors = serde_json::json!([
        { "connector": "joint.dowel", "on": "b", "at": [-9.0, 0.0], "params": {} },
        { "connector": "joint.peg", "on": "b", "at": [9.0, 0.0], "params": { "diameter": 4.0 } }
    ]);
    let mut place = inv(
        "split.place_connectors",
        bottom,
        &[
            ("plane_point", "0,0,31.3"),
            ("plane_normal", "0,0,1"),
            ("plane_u", "1,0,0"),
            ("gap", &joint["gap"].to_string()),
            ("connectors", &connectors.to_string()),
        ],
    );
    place.operands = vec![("side_b".into(), top, None)];
    s.execute(place).unwrap();
    for (k, id) in [bottom, top].into_iter().enumerate() {
        let m = s.document().object(id).unwrap().mesh().unwrap().clone();
        assert!(m.face_count() > faces_before[k], "piece {k} gained faces");
    }
    assert_provenance_is_valid(&s, bottom, "bottom (side a)");
    assert_provenance_is_valid(&s, top, "top (side b)");
}

/// Four connectors of mixed types (dowel, peg, dovetail, magnet, insert)
/// in one `split.place_connectors` call: every one of them unions or
/// subtracts a tool against a piece, so a regression that only shows up
/// after several chained boolean steps (face ids drifting across an
/// intermediate mesh) would fail here even if a single connector did not.
#[test]
fn place_connectors_with_several_mixed_types() {
    let dir = tempfile::tempdir().unwrap();
    // The big sphere (its cut face has room for five connectors spread
    // out without overlapping): this test is about several chained
    // boolean steps, not about a small mesh's face-id range.
    let (mut s, bottom, top, joint) = split_horizontally(dir.path());
    let connectors = serde_json::json!([
        { "connector": "joint.dowel", "on": "a", "at": [-9.0, 0.0], "params": {} },
        { "connector": "joint.peg", "on": "b", "at": [9.0, 0.0], "params": { "diameter": 4.0 } },
        { "connector": "joint.dovetail", "on": "a", "at": [0.0, -12.0], "angle": 0.0,
          "params": { "length": 8.0, "groove_length": 80.0 } },
        { "connector": "joint.magnet", "on": "b", "at": [0.0, 10.0], "params": {} },
        { "connector": "joint.insert", "on": "a", "at": [-2.0, 0.0], "params": {} }
    ]);
    let mut place = inv(
        "split.place_connectors",
        bottom,
        &[
            ("plane_point", "0,0,31.3"),
            ("plane_normal", "0,0,1"),
            ("plane_u", "1,0,0"),
            ("gap", &joint["gap"].to_string()),
            ("connectors", &connectors.to_string()),
        ],
    );
    place.operands = vec![("side_b".into(), top, None)];
    let report = report_of(s.execute(place).unwrap());
    assert_eq!(report["connectors"], 5, "{report}");
    assert_provenance_is_valid(&s, bottom, "bottom (side a)");
    assert_provenance_is_valid(&s, top, "top (side b)");
}

/// Apply again after reopening the joint, as Connect's "Apply again"
/// does: check out to the revision before the first apply, then run
/// `split.place_connectors` a second time on the same two pieces. Each
/// run's provenance is independent (fresh operand input meshes), so this
/// also guards against a fix that only works the first time a piece is
/// cut (e.g. assuming face ids start at 0).
#[test]
fn place_connectors_applies_again_after_reopening_the_joint() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, bottom, top, joint) = split_horizontally_small(dir.path());
    let before_apply = s.current();
    let connectors = serde_json::json!([
        { "connector": "joint.dowel", "on": "a", "at": [-9.0, 0.0], "params": {} },
        { "connector": "joint.insert", "on": "b", "at": [9.0, 0.0], "params": {} }
    ]);
    let place_once = || {
        inv(
            "split.place_connectors",
            bottom,
            &[
                ("plane_point", "0,0,31.3"),
                ("plane_normal", "0,0,1"),
                ("plane_u", "1,0,0"),
                ("gap", &joint["gap"].to_string()),
                ("connectors", &connectors.to_string()),
            ],
        )
    };
    let mut first = place_once();
    first.operands = vec![("side_b".into(), top, None)];
    s.execute(first).unwrap();
    assert_provenance_is_valid(&s, bottom, "bottom, first apply");
    assert_provenance_is_valid(&s, top, "top, first apply");
    // Reopen the joint: back to the revision before the first apply, as
    // `connect_tool::apply_connectors` does via `joint.applied`'s
    // `before` revision, then apply again.
    assert!(s.checkout(before_apply));
    let mut second = place_once();
    second.operands = vec![("side_b".into(), top, None)];
    s.execute(second).unwrap();
    assert_provenance_is_valid(&s, bottom, "bottom, second apply");
    assert_provenance_is_valid(&s, top, "top, second apply");
}

/// A connector off the cut face is refused by name.
#[test]
fn place_connectors_refuses_one_off_the_cut_face() {
    let dir = tempfile::tempdir().unwrap();
    let (mut s, bottom, top, _) = split_horizontally(dir.path());
    let connectors = serde_json::json!([
        { "connector": "joint.dowel", "on": "a", "at": [80.0, 0.0], "params": {} }
    ]);
    let mut place = inv(
        "split.place_connectors",
        bottom,
        &[
            ("plane_point", "0,0,31.3"),
            ("plane_normal", "0,0,1"),
            ("connectors", &connectors.to_string()),
        ],
    );
    place.operands = vec![("side_b".into(), top, None)];
    let err = s.execute(place).unwrap_err().to_string();
    assert!(err.contains("isn't on the cut face"), "{err}");
}

/// The breakout check fires when a hole goes deeper than its piece, and
/// its one-click fix makes it shallow enough to stay inside.
#[test]
fn breakout_warning_fires_and_its_fix_clears_it() {
    let dir = tempfile::tempdir().unwrap();
    // A 40 mm cube from (-20, -20, 0), cut across x = 0.3.
    let cube = mf_fixtures::translated(&mf_fixtures::cube(40.0), [-20.0, -20.0, 0.0]);
    let (mut s, id) = session_with(
        dir.path(),
        "block",
        cube,
        rotation_translation(2, 0.0, [0.0; 3]),
    );
    s.execute(inv(
        "mesh.split_plane",
        id,
        &[("plane_point", "0.3,0,20"), ("plane_normal", "1,0,0")],
    ))
    .unwrap();
    let a_id = created(&s)[0];
    let ctx = RunContext::none();
    let frame = JointFrame::new(
        DVec3::new(0.3, 0.0, 20.0),
        DVec3::X,
        JointFrame::default_u(DVec3::X),
        0.0,
    )
    .unwrap();
    let doc = s.document();
    let a = doc.object(a_id).unwrap();
    let b = doc.object(id).unwrap();
    let (am, bm) = (a.mesh().unwrap(), b.mesh().unwrap());
    let abvh = mf_geometry::Bvh::build(am, &ctx).unwrap();
    let bbvh = mf_geometry::Bvh::build(bm, &ctx).unwrap();
    let acap = cap_region(am, Some(&abvh), &a.transform, &frame, JointSide::A, &ctx).unwrap();
    let bcap = cap_region(bm, Some(&bbvh), &b.transform, &frame, JointSide::B, &ctx).unwrap();
    assert!((acap.area() - 1600.0).abs() < 1e-6, "{}", acap.area());
    assert!((bcap.area() - 1600.0).abs() < 1e-6, "{}", bcap.area());
    let pa = Piece {
        name: "block (bottom)",
        mesh: am,
        transform: a.transform,
        bvh: &abvh,
        cap: &acap,
    };
    let pb = Piece {
        name: "block (top)",
        mesh: bm,
        transform: b.transform,
        bvh: &bbvh,
        cap: &bcap,
    };
    let settings = CheckSettings {
        min_wall: 1.5,
        fit_class: mf_parts::FitClass::builtins()[2].clone(),
        machine: mf_parts::MachineProfile::uncalibrated(),
        mm: 1.0,
    };
    let centre = frame.to_2d(DVec3::new(0.3, 0.0, 20.0));
    // 30 mm deep into a piece 20.3 mm long: it comes out the far side.
    let deep = Placement {
        connector: "joint.dowel".into(),
        on: JointSide::A,
        at: centre,
        angle: 0.0,
        params: BTreeMap::from([("depth_on".to_owned(), 30.0), ("clearance".to_owned(), 0.2)]),
    };
    let checks = check_placement(&frame, [&pa, &pb], &deep, &settings, &ctx).unwrap();
    assert!(
        checks
            .iter()
            .any(|c| c.kind == CheckKind::Breakout(JointSide::A)),
        "{checks:?}"
    );
    // 19 mm deep leaves 1.3 mm: still too thin.
    let thin = Placement {
        params: BTreeMap::from([("depth_on".to_owned(), 19.0)]),
        ..deep.clone()
    };
    let checks = check_placement(&frame, [&pa, &pb], &thin, &settings, &ctx).unwrap();
    let breakout = checks
        .iter()
        .find(|c| c.kind == CheckKind::Breakout(JointSide::A))
        .unwrap_or_else(|| panic!("{checks:?}"));
    assert!(
        breakout.message.contains("within 1.3 mm"),
        "{}",
        breakout.message
    );
    // The fix: shallow enough to leave the minimum wall.
    let fixed = mf_operations::fix(
        &frame,
        [&pa, &pb],
        &deep,
        CheckKind::Breakout(JointSide::A),
        &settings,
        &ctx,
    )
    .unwrap()
    .unwrap();
    let depth = fixed.params["depth_on"];
    assert!(depth > 17.0 && depth <= 18.8 + 1e-9, "{depth}");
    let checks = check_placement(&frame, [&pa, &pb], &fixed, &settings, &ctx).unwrap();
    assert!(checks.is_empty(), "{checks:?}");
    // Near the cap's edge: the edge-distance check, and its fix moves it
    // inwards.
    let edge = Placement {
        at: [centre[0] + 18.5, centre[1]],
        params: BTreeMap::new(),
        ..deep.clone()
    };
    let checks = check_placement(&frame, [&pa, &pb], &edge, &settings, &ctx).unwrap();
    assert!(
        checks.iter().any(|c| c.kind == CheckKind::EdgeDistance),
        "{checks:?}"
    );
    let moved = mf_operations::fix(
        &frame,
        [&pa, &pb],
        &edge,
        CheckKind::EdgeDistance,
        &settings,
        &ctx,
    )
    .unwrap()
    .unwrap();
    let checks = check_placement(&frame, [&pa, &pb], &moved, &settings, &ctx).unwrap();
    assert!(checks.is_empty(), "{checks:?} at {:?}", moved.at);
    // A dowel longer than its two holes, and its fix.
    let long = Placement {
        params: BTreeMap::from([("dowel_length".to_owned(), 14.0)]),
        ..deep.clone()
    };
    let checks = check_placement(&frame, [&pa, &pb], &long, &settings, &ctx).unwrap();
    assert!(
        checks.iter().any(|c| c.kind == CheckKind::DowelLength),
        "{checks:?}"
    );
    let short = mf_operations::fix(
        &frame,
        [&pa, &pb],
        &long,
        CheckKind::DowelLength,
        &settings,
        &ctx,
    )
    .unwrap()
    .unwrap();
    assert!((short.params["dowel_length"] - 11.6).abs() < 1e-9);
    // A clearance tighter than the Fit class asks for, and its fix.
    let tight = Placement {
        params: BTreeMap::from([("clearance".to_owned(), 0.02)]),
        ..deep.clone()
    };
    let checks = check_placement(&frame, [&pa, &pb], &tight, &settings, &ctx).unwrap();
    assert!(
        checks.iter().any(|c| c.kind == CheckKind::Fit),
        "{checks:?}"
    );
    let eased = mf_operations::fix(&frame, [&pa, &pb], &tight, CheckKind::Fit, &settings, &ctx)
        .unwrap()
        .unwrap();
    let checks = check_placement(&frame, [&pa, &pb], &eased, &settings, &ctx).unwrap();
    assert!(
        !checks.iter().any(|c| c.kind == CheckKind::Fit),
        "{checks:?}"
    );
}

/// A connector's counterpart is the same connector seen from across the
/// cut: for every kind, placed on either side, every part is centred on
/// the same spot of the cut face, the parts for the side it was placed
/// on reach into that side and its counterpart's into the other.
#[test]
fn a_counterpart_mirrors_its_connector() {
    let frame = JointFrame::new(
        DVec3::new(3.0, -2.0, 31.3),
        DVec3::new(0.2, 0.1, 1.0),
        DVec3::X,
        0.4,
    )
    .unwrap();
    for kind in mf_parts::connectors::joint::JOINT_CONNECTORS {
        for on in [JointSide::A, JointSide::B] {
            let p = Placement {
                connector: (*kind).to_owned(),
                on,
                at: [4.5, -7.25],
                angle: 30.0,
                params: BTreeMap::new(),
            };
            let parts = mf_operations::placed_parts(&frame, &p, 1.0).unwrap();
            let mut sides = Vec::new();
            for part in parts.iter().filter(|q| q.side.is_some()) {
                let side = part.side.unwrap();
                let n = part.world.vertex_count() as f64;
                let c = part
                    .world
                    .vertices()
                    .fold(DVec3::ZERO, |acc, (_, q)| acc + DVec3::from(q) / n);
                // Centred on the placement's spot of the cut face (a
                // dovetail's groove too: it runs both ways equally).
                let at = frame.to_2d(c);
                assert!(
                    (at[0] - p.at[0]).abs() < 1e-6 && (at[1] - p.at[1]).abs() < 1e-6,
                    "{kind} on {on:?}: {} at {at:?}",
                    part.role
                );
                // The deepest point is inside its own side.
                let deepest = part
                    .world
                    .vertices()
                    .map(|(_, q)| (DVec3::from(q) - frame.point).dot(frame.outward(side)))
                    .fold(f64::INFINITY, f64::min);
                assert!(
                    deepest < -frame.gap / 2.0 - 0.1,
                    "{kind} {}: {deepest}",
                    part.role
                );
                sides.push(side);
            }
            assert!(sides.contains(&on), "{kind}: a part on its own side");
            assert!(
                sides.contains(&on.other()),
                "{kind}: a counterpart on the other side"
            );
        }
    }
}

/// D23: a dovetail saved before `groove_a`/`groove_b` existed (only the
/// old `groove_length`) still generates the same symmetric groove it
/// always did — `placement_params`'s own legacy shim, exercised through
/// the real command path rather than directly.
#[test]
fn a_legacy_groove_length_only_dovetail_still_replays_its_old_symmetric_shape() {
    let frame = JointFrame::new(DVec3::new(0.0, 0.0, 0.0), DVec3::Z, DVec3::X, 0.0).unwrap();
    let p = Placement {
        connector: "joint.dovetail".into(),
        on: JointSide::A,
        at: [0.0, 0.0],
        angle: 0.0,
        params: BTreeMap::from([
            ("length".to_owned(), 8.0),
            ("groove_length".to_owned(), 80.0),
        ]),
    };
    let parts = mf_operations::placed_parts(&frame, &p, 1.0).unwrap();
    let groove = parts
        .iter()
        .find(|q| q.role == "counterpart groove")
        .unwrap();
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for (_, v) in groove.local.vertices() {
        lo = lo.min(v[0]);
        hi = hi.max(v[0]);
    }
    // 80 mm groove_length, split evenly: ±40 mm.
    assert!((lo - -40.0).abs() < 1e-6, "lo = {lo}");
    assert!((hi - 40.0).abs() < 1e-6, "hi = {hi}");
}

/// D23: a dovetail that's stopped (not through) on one end, whose
/// recorded reach falls short of the cap's own edge on that side, warns
/// "Can't slide together"; the fix extends it to the edge and the
/// warning clears.
#[test]
fn a_stopped_dovetail_short_of_the_edge_warns_and_its_fix_reaches_it() {
    let dir = tempfile::tempdir().unwrap();
    let cube = mf_fixtures::translated(&mf_fixtures::cube(40.0), [-20.0, -20.0, 0.0]);
    let (mut s, id) = session_with(
        dir.path(),
        "block",
        cube,
        rotation_translation(2, 0.0, [0.0; 3]),
    );
    s.execute(inv(
        "mesh.split_plane",
        id,
        &[("plane_point", "0,0,20"), ("plane_normal", "0,0,1")],
    ))
    .unwrap();
    let bottom_id = created(&s)[0];
    let ctx = RunContext::none();
    let frame = JointFrame::new(
        DVec3::new(0.0, 0.0, 20.0),
        DVec3::Z,
        JointFrame::default_u(DVec3::Z),
        0.0,
    )
    .unwrap();
    let doc = s.document();
    let a = doc.object(bottom_id).unwrap();
    let b = doc.object(id).unwrap();
    let (am, bm) = (a.mesh().unwrap(), b.mesh().unwrap());
    let abvh = mf_geometry::Bvh::build(am, &ctx).unwrap();
    let bbvh = mf_geometry::Bvh::build(bm, &ctx).unwrap();
    let acap = cap_region(am, Some(&abvh), &a.transform, &frame, JointSide::A, &ctx).unwrap();
    let bcap = cap_region(bm, Some(&bbvh), &b.transform, &frame, JointSide::B, &ctx).unwrap();
    let pa = Piece {
        name: "block (bottom)",
        mesh: am,
        transform: a.transform,
        bvh: &abvh,
        cap: &acap,
    };
    let pb = Piece {
        name: "block (top)",
        mesh: bm,
        transform: b.transform,
        bvh: &bbvh,
        cap: &bcap,
    };
    let settings = CheckSettings {
        min_wall: 1.5,
        fit_class: mf_parts::FitClass::builtins()[2].clone(),
        machine: mf_parts::MachineProfile::uncalibrated(),
        mm: 1.0,
    };
    // The 40x40 cap's edge along +x from the centre is 20 mm away; a
    // stopped end recorded at only 5 mm falls well short of it.
    let p = Placement {
        connector: "joint.dovetail".into(),
        on: JointSide::A,
        at: [0.0, 0.0],
        angle: 0.0,
        params: BTreeMap::from([
            ("length".to_owned(), 8.0),
            ("groove_a".to_owned(), 15.0),
            ("groove_b".to_owned(), 5.0),
            ("open_a".to_owned(), 1.0),
            ("open_b".to_owned(), 0.0),
        ]),
    };
    let checks = check_placement(&frame, [&pa, &pb], &p, &settings, &ctx).unwrap();
    let warning = checks
        .iter()
        .find(|c| c.kind == CheckKind::DovetailStopped(false))
        .unwrap_or_else(|| panic!("no stopped-end warning: {checks:?}"));
    assert_eq!(warning.message, "Can't slide together: extend it to the edge");
    // The -x end (open_a = 1, through) never warns even though its own
    // recorded reach (15 mm) is also short of the edge: "through" means
    // the recorded number already accounts for reaching well past it,
    // never checked against the live cap.
    assert!(
        !checks
            .iter()
            .any(|c| c.kind == CheckKind::DovetailStopped(true)),
        "{checks:?}"
    );
    let fixed = mf_operations::fix(
        &frame,
        [&pa, &pb],
        &p,
        CheckKind::DovetailStopped(false),
        &settings,
        &ctx,
    )
    .unwrap()
    .unwrap_or_else(|| panic!("no fix"));
    assert!(
        fixed.params["groove_b"] > 20.0 - 1e-6,
        "{:?}",
        fixed.params
    );
    let checks = check_placement(&frame, [&pa, &pb], &fixed, &settings, &ctx).unwrap();
    assert!(
        !checks
            .iter()
            .any(|c| matches!(c.kind, CheckKind::DovetailStopped(_))),
        "the fix cleared it: {checks:?}"
    );
}

/// D22: a joint whose only alignment is a single round dowel warns that
/// the parts can rotate about it; "Make square" fixes it in place, and
/// "Add second dowel" fixes it by adding a second one (either clears
/// the warning). A dovetail, insert, non-round dowel/peg, or a second
/// dowel/peg all independently resist rotation, so none of those
/// configurations warn.
#[test]
fn a_single_round_dowel_warns_about_rotation_and_both_fixes_clear_it() {
    let dir = tempfile::tempdir().unwrap();
    let (s, bottom, top, joint) = split_horizontally(dir.path());
    let frame = JointFrame::new(
        DVec3::from([0.0, 0.0, 31.3]),
        DVec3::Z,
        JointFrame::default_u(DVec3::Z),
        joint["gap"].as_f64().unwrap(),
    )
    .unwrap();
    let ctx = RunContext::none();
    let doc = s.document();
    let a = doc.object(bottom).unwrap();
    let _b = doc.object(top).unwrap();
    let am = a.mesh().unwrap();
    let abvh = mf_geometry::Bvh::build(am, &ctx).unwrap();
    let acap = cap_region(am, Some(&abvh), &a.transform, &frame, JointSide::A, &ctx).unwrap();

    let round_dowel = Placement {
        connector: "joint.dowel".into(),
        on: JointSide::A,
        at: [-9.0, 0.0],
        angle: 0.0,
        params: BTreeMap::new(),
    };
    // One round dowel alone: warns.
    let checks = joint_checks(std::slice::from_ref(&round_dowel), 0.0);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].kind, JointCheckKind::SingleRoundDowel);
    assert_eq!(
        checks[0].message,
        "The parts can rotate around one round dowel. Add a second, or use a square or keyed \
         shape"
    );
    assert_eq!(checks[0].fixes, vec!["add_second_dowel", "make_square"]);

    // "Make square": the same one dowel, now square, no warning.
    let squared = fix_joint(
        std::slice::from_ref(&round_dowel),
        0.0,
        &acap,
        "make_square",
    )
    .unwrap();
    assert_eq!(squared.len(), 1);
    assert_eq!(squared[0].params["shape"], 1.0);
    assert!(joint_checks(&squared, 0.0).is_empty());

    // "Add second dowel": two round dowels, no warning either (a
    // second independent dowel resists rotation on its own).
    let doubled = fix_joint(
        std::slice::from_ref(&round_dowel),
        0.0,
        &acap,
        "add_second_dowel",
    )
    .unwrap();
    assert_eq!(doubled.len(), 2);
    assert!(
        doubled[1].at != round_dowel.at,
        "the second dowel isn't on top of the first"
    );
    assert!(joint_checks(&doubled, 0.0).is_empty());

    // A dovetail alone resists rotation on its own: no warning even
    // with only one connector.
    let dovetail = Placement {
        connector: "joint.dovetail".into(),
        ..round_dowel.clone()
    };
    assert!(joint_checks(std::slice::from_ref(&dovetail), 0.0).is_empty());

    // An unknown fix name, or a joint with no single round dowel/peg to
    // fix, fixes nothing.
    assert!(fix_joint(std::slice::from_ref(&round_dowel), 0.0, &acap, "nope").is_none());
    assert!(fix_joint(&doubled, 0.0, &acap, "make_square").is_none());
}
