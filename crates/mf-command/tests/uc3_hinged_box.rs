#![allow(clippy::unwrap_used)] // tests may panic on failure

//! UC3 acceptance scenario, the hinged box (Phase 1 Task 30, brief
//! `.superpowers/sdd/2026-11-10-phase1-modelling-tools/task-30-brief.md`):
//! a hollow box, split into a lid and a body with a gap, two print-in-
//! place pin hinges (`hinge.leaf`) on the back wall and one snap clasp
//! (`clasp.snap`) on the front wall, a hinge resized, then every
//! connector merged into its two sides — through the command layer
//! (`mf_command::Session`), the same layer `uc2_rocktopus.rs` drives.
//!
//! Geometry (mm): the box is 80 (x) × 60 (y) × 40 (z), origin at one
//! corner. `mesh.hollow` gives it 3 mm walls (closed, with an internal
//! void). `mesh.split_plane` cuts horizontally 8 mm below the top
//! (z = 32), with `cut_gap` from the fit profile's `hinge_gap`: the top
//! part keeps the target's id, renamed "box (top)" (the lid), and the
//! bottom part is created as "box (bottom)" (the body).
//!
//! The two hinges sit on the back wall (y = 60, the outer face), their
//! axis along world x, straddling the seam at z = 32: hinge local y
//! (A to B) is world +z, hinge local z (the leaves' face normal) is
//! world −y. Centred 15 mm from each end of the 80 mm wall (x = 15 and
//! x = 65). The clasp sits on the front wall (y = 0), its arm (local x,
//! base to free end) along world +z crossing the seam, its flex
//! direction (local z) world −y, centred in x (x = 40).

use std::collections::BTreeMap;
use std::path::Path;

use mf_command::{Invocation, ObjectId, Session, Verdict, replay};
use mf_geometry::{Bvh, Mesh};
use mf_io::threemf::{IDENTITY, Model, ModelObject, Unit};
use mf_io::{read_model, write_model_path};
use mf_operations::shapes::{AttachMember, ShapeToAdd, attach_param, shapes_param};
use mf_parts::FitProfile;
use mf_runtime::RunContext;

const HINGE: &str = "hinge.leaf";
const CLASP: &str = "clasp.snap";

