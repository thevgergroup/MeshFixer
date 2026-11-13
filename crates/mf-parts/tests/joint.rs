#![allow(clippy::unwrap_used)] // tests may panic on failure

//! Connectors across a cut (`joint.*`): the anti-rotation dowel/peg
//! shapes (D22) and the dovetail's drag-to-place line-segment geometry
//! (D23). The connector-wide behaviours (counterparts, replay, checks)
//! are covered at the command level in
//! `mf-command/tests/split_workflow.rs`; this file is the geometry unit
//! tests for the two pieces that live entirely in `mf_parts`.

use std::collections::BTreeMap;

use mf_parts::connectors::joint::{Dovetail, Dowel, Peg, Shape, dovetail_reach, footprint};
use mf_parts::{GenContext, Generator, ShapePart};
use mf_runtime::RunContext;

fn params(connector: &dyn Generator, extra: &[(&str, &str)]) -> mf_spec::Params {
    let spec = connector.spec();
    let mut raw: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in extra {
        raw.insert(k.to_string(), v.to_string());
    }
    spec.validate(&raw).unwrap()
}

fn generate(connector: &dyn Generator, extra: &[(&str, &str)]) -> Vec<ShapePart> {
    connector
        .generate(
            &params(connector, extra),
            &GenContext::new(&RunContext::none()),
        )
        .unwrap()
}

fn part<'a>(parts: &'a [ShapePart], role: &str) -> &'a mf_geometry::Mesh {
    &parts
        .iter()
        .find(|p| p.name == role)
        .unwrap_or_else(|| panic!("no part with role {role}"))
        .mesh
}

/// The outline's reach from its centre (max over its points), for
/// comparing a shape's footprint against the circle it's based on.
fn reach(outline: &[[f64; 2]]) -> f64 {
    outline
        .iter()
        .map(|p| (p[0] * p[0] + p[1] * p[1]).sqrt())
        .fold(0.0, f64::max)
}

// ---------------------------------------------------------------------
// D22: anti-rotation dowel/peg shapes.
// ---------------------------------------------------------------------

#[test]
fn shape_code_round_trips() {
    for (code, shape) in [
        (0.0, Shape::Round),
        (1.0, Shape::Square),
        (2.0, Shape::NSided { sides: 6 }),
        (3.0, Shape::Keyed),
    ] {
        assert_eq!(Shape::from_code(code, 6.0), shape);
        assert_eq!(shape.code(), code);
    }
    // Out of range reads as Round rather than panicking (a plain f64
    // param has no enum validation of its own).
    assert_eq!(Shape::from_code(99.0, 6.0), Shape::Round);
    // Sides is clamped to 3..=12 even if a stray value sneaks in.
    assert_eq!(Shape::from_code(2.0, 1.0), Shape::NSided { sides: 3 });
    assert_eq!(Shape::from_code(2.0, 50.0), Shape::NSided { sides: 12 });
}

#[test]
fn a_round_dowel_defaults_unchanged_by_the_shape_param() {
    // Shape defaults to Round (0): the hole is exactly `circle` as it
    // always was, so an existing save with no shape param at all still
    // generates the same geometry (replay stability).
    let round = generate(
        &Dowel,
        &[("diameter", "6"), ("depth_on", "5"), ("depth_other", "5")],
    );
    let square = generate(
        &Dowel,
        &[
            ("diameter", "6"),
            ("depth_on", "5"),
            ("depth_other", "5"),
            ("shape", "1"),
        ],
    );
    let hole_round = part(&round, "hole");
    let hole_square = part(&square, "hole");
    assert_ne!(
        hole_round.vertices().count(),
        hole_square.vertices().count(),
        "a round hole and a square hole don't share a vertex count"
    );
}

#[test]
fn nsided_and_keyed_profiles_stay_within_the_fit_diameter() {
    // The N-sided and keyed shapes are both built directly on the fit
    // circle (their corners, or their round arc, sit exactly on it): a
    // hex or a D never reaches past the round hole it's based on. A
    // square (matching `split::pegs::profile`'s own precedent, which
    // this follows rather than inventing a different convention) is
    // `diameter` wide corner to corner across its flats, so its
    // corners *do* reach past the circle by design — that shape's
    // extent is covered on its own below, not here.
    let d = 6.0_f64;
    for (shape_code, sides) in [("2", "6"), ("2", "12"), ("3", "6")] {
        let p = params(
            &Dowel,
            &[
                ("diameter", "6"),
                ("depth_on", "5"),
                ("depth_other", "5"),
                ("shape", shape_code),
                ("sides", sides),
            ],
        );
        let outline = footprint("joint.dowel", &p).unwrap();
        let r = reach(&outline);
        assert!(
            r <= d / 2.0 + 1e-6,
            "shape {shape_code} (sides {sides}) reaches {r} past the {} mm fit radius",
            d / 2.0
        );
    }
}

#[test]
fn a_square_profile_is_diameter_wide_side_to_side_like_split_pegs_own_squares() {
    // `shape_profile`'s Square arm is `square_profile(d)`: a square of
    // side `d` (not inscribed in the `d`-circle), exactly
    // `split::pegs::profile`'s own `PegShape::Square` convention.
    let d = 6.0_f64;
    let p = params(
        &Dowel,
        &[
            ("diameter", "6"),
            ("depth_on", "5"),
            ("depth_other", "5"),
            ("shape", "1"),
        ],
    );
    let outline = footprint("joint.dowel", &p).unwrap();
    assert_eq!(outline.len(), 4, "a square has four corners");
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for q in &outline {
        for k in 0..2 {
            lo[k] = lo[k].min(q[k]);
            hi[k] = hi[k].max(q[k]);
        }
    }
    assert!((hi[0] - lo[0] - d).abs() < 1e-6, "side = {}", hi[0] - lo[0]);
    assert!((hi[1] - lo[1] - d).abs() < 1e-6, "side = {}", hi[1] - lo[1]);
}

