#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The parametric shapes library (parametric-shapes brief, "Library
//! additions"): every built-in builds a valid closed solid from its own
//! defaults, regenerates the same mesh from its recorded recipe, and a
//! plugin-registered shape joins the registry.

use std::collections::BTreeMap;

use mf_geometry::{Bvh, FaceId, Mesh};
use mf_parts::{GeneratorRecord, Shape, ShapeRole, find_shape, new_record, regenerate, shapes};
use mf_runtime::RunContext;
use mf_spec::{OperationSpec, Params};

fn assert_closed_solid(mesh: &Mesh, label: &str) {
    let ctx = RunContext::none();
    let bvh = Bvh::build(mesh, &ctx).unwrap();
    let all: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
    mf_geometry::validate_solid(mesh, &all, &bvh, &ctx)
        .unwrap()
        .unwrap_or_else(|d| panic!("{label}: not a valid solid: {d}"));
}

fn defaults(id: &str) -> GeneratorRecord {
    new_record(find_shape(id).unwrap(), &BTreeMap::new()).unwrap()
}

fn with(id: &str, raw: &[(&str, &str)]) -> GeneratorRecord {
    let raw = raw
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    new_record(find_shape(id).unwrap(), &raw).unwrap()
}

const LIBRARY: &[&str] = &[
    "shape.box",
    "shape.cylinder",
    "shape.sphere",
    "shape.cone",
    "shape.torus",
    "shape.capsule",
    "shape.prism",
    "shape.dovetail",
    "shape.keyed_peg",
    "shape.threaded_rod",
    "shape.nut",
    "shape.threaded_hole",
    "shape.teardrop_cylinder",
    "shape.magnet_pocket",
    "shape.insert_pocket",
];

#[test]
fn every_library_shape_is_a_closed_solid_from_its_defaults() {
    let ctx = RunContext::none();
    for id in LIBRARY {
        let record = defaults(id);
        let mesh = regenerate(&record, &ctx).unwrap();
        assert_closed_solid(&mesh, id);
        // Deterministic: the same recipe gives the same mesh.
        let again = regenerate(&record, &ctx).unwrap();
        assert_eq!(mesh.to_arrays(), again.to_arrays(), "{id}");
        // The recipe round-trips through its JSON property.
        assert_eq!(
            GeneratorRecord::from_json(&record.to_json()).unwrap(),
            record
        );
    }
}

#[test]
fn solids_sit_on_the_plate_and_holes_reach_down_from_it() {
    let ctx = RunContext::none();
    for id in LIBRARY {
        let shape = find_shape(id).unwrap();
        let bb = regenerate(&defaults(id), &ctx)
            .unwrap()
            .bounding_box()
            .unwrap();
        match shape.role() {
            ShapeRole::Solid => assert!(bb.min[2].abs() < 1e-6, "{id} sits on z = 0: {bb:?}"),
            ShapeRole::Hole => {
                assert!(bb.min[2] < -0.4, "{id} reaches down: {bb:?}");
                assert!(bb.max[2] > 0.0, "{id} pokes above the surface: {bb:?}");
            }
        }
    }
}

#[test]
fn sizes_follow_the_params() {
    let ctx = RunContext::none();
    let bb = |r: &GeneratorRecord| regenerate(r, &ctx).unwrap().bounding_box().unwrap();
    let b = bb(&with(
        "shape.box",
        &[("width", "30"), ("depth", "12"), ("height", "4")],
    ));
    assert!((b.max[0] - b.min[0] - 30.0).abs() < 1e-9);
    assert!((b.max[1] - b.min[1] - 12.0).abs() < 1e-9);
    assert!((b.max[2] - b.min[2] - 4.0).abs() < 1e-9);
    // A magnet pocket is the magnet plus its fit, diametral.
    let m = bb(&with(
        "shape.magnet_pocket",
        &[
            ("magnet_diameter", "8"),
            ("fit_clearance", "0.2"),
            ("machine_tolerance", "0.1"),
            ("hole_profile", "round"),
        ],
    ));
    assert!((m.max[0] - m.min[0] - 8.3).abs() < 0.01, "{m:?}");
    // A 7-sided prism really has 7 sides.
    let p = regenerate(&with("shape.prism", &[("sides", "7")]), &ctx).unwrap();
    assert_eq!(p.vertex_count(), 14);
    // A cone with a top is a frustum.
    let c = bb(&with(
        "shape.cone",
        &[("top_diameter", "4"), ("height", "9")],
    ));
    assert!((c.max[2] - 9.0).abs() < 1e-9);
}