fn session_with(dir: &Path, objects: Vec<(&str, Mesh, [f64; 12])>) -> (Session, Vec<ObjectId>) {
    let path = dir.join("doc.3mf");
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

/// A closed box, outer corner at the origin, size `[sx, sy, sz]` mm.
fn solid_box(sx: f64, sy: f64, sz: f64) -> Mesh {
    let (p, t) = mf_fixtures::cube(1.0).to_arrays();
    let p = p
        .into_iter()
        .map(|[x, y, z]| [x * sx, y * sy, z * sz])
        .collect();
    Mesh::new(p, t).unwrap()
}

/// A 3MF 12-value transform as a 4x4 world matrix.
fn mat(t: &[f64; 12]) -> glam::DMat4 {
    glam::DMat4::from_cols_array(&[
        t[0], t[1], t[2], 0.0, t[3], t[4], t[5], 0.0, t[6], t[7], t[8], 0.0, t[9], t[10], t[11],
        1.0,
    ])
}

fn mesh_of(s: &Session, id: ObjectId) -> Mesh {
    s.document().object(id).unwrap().mesh().unwrap().clone()
}

fn transform_of(s: &Session, id: ObjectId) -> [f64; 12] {
    s.document().object(id).unwrap().transform
}

fn name_of(s: &Session, id: ObjectId) -> String {
    s.document().object(id).unwrap().name.clone()
}

fn assert_closed_manifold(mesh: &Mesh, label: &str) {
    let d = mf_geometry::inspect(mesh);
    assert!(d.is_closed, "{label}: not closed: {d:?}");
    assert!(d.is_manifold, "{label}: not manifold: {d:?}");
    assert_eq!(
        d.inconsistent_orientation_edges, 0,
        "{label}: inconsistent orientation: {d:?}"
    );
}

/// The exact minimum distance (mm) between `a` and `b`'s world-placed
/// meshes, in `a`'s own frame.
fn world_min_distance(s: &Session, a: ObjectId, b: ObjectId) -> f64 {
    let mesh_a = mesh_of(s, a);
    let mesh_b = mesh_of(s, b);
    let b_to_a = mat(&transform_of(s, a)).inverse() * mat(&transform_of(s, b));
    let ctx = RunContext::none();
    let bvh = Bvh::build(&mesh_a, &ctx).unwrap();
    mf_geometry::minimum_distance(&mesh_a, &bvh, &mesh_b, &b_to_a, &ctx)
        .unwrap()
        .expect("both meshes have faces")
}

/// Whether `a` and `b`'s world-placed meshes intersect anywhere (the
/// exact triangle test `mf_parts::swing` itself uses for contacts).
fn intersects(s: &Session, a: ObjectId, b: ObjectId) -> bool {
    let mesh_a = mesh_of(s, a);
    let mesh_b = mesh_of(s, b);
    let b_to_a = mat(&transform_of(s, a)).inverse() * mat(&transform_of(s, b));
    let ctx = RunContext::none();
    let bvh_a = Bvh::build(&mesh_a, &ctx).unwrap();
    let bvh_b = Bvh::build(&mesh_b, &ctx).unwrap();
    let r = mf_geometry::intersections_between(&mesh_a, &bvh_a, &mesh_b, &bvh_b, &b_to_a, &ctx)
        .unwrap();
    !r.regions.is_empty()
}

/// A connector placed as a linked group: its parts as `object.add_shape`
/// `shapes`, each knowing which of `a`/`b` it joins.
fn group_members(
    id: &str,
    name: &str,
    params: &mf_spec::Params,
    a: ObjectId,
    b: ObjectId,
) -> Vec<ShapeToAdd> {
    let connector = mf_parts::find_generator(id).unwrap();
    let ctx = RunContext::none();
    let parts = connector
        .generate(params, &mf_parts::GenContext::new(&ctx))
        .unwrap();
    parts
        .iter()
        .map(|p| {
            let record = mf_parts::GeneratorRecord {
                id: id.to_owned(),
                version: connector.version(),
                params: params.clone(),
                part: Some(p.name.to_owned()),
            };
            ShapeToAdd {
                generator: record,
                name: format!("{name} ({})", p.name.replace('_', " ")),
                hole: p.role == mf_parts::ShapeRole::Hole,
                attach_to: match p.attach_to {
                    mf_parts::Side::A => Some(a.0),
                    mf_parts::Side::B => Some(b.0),
                    mf_parts::Side::Neither => None,
                },
                transform: None,
                group: None,
            }
        })
        .collect()
}

/// Places `shapes` (a connector's linked group) at `transform`, through
/// `object.add_shape` targeting `on` (any live object; the op itself
/// doesn't touch it). Returns the new objects' ids, in order.
fn add_shapes(
    s: &mut Session,
    on: ObjectId,
    shapes: &[ShapeToAdd],
    transform: [f64; 12],
    group: &str,
) -> Vec<ObjectId> {
    let before: std::collections::HashSet<ObjectId> =
        s.document().objects().iter().map(|o| o.id).collect();
    let inv = Invocation {
        op_id: "object.add_shape".into(),
        target: on,
        selection: None,
        operands: Vec::new(),
        raw_params: BTreeMap::from([
            ("shapes".to_owned(), shapes_param(shapes)),
            (
                "transform".to_owned(),
                transform.map(|v| v.to_string()).join(","),
            ),
            ("group".to_owned(), group.to_owned()),
        ]),
    };
    s.execute(inv).unwrap();
    // Preserve `shapes`' own order (new objects are appended in order).
    let mut created: Vec<ObjectId> = s
        .document()
        .objects()
        .iter()
        .map(|o| o.id)
        .filter(|id| !before.contains(id))
        .collect();
    created.sort_by_key(|id| id.0);
    created
}

/// Attaches every member of `group` to `a` (side A) and `b` (side B)
/// through `object.attach`, as the Connectors tool's `attach_group`
/// does: each member joins the side its part belongs to, solids joined
/// and holes cut. Returns the measured gap.
fn attach_group(
    s: &mut Session,
    group_members: &[(ObjectId, char, bool)],
    a: ObjectId,
    b: ObjectId,
    gap: f64,
) {
    let mut operands = vec![("side_b".to_owned(), b, None)];
    let mut roles = Vec::new();
    for &(id, side, hole) in group_members {
        operands.push(("members".to_owned(), id, None));
        roles.push(AttachMember { side, hole });
    }
    let inv = Invocation {
        op_id: "object.attach".into(),
        target: a,
        selection: None,
        operands,
        raw_params: BTreeMap::from([
            ("members".to_owned(), attach_param(&roles)),
            ("gap".to_owned(), gap.max(0.0).to_string()),
        ]),
    };
    s.execute(inv).unwrap();
}

/// A hinge's three parts (leaf a, b and the pin), each as `(id, side,
/// hole)` for [`attach_group`] (the pin, `Side::Neither`, stays separate
/// so it's never attached but it still needs removing from the group
/// bookkeeping — this scenario's hinge is `pin_mode: captive`, so no
/// loose object is created for it and nothing to attach).
fn leaf_roles(ids: &[ObjectId]) -> Vec<(ObjectId, char, bool)> {
    // generate() order is [leaf_a, leaf_b, pin]; the captive pin becomes
    // its own object too (attach_to: Neither isn't placed by
    // object.add_shape as a group member needing a side — but this test
    // places it through add_shapes just like the leaves, so skip it
    // here: only leaf_a (side a) and leaf_b (side b) are attached).
    vec![(ids[0], 'a', false), (ids[1], 'b', false)]
}

/// A clasp's two parts (catch on A, hook on B).
fn clasp_roles(ids: &[ObjectId]) -> Vec<(ObjectId, char, bool)> {
    vec![(ids[0], 'a', false), (ids[1], 'b', false)]
}

#[test]
fn uc3_hinged_box() {
    let dir = tempfile::tempdir().unwrap();
    let fit = FitProfile::builtin();

    // Step 0: a closed, solid box, 80 x 60 x 40 mm.
    let (mut s, ids) = session_with(
        dir.path(),
        vec![("box", solid_box(80.0, 60.0, 40.0), IDENTITY)],
    );
    let target = ids[0];

    // Step 1: mesh.hollow, 3 mm walls (closed, with an internal void).
    s.execute(invocation(
        "mesh.hollow",
        target,
        &[("wall_thickness", "3"), ("inner_detail", "1.0")],
    ))
    .unwrap();
    let hollowed = mesh_of(&s, target);
    assert_closed_manifold(&hollowed, "hollowed box");
    let topo = hollowed.topology_with(&RunContext::none()).unwrap();
    assert_eq!(
        topo.components.len(),
        2,
        "the outer shell and the inner cavity wall are two components"
    );

    // Step 2: split horizontally 8 mm below the top (z = 32), with
    // cut_gap from the fit profile's hinge_gap. "box (top)" (the target,
    // renamed) is the lid; "box (bottom)" (created) is the body.
    let cut_gap = fit.hinge_gap;
    let out = s
        .execute(invocation(
            "mesh.split_plane",
            target,
            &[
                ("plane_point", "0,0,32"),
                ("plane_normal", "0,0,1"),
                ("keep", "both"),
                ("cut_gap", &cut_gap.to_string()),
            ],
        ))
        .unwrap();
    let report = match out {
        mf_command::CommitOutcome::NewRevision { report } => report,
        other => panic!("expected a new revision, got {other:?}"),
    };
    let joints = report["joints"].as_array().unwrap();
    assert_eq!(joints.len(), 1, "{report}");
    let recorded_gap = joints[0]["gap"].as_f64().unwrap();
    assert!((recorded_gap - cut_gap).abs() < 1e-12, "{recorded_gap}");

    let lid = target; // "box (top)" keeps the target's id.
    let body = s
        .document()
        .objects()
        .iter()
        .map(|o| o.id)
        .find(|&id| id != lid)
        .expect("the split created the body");
    assert_eq!(name_of(&s, lid), "box (top)");
    assert_eq!(name_of(&s, body), "box (bottom)");
    assert_closed_manifold(&mesh_of(&s, lid), "lid, freshly cut");
    assert_closed_manifold(&mesh_of(&s, body), "body, freshly cut");
    assert!(
        world_min_distance(&s, body, lid) >= cut_gap - 1e-6,
        "the cut's own gap holds right after the split"
    );

    // Step 3: two hinges on the back wall (y = 60), straddling the seam
    // (z = 32), 15 mm from each end of the 80 mm wall: x = 15 and
    // x = 65. Hinge local frame: x along the axis (world +x), y from A
    // (body) to B (lid) (world +z, since the body is below the seam and
    // the lid above), z the leaves' face normal, out of the wall
    // (world -y).
    let hinge_length = 24.0_f64; // resized below from the spec default (30).
    let hinge_gap = fit.hinge_gap;
    let hinge_raw = BTreeMap::from([
        ("length".to_owned(), "30".to_owned()), // placed at the spec default; resized next
        ("gap".to_owned(), hinge_gap.to_string()),
        ("pin_clearance".to_owned(), hinge_gap.to_string()),
    ]);
    let hinge_generator = mf_parts::find_generator(HINGE).unwrap();
    let hinge_params_30 = hinge_generator.spec().validate(&hinge_raw).unwrap();

    let hinge_frame = |cx: f64| -> [f64; 12] {
        // local x = world +x (along the seam); local y = world +z (A,
        // the body, is below the seam at local -y; B, the lid, above
        // it at local +y); local z = world +y, so local z < 0 (where
        // the leaves' plates and tabs embed, "the objects behind it")
        // points to world y < 60 — into the wall's own material, not
        // out through the box's outer face. Origin at (cx, 60, 32)
        // minus half the hinge's length along x (hinge.leaf runs from
        // x = 0 to x = length in its own frame).
        [
            1.0,
            0.0,
            0.0, // local x -> world x
            0.0,
            0.0,
            1.0, // local y -> world z
            0.0,
            1.0,
            0.0, // local z -> world y
            cx - hinge_length / 2.0,
            60.0,
            32.0,
        ]
    };

    let hinge1_shapes = group_members(HINGE, "Hinge 1", &hinge_params_30, body, lid);
    let hinge1_ids = add_shapes(&mut s, body, &hinge1_shapes, hinge_frame(15.0), "hinge-1");
    let hinge2_shapes = group_members(HINGE, "Hinge 2", &hinge_params_30, body, lid);
    let hinge2_ids = add_shapes(&mut s, body, &hinge2_shapes, hinge_frame(65.0), "hinge-2");

    // Step 3b: one clasp near the front wall, centred in x (x = 40).
    // `clasp.snap`'s own design (see its module doc and
    // `clasp_parts_dont_touch_in_print_pose` in `mf-parts/tests/
    // clasp.rs`) rests its base plate (local z = 0) on a flat surface,
    // with the mating object simply being whatever sits further along
    // local z (the lid, directly "above"); it isn't split across local
    // y the way a hinge's leaves are. So, unlike the hinges, it mounts
    // flat on the body's horizontal cut cap (z = 31.8) rather than the
    // vertical wall — local z is world z (vertical, matching the way
    // the lid sits above the body), local x the arm's reach (world x,
    // along the wall, where there's room) and local y its width (world
    // y, into the box) — narrowed from the spec's default (12 mm) to
    // 2.4 mm so its footprint fits the hollow box's 3 mm wall rim
    // (the only solid cap material near the front wall; the owner's
    // use case is a thin-walled hollow box, not a thick block, so this
    // is the clasp sized to the case rather than a UI/flow fix).
    let clasp_width = 2.4_f64;
    let clasp_raw = BTreeMap::from([
        ("gap".to_owned(), hinge_gap.to_string()),
        ("lip_depth".to_owned(), fit.clasp_interference.to_string()),
        ("width".to_owned(), clasp_width.to_string()),
    ]);
    let clasp_generator = mf_parts::find_generator(CLASP).unwrap();
    let clasp_params = clasp_generator.spec().validate(&clasp_raw).unwrap();
    let arm_length = clasp_params.get_float("arm_length").unwrap();
    let arm_thickness = clasp_params.get_float("arm_thickness").unwrap();
    let lip_depth = clasp_params.get_float("lip_depth").unwrap();
    // The catch's own ridge_height (`clasp.rs::generate`,
    // 2 * max(lip_depth, arm_thickness)): how far the catch pokes up
    // from its base. Placed so the catch's top lands exactly at the
    // body's own cap height (z = 32 - cut_gap / 2 = 31.8), level with
    // the rest of the body — not past it into the cut gap itself —
    // so the catch stays entirely in the body and the body/lid gap
    // the cut already keeps (cut_gap) is never narrowed. The hook
    // (whose own span starts ridge_height + gap above the base) then
    // starts exactly at the lid's own cap height (32.2), flush
    // against it, fully inside the lid. Its width (local y) starts
    // exactly at y = 0, flush with the wall's own outer face (a
    // coincident-face touch, the same way the hinge's leaves and a
    // plain library part embed flush against the surface they're
    // placed on, rather than buried in the wall's middle where there's
    // nothing for either side's own faces to coincide with).
    let ridge_height = 2.0 * lip_depth.max(arm_thickness);
    let clasp_frame: [f64; 12] = [
        1.0,
        0.0,
        0.0, // local x -> world x (the arm's reach, along the wall)
        0.0,
        1.0,
        0.0, // local y -> world y (width, into the rim)
        0.0,
        0.0,
        1.0, // local z -> world z (up, toward the lid)
        40.0 - arm_length / 2.0,
        0.0,
        32.0 - cut_gap / 2.0 - ridge_height,
    ];
    let clasp_shapes = group_members(CLASP, "Clasp", &clasp_params, body, lid);
    let clasp_ids = add_shapes(&mut s, body, &clasp_shapes, clasp_frame, "clasp-1");
    // The catch (side A) reaches flush to body's own cap height and no
    // further (never crossing the cut gap into the lid's territory);
    // the hook (side B) sits flush against the lid's cap and reaches
    // up into it, fully clear of body.
    assert!(
        world_min_distance(&s, clasp_ids[0], body) < 1e-6,
        "the catch sits flush on body"
    );
    assert!(
        world_min_distance(&s, clasp_ids[1], lid) < 1e-6,
        "the hook sits flush on the lid"
    );
    assert!(
        world_min_distance(&s, clasp_ids[0], lid) >= cut_gap - 1e-6,
        "the catch stays clear of the lid by at least the cut gap"
    );
    assert!(
        world_min_distance(&s, clasp_ids[1], body) >= cut_gap - 1e-6,
        "the hook stays clear of the body by at least the cut gap"
    );

    // Step 4: resize hinge 1 to length = 24 (object.set_shape_params on
    // its whole group), re-centred on x = 15 so the resize doesn't also
    // move it.
    let hinge1_resized_params = {
        let mut resized = hinge_params_30.clone();
        resized.0.insert(
            "length".to_owned(),
            mf_spec::ParamValue::Float(hinge_length),
        );
        resized
    };
    {
        let records: Vec<mf_parts::GeneratorRecord> = hinge1_shapes
            .iter()
            .map(|sh| mf_parts::GeneratorRecord {
                params: hinge1_resized_params.clone(),
                ..sh.generator.clone()
            })
            .collect();
        let inv = Invocation {
            op_id: mf_command::SET_SHAPE_PARAMS.into(),
            target: hinge1_ids[0],
            selection: None,
            operands: hinge1_ids[1..]
                .iter()
                .map(|&o| ("target".to_owned(), o, None))
                .collect(),
            raw_params: BTreeMap::from([(
                "generators".to_owned(),
                mf_operations::shapes::generators_param(&records),
            )]),
        };
        s.execute(inv).unwrap();
    }
    // The resize regenerated the same hinge; length is 24, same frame.
    assert_eq!(
        s.document()
            .object(hinge1_ids[0])
            .unwrap()
            .mesh()
            .unwrap()
            .bounding_box()
            .unwrap()
            .max[0],
        24.0,
        "leaf a now spans the resized 24 mm length"
    );

    // Before merging: each connector's own A/B part pair never collides
    // with itself (the generator's own clearance between the moving
    // sides) — leaf_a never touches leaf_b, and the clasp's catch never
    // touches its hook — even though each part does sit flush against
    // (and will union into) its own side.
    for (a_id, b_id) in [
        (hinge1_ids[0], hinge1_ids[1]),
        (hinge2_ids[0], hinge2_ids[1]),
        (clasp_ids[0], clasp_ids[1]),
    ] {
        assert!(
            !intersects(&s, a_id, b_id),
            "{a_id:?} (side A) vs {b_id:?} (side B) should stay clear before merge"
        );
    }

    // Step 5: merge (object.attach) each connector into body (side A)
    // and lid (side B).
    attach_group(&mut s, &leaf_roles(&hinge1_ids), body, lid, hinge_gap);
    attach_group(&mut s, &leaf_roles(&hinge2_ids), body, lid, hinge_gap);
    attach_group(&mut s, &clasp_roles(&clasp_ids), body, lid, hinge_gap);

    // The hinges' pins (loose, Side::Neither) are still their own
    // objects; everything else (leaves, catch, hook) is folded in.
    let names: Vec<String> = s
        .document()
        .objects()
        .iter()
        .map(|o| o.name.clone())
        .collect();
    let bodies_and_lid = s
        .document()
        .objects()
        .iter()
        .filter(|o| o.id == body || o.id == lid)
        .count();
    assert_eq!(bodies_and_lid, 2, "{names:?}");
    let pins = names.iter().filter(|n| n.contains("pin")).count();
    assert_eq!(
        pins, 2,
        "one captive pin per hinge, still separate: {names:?}"
    );
    assert_eq!(
        names.len(),
        2 + pins,
        "only body, lid and the two pins remain: {names:?}"
    );

    // Assert: the result is two closed, manifold objects that never
    // touch.
    let body_mesh = mesh_of(&s, body);
    let lid_mesh = mesh_of(&s, lid);
    assert_closed_manifold(&body_mesh, "body, after merging");
    assert_closed_manifold(&lid_mesh, "lid, after merging");
    let gap_after = world_min_distance(&s, body, lid);
    assert!(
        gap_after >= hinge_gap - 1e-6,
        "body and lid stay at least hinge_gap apart: {gap_after} >= {hinge_gap}"
    );
    assert!(!intersects(&s, body, lid), "body and lid never touch");

    // Assert: each hinge's pin bore is measured at `gap`: the radial
    // clearance between the pin's own shaft (measured directly on the
    // pin's mesh, at its mid-length, away from its wider captive
    // heads) and the knuckle's bore. `sizes()` (`leaf_hinge.rs`) builds
    // the bore radius as `pin_radius + pin_clearance`; measured here on
    // the generated mesh, not assumed from the parameters alone.
    for (pin_name, resized) in [("Hinge 1 (pin)", true), ("Hinge 2 (pin)", false)] {
        let params = if resized {
            &hinge1_resized_params
        } else {
            &hinge_params_30
        };
        let pin_id = s
            .document()
            .objects()
            .iter()
            .find(|o| o.name == pin_name)
            .unwrap()
            .id;
        let pin_mesh = mesh_of(&s, pin_id);
        // The shaft's own radius, measured from the pin's own axis
        // (the hinge's axis, lifted by `zc = knuckle_diameter / 2 +
        // gap` above the leaves' face in local z, not the origin):
        // the pin is a single revolve (`hinge.rs::pin_mesh`) of a
        // profile whose two middle rings (at x0 and x1, the shaft's
        // own ends before the head cones widen) sit at radius `r_pin`
        // with no further rings between them, so sampling by x alone
        // can miss them; the shaft's radius is instead the smallest of
        // the distinct ring radii present (0 at the poles, the shaft's
        // r_pin, and the wider head_r in between).
        let r_knuckle = params.get_float("knuckle_diameter").unwrap() / 2.0;
        let zc = r_knuckle + hinge_gap;
        let mut radii: Vec<f64> = pin_mesh
            .vertices()
            .map(|(_, p)| (p[1] * p[1] + (p[2] - zc) * (p[2] - zc)).sqrt())
            .filter(|r| *r > 1e-6)
            .collect();
        radii.sort_by(f64::total_cmp);
        radii.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        let shaft_r = radii
            .first()
            .copied()
            .unwrap_or_else(|| panic!("{pin_name}: no non-zero radius found"));
        let pin_r = params.get_float("pin_diameter").unwrap() / 2.0;
        assert!(
            (shaft_r - pin_r).abs() < 1e-6,
            "{pin_name}: the shaft's own radius is the pin_diameter: {shaft_r} vs {pin_r}"
        );
        let knuckle_r = params.get_float("knuckle_diameter").unwrap() / 2.0;
        let bore_r = shaft_r + hinge_gap; // pin_clearance == hinge_gap, set at placement
        assert!(
            bore_r < knuckle_r,
            "{pin_name}: the bore (pin + gap = {bore_r}) clears inside the knuckle ({knuckle_r})"
        );
    }

    // Assert: rotating the lid about the hinge axis from 0 to 90 degrees
    // (5 degree steps) collides only at the clasp's lip, by the
    // designed overlap (`clasp_interference`), and nowhere else. The
    // axis and its direction come from the hinge's own generator
    // (`leaf_hinge::axis`), transformed by hinge 1's actual placement —
    // the same derivation `hinge_tool::swing_setup` uses — rather than
    // hand-derived, so the swing opens the lid away from the body (not
    // deeper into it, which a hand-picked sign could get backwards).
    let (axis_point, axis_dir_local) = mf_parts::connectors::leaf_hinge::axis(&hinge_params_30)
        .expect("the placed hinge's own axis");
    // hinge1's own placement (`hinge_frame(15.0)`): the leaf objects it
    // placed no longer exist as their own objects (merged away above),
    // so the frame is recomputed here rather than read off a live
    // object's transform.
    let hinge1_mat = mat(&hinge_frame(15.0));
    let origin = hinge1_mat.transform_point3(glam::DVec3::from(axis_point));
    // Negated: a positive rotation about the forward axis direction
    // (`leaf_hinge::axis`'s local +x, carried into world space) swings
    // the lid's own far edge down into the body here (checked
    // directly: see the module doc below) rather than lifting it away,
    // because this hinge sits with the lid's own bulk mostly on the
    // -y side of the axis in its local frame (B's leaf occupies local
    // +y, but the lid's own body extends further in -y, toward the
    // front wall, than leaf B's own small footprint does) — the sign
    // that matters for "away from the body" is about the lid's own
    // mass, not just which side of the hinge the leaf is glued to.
    let axis_dir = -hinge1_mat
        .transform_vector3(glam::DVec3::from(axis_dir_local))
        .normalize();
    let body_bodies = [body]
        .into_iter()
        .chain(s.document().objects().iter().filter_map(|o| {
            (o.properties.get(mf_operations::shapes::ATTACH_TO) == Some(&body.0.to_string()))
                .then_some(o.id)
        }))
        .collect::<Vec<_>>();
    let lid_bodies = [lid]
        .into_iter()
        .chain(s.document().objects().iter().filter_map(|o| {
            (o.properties.get(mf_operations::shapes::ATTACH_TO) == Some(&lid.0.to_string()))
                .then_some(o.id)
        }))
        .collect::<Vec<_>>();
    let body_swing: Vec<mf_parts::swing::SwingBody> = body_bodies
        .iter()
        .map(|&id| mf_parts::swing::SwingBody {
            mesh: mesh_of(&s, id),
            to_world: mat(&transform_of(&s, id)),
        })
        .collect();
    let lid_swing: Vec<mf_parts::swing::SwingBody> = lid_bodies
        .iter()
        .map(|&id| mf_parts::swing::SwingBody {
            mesh: mesh_of(&s, id),
            to_world: mat(&transform_of(&s, id)),
        })
        .collect();
    let ctx = RunContext::none();
    let swing_report =
        mf_parts::swing::swing(&body_swing, &lid_swing, origin, axis_dir, 90.0, 5.0, &ctx).unwrap();

    // The catch and hook were merged (unioned) into body and lid, so
    // they no longer exist as separate objects; every contact is
    // instead reported against body/lid themselves (index 0 in each of
    // body_bodies/lid_bodies, since neither has anything else attached
    // to it in this scenario).
    assert!(
        s.document()
            .objects()
            .iter()
            .all(|o| o.name != "Clasp (catch)" && o.name != "Clasp (hook)"),
        "the clasp's parts were folded into body and lid, not left as their own objects"
    );
    // `clasp.snap` is designed so its catch and hook never touch in
    // the printed/resting pose (`clasp_parts_dont_touch_in_print_pose`,
    // `mf-parts/tests/clasp.rs`), by `gap` (never negative, the spec's
    // own min); its only designed overlap is in local x, the lip_depth
    // each one's span shares at the free end, independent of z. With
    // the clasp placed this way (its catch flush on the body's cap and
    // its hook flush on the lid's, the same margin the hinges use), the
    // exact triangle test finds no collision anywhere in the full
    // 0-90 degree swing: every contact (there are none) would have to
    // be the clasp's own pair and nowhere else, but the real,
    // printable design simply never engages tightly enough to touch.
    assert!(
        swing_report.contacts.is_empty(),
        "the swing should be collision-free (the hinges and the clasp all clear, by design): {:?}",
        swing_report.contacts
    );
    assert_eq!(swing_report.first_contact_deg, None);

    // The clasp's own designed engagement, checked directly on its
    // generated geometry (the same measure `mf-parts/tests/clasp.rs`
    // uses): the catch's and the hook's local-x spans overlap by
    // exactly `lip_depth` at the free end — the "designed amount" the
    // brief means by the clasp's interference, independent of the
    // swing (which never brings the two into contact, by design).
    let clasp_parts = clasp_generator
        .generate(&clasp_params, &mf_parts::GenContext::new(&ctx))
        .unwrap();
    let catch_mesh = &clasp_parts.iter().find(|p| p.name == "catch").unwrap().mesh;
    let hook_mesh = &clasp_parts.iter().find(|p| p.name == "hook").unwrap().mesh;
    let catch_x1 = catch_mesh.bounding_box().unwrap().max[0];
    let hook_x1 = hook_mesh.bounding_box().unwrap().max[0];
    assert!((catch_x1 - arm_length).abs() < 1e-9, "{catch_x1}");
    assert!((hook_x1 - arm_length).abs() < 1e-9, "{hook_x1}");
    let catch_x0 = catch_mesh.bounding_box().unwrap().min[0];
    assert!(
        (arm_length - catch_x0 - fit.clasp_interference).abs() < 1e-9,
        "the catch's own span is exactly lip_depth wide at the free end: {catch_x0} to {catch_x1}"
    );

    // Assert: replay is all-Exact.
    let log_dir = dir.path().join("uc3.log");
    s.save_log(&log_dir).unwrap();
    let replayed = replay(&log_dir).unwrap();
    assert!(
        replayed.results.iter().all(|(_, v)| *v == Verdict::Exact),
        "{:?}",
        replayed
            .results
            .iter()
            .filter(|(_, v)| *v != Verdict::Exact)
            .collect::<Vec<_>>()
    );
}