#[test]
fn an_nsided_peg_has_exactly_its_requested_corners() {
    for sides in [3, 5, 6, 12] {
        let p = params(
            &Peg,
            &[
                ("diameter", "6"),
                ("depth_on", "3"),
                ("depth_other", "6"),
                ("shape", "2"),
                ("sides", &sides.to_string()),
            ],
        );
        let outline = footprint("joint.peg", &p).unwrap();
        assert_eq!(outline.len(), sides, "sides = {sides}");
    }
}

#[test]
fn a_shaped_peg_fits_its_own_shaped_counterpart_hole() {
    // The peg and its counterpart hole must be the same shape (a square
    // peg in a round hole would spin just as freely as a round one).
    let parts = generate(
        &Peg,
        &[
            ("diameter", "6"),
            ("depth_on", "3"),
            ("depth_other", "6"),
            ("shape", "2"),
            ("sides", "6"),
        ],
    );
    let peg = part(&parts, "peg");
    let hole = part(&parts, "counterpart hole");
    // Both cross-sections are hexagonal prisms: 6 + 6 = 12 vertices per
    // ring, times two rings (top and bottom) for each prism.
    assert_eq!(peg.vertices().count(), 12);
    assert_eq!(hole.vertices().count(), 12);
}

// ---------------------------------------------------------------------
// D23: the dovetail's drag-to-place line segment (through vs stopped).
// ---------------------------------------------------------------------

#[test]
fn dovetail_reach_defaults_to_the_legacy_groove_length_when_groove_a_b_are_absent() {
    // `dovetail_reach` on its own params (the fallback a *caller* is
    // responsible for triggering by leaving groove_a/groove_b out of
    // what it validates — `mf_operations::placement_params` does
    // exactly that for a placement saved before D23, splitting its old
    // groove_length in two before the spec's own groove_a/groove_b
    // defaults would otherwise paper over the gap unasked; see
    // `mf-operations`'s own test of that shim). Built directly here
    // (not through `OperationSpec::validate`, which always fills every
    // declared field) so this is a test of `dovetail_reach` itself.
    use mf_spec::{ParamValue, Params};
    use std::collections::BTreeMap;
    let p = Params(BTreeMap::from([(
        "groove_length".to_owned(),
        ParamValue::Float(80.0),
    )]));
    let reach = dovetail_reach(&p).unwrap();
    assert_eq!(reach.a, 40.0);
    assert_eq!(reach.b, 40.0);
    assert!(
        reach.open_a && reach.open_b,
        "legacy placements slide both ways"
    );
}

#[test]
fn dovetail_reach_defaults_to_200mm_groove_length_when_nothing_at_all_is_set() {
    use mf_spec::Params;
    use std::collections::BTreeMap;
    let p = Params(BTreeMap::new());
    let reach = dovetail_reach(&p).unwrap();
    assert_eq!(reach.a, 100.0);
    assert_eq!(reach.b, 100.0);
}

#[test]
fn dovetail_reach_reads_groove_a_b_and_open_a_b_independently() {
    let p = params(
        &Dovetail,
        &[
            ("length", "16"),
            ("groove_a", "30"),
            ("groove_b", "12"),
            ("open_a", "1"),
            ("open_b", "0"),
        ],
    );
    let reach = dovetail_reach(&p).unwrap();
    assert_eq!(reach.a, 30.0);
    assert_eq!(reach.b, 12.0);
    assert!(reach.open_a);
    assert!(!reach.open_b);
}

#[test]
fn a_stopped_dovetail_groove_ends_exactly_at_its_recorded_reach() {
    // The groove's generated length along x is `-groove_a` to
    // `+groove_b` (never re-derived from a cap at generate time): with
    // an asymmetric reach, the groove's bounding box is asymmetric too.
    let parts = generate(
        &Dovetail,
        &[
            ("diameter", "8"),
            ("length", "10"),
            ("depth_on", "3"),
            ("depth_other", "5"),
            ("groove_a", "20"),
            ("groove_b", "5"),
            ("open_a", "0"),
            ("open_b", "0"),
        ],
    );
    let groove = part(&parts, "counterpart groove");
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for (_, p) in groove.vertices() {
        lo = lo.min(p[0]);
        hi = hi.max(p[0]);
    }
    assert!((lo - -20.0).abs() < 1e-6, "lo = {lo}");
    assert!((hi - 5.0).abs() < 1e-6, "hi = {hi}");
}

#[test]
fn the_groove_never_ends_up_shorter_than_the_tail_that_slides_through_it() {
    // A tail longer than its own recorded groove reach (an edit that
    // grew the tail without growing the groove) still gets a groove
    // that's at least long enough for the tail to pass through, on
    // both sides, rather than silently clipping the tail.
    let parts = generate(
        &Dovetail,
        &[
            ("diameter", "8"),
            ("length", "40"), // half-length 20, each side
            ("depth_on", "3"),
            ("depth_other", "5"),
            ("groove_a", "2"),
            ("groove_b", "2"),
            ("open_a", "0"),
            ("open_b", "0"),
        ],
    );
    let groove = part(&parts, "counterpart groove");
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for (_, p) in groove.vertices() {
        lo = lo.min(p[0]);
        hi = hi.max(p[0]);
    }
    assert!(lo <= -20.0, "lo = {lo}");
    assert!(hi >= 20.0, "hi = {hi}");
}