#[test]
fn the_teardrop_cylinder_points_where_its_params_say() {
    let ctx = RunContext::none();
    let up_y = regenerate(&defaults("shape.teardrop_cylinder"), &ctx)
        .unwrap()
        .bounding_box()
        .unwrap();
    // Default: 6 mm round part, teardrop to +y reaching r·√2.
    let r = 3.0;
    assert!(
        (up_y.max[1] - r * std::f64::consts::SQRT_2).abs() < 1e-6,
        "{up_y:?}"
    );
    assert!((up_y.min[1] + r).abs() < 0.02, "{up_y:?}");
    let round = regenerate(
        &with("shape.teardrop_cylinder", &[("hole_profile", "round")]),
        &ctx,
    )
    .unwrap()
    .bounding_box()
    .unwrap();
    assert!(round.max[1] <= r + 1e-9, "{round:?}");
}

#[test]
fn bad_params_are_refused_by_name() {
    let ctx = RunContext::none();
    let tube = with("shape.torus", &[("diameter", "4"), ("tube_diameter", "6")]);
    let err = regenerate(&tube, &ctx).unwrap_err();
    assert!(err.to_string().contains("tube"), "{err}");
    let mut old = defaults("shape.box");
    old.version = 99;
    let err = regenerate(&old, &ctx).unwrap_err();
    assert!(err.to_string().contains("version 99"), "{err}");
}

struct Wedge;

fn wedge_spec() -> &'static OperationSpec {
    static S: std::sync::OnceLock<OperationSpec> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        mf_parts::shape::spec_of(
            "test.wedge",
            "A test wedge.",
            vec![mf_parts::shape::P::Mm("size", 5.0, 0.1, None, "Size")],
        )
    })
}

impl Shape for Wedge {
    fn id(&self) -> &'static str {
        "test.wedge"
    }
    fn name(&self) -> &'static str {
        "Wedge"
    }
    fn version(&self) -> u32 {
        1
    }
    fn spec(&self) -> &'static OperationSpec {
        wedge_spec()
    }
    fn generate(&self, params: &Params, _: &RunContext) -> Result<Mesh, mf_parts::ConnectorError> {
        let s = params.get_float("size").unwrap();
        Ok(Mesh::new(
            vec![[0.0, 0.0, 0.0], [s, 0.0, 0.0], [0.0, s, 0.0], [0.0, 0.0, s]],
            vec![[0, 2, 1], [0, 1, 3], [0, 3, 2], [1, 2, 3]],
        )
        .unwrap())
    }
}

#[test]
fn a_registered_shape_joins_the_registry_once() {
    static WEDGE: Wedge = Wedge;
    mf_parts::register_shape(&WEDGE).unwrap();
    assert!(shapes().iter().any(|s| s.id() == "test.wedge"));
    let mesh = regenerate(&defaults("test.wedge"), &RunContext::none()).unwrap();
    assert_closed_solid(&mesh, "wedge");
    assert!(mf_parts::register_shape(&WEDGE).is_err(), "ids are unique");
    // Built-ins come first, in library order.
    let ids: Vec<&str> = shapes().iter().map(|s| s.id()).collect();
    assert_eq!(&ids[..LIBRARY.len()], LIBRARY);
}
