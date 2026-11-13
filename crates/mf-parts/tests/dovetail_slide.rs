#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The dovetail as a linked parametric pair (`dovetail.slide`, owner
//! decision 2026-11-12): the tail (Solid) and groove (Hole) it
//! generates, and the pure extend-through-edge geometry the desktop's
//! `dovetail_tool::reextend`/`ends_inside` call into
//! (`extended_length`, `exits_piece`). The command-level workflow (place,
//! rotate, apply, check the fit) is covered in
//! `mf-desktop/tests/core_workflows.rs`.

use std::collections::BTreeMap;

use mf_parts::Generator;
use mf_parts::connectors::dovetail_slide::{DovetailSlide, exits_piece, extended_length};
use mf_runtime::RunContext;

fn params(extra: &[(&str, &str)]) -> mf_spec::Params {
    let spec = DovetailSlide.spec();
    let mut raw: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in extra {
        raw.insert(k.to_string(), v.to_string());
    }
    spec.validate(&raw).unwrap()
}

fn generate(extra: &[(&str, &str)]) -> Vec<mf_parts::ShapePart> {
    DovetailSlide
        .generate(
            &params(extra),
            &mf_parts::GenContext::new(&RunContext::none()),
        )
        .unwrap()
}

fn part<'a>(parts: &'a [mf_parts::ShapePart], role: &str) -> &'a mf_geometry::Mesh {
    &parts
        .iter()
        .find(|p| p.name == role)
        .unwrap_or_else(|| panic!("no part with role {role}"))
        .mesh
}

fn x_bounds(mesh: &mf_geometry::Mesh) -> (f64, f64) {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for (_, p) in mesh.vertices() {
        lo = lo.min(p[0]);
        hi = hi.max(p[0]);
    }
    (lo, hi)
}

// ---------------------------------------------------------------------
// Generation: the tail is Solid (joins A), the groove is a Hole cut
// from B, offset by the fit clearance plus the machine tolerance.
// ---------------------------------------------------------------------

#[test]
fn the_tail_joins_side_a_and_the_groove_cuts_side_b() {
    let parts = generate(&[]);
    assert_eq!(parts.len(), 2);
    let tail = parts.iter().find(|p| p.name == "tail").unwrap();
    let groove = parts.iter().find(|p| p.name == "groove").unwrap();
    assert_eq!(tail.attach_to, mf_parts::Side::A);
    assert_eq!(tail.join(), mf_parts::JoinKind::Union);
    assert_eq!(groove.attach_to, mf_parts::Side::B);
    assert_eq!(groove.join(), mf_parts::JoinKind::Subtract);
}

/// The widest span of `mesh` along y, over every vertex (a convex
/// prism's widest cross-section is always at one of its two end rows).
fn widest_y(mesh: &mf_geometry::Mesh) -> f64 {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for (_, p) in mesh.vertices() {
        lo = lo.min(p[1]);
        hi = hi.max(p[1]);
    }
    hi - lo
}

#[test]
fn the_tail_is_narrower_than_the_groove_by_the_fit_clearance_plus_machine_tolerance() {
    // At their widest (the deepest row, where both widen most with the
    // flank angle), the tail is exactly `fit_clearance +
    // machine_tolerance` narrower than the groove: both sections share
    // the same `angle` and the same depth range end (`depth`), so their
    // widening with depth is identical and only the `width` they start
    // from differs.
    let parts = generate(&[
        ("width", "8"),
        ("depth", "5"),
        ("fit_clearance", "0.15"),
        ("machine_tolerance", "0.1"),
    ]);
    let tail_width = widest_y(part(&parts, "tail"));
    let groove_width = widest_y(part(&parts, "groove"));
    assert!(
        (groove_width - tail_width - 0.25).abs() < 1e-6,
        "tail {tail_width}, groove {groove_width}"
    );
}

/// Six times the mesh's signed volume (divergence theorem over its
/// triangles): positive for outward (CCW-from-outside) winding.
fn six_signed_volume(mesh: &mf_geometry::Mesh) -> f64 {
    let (p, t) = mesh.to_arrays();
    t.iter()
        .map(|tri| {
            let [a, b, c] = tri.map(|i| p[i as usize]);
            a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0])
        })
        .sum()
}

#[test]
fn the_tail_and_groove_wind_outward() {
    // `prism_x` builds its own triangles by hand rather than through
    // `joint::prism_x`'s rotate-a-z-prism trick: a positive signed
    // volume confirms the winding actually faces outward (a flipped
    // normal would still close up topologically, which
    // `the_tail_and_groove_are_both_closed_solids` alone can't catch).
    let parts = generate(&[]);
    for role in ["tail", "groove"] {
        let v = six_signed_volume(part(&parts, role));
        assert!(v > 0.0, "{role}'s signed volume should be positive: {v}");
    }
}

#[test]
fn the_tail_and_groove_are_both_closed_solids() {
    // `prism_x` builds its own winding by hand (never `mf_boolean`'s
    // kernel at generate time): both ends plus the four side quads must
    // close up into one watertight solid, same check Parts' own "open
    // stamp" warning uses (`parts_tool.rs`), or Combine would refuse it.
    let parts = generate(&[]);
    for role in ["tail", "groove"] {
        let mesh = part(&parts, role);
        assert!(
            mesh.topology().boundary_loops.is_empty(),
            "{role} isn't closed"
        );
    }
}

#[test]
fn the_tail_runs_the_full_recorded_length_along_the_slide_axis() {
    let parts = generate(&[("length", "20")]);
    let (lo, hi) = x_bounds(part(&parts, "tail"));
    assert!((lo - -10.0).abs() < 1e-6, "lo = {lo}");
    assert!((hi - 10.0).abs() < 1e-6, "hi = {hi}");
}

#[test]
fn a_wider_flank_angle_widens_the_tail_more_with_depth() {
    let shallow = generate(&[("angle", "5"), ("depth", "5")]);
    let steep = generate(&[("angle", "30"), ("depth", "5")]);
    let width_at_y = |mesh: &mf_geometry::Mesh| -> f64 {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for (_, p) in mesh.vertices() {
            lo = lo.min(p[1]);
            hi = hi.max(p[1]);
        }
        hi - lo
    };
    assert!(width_at_y(part(&steep, "tail")) > width_at_y(part(&shallow, "tail")));
}

#[test]
fn the_clearance_as_wide_as_the_tail_is_refused() {
    let spec = DovetailSlide.spec();
    let raw = BTreeMap::from([
        ("width".to_owned(), "8".to_owned()),
        ("fit_clearance".to_owned(), "8".to_owned()),
        ("machine_tolerance".to_owned(), "0".to_owned()),
    ]);
    let params = spec.validate(&raw).unwrap();
    let err = match DovetailSlide.generate(&params, &mf_parts::GenContext::new(&RunContext::none()))
    {
        Ok(_) => panic!("expected clearance_too_wide"),
        Err(e) => e,
    };
    let mf_parts::ConnectorError::Failed { code, .. } = err;
    assert_eq!(code, "clearance_too_wide");
}

// ---------------------------------------------------------------------
// Extend through edge: the pure geometry `dovetail_tool::reextend` and
// `::ends_inside` call into.
// ---------------------------------------------------------------------

#[test]
fn extended_length_grows_to_clear_the_target_on_both_ends_plus_the_margin() {
    // The target spans local x from -3 to 22 (asymmetric about the
    // tail's own centre, x = 0): the longer side (22) sets the needed
    // half-length, so the full length is 2 * (22 + 1mm) = 46.
    let length = extended_length(10.0, -3.0, 22.0);
    assert!((length - 46.0).abs() < 1e-9, "{length}");
}

#[test]
fn extended_length_never_shrinks_a_length_already_longer_than_needed() {
    // A deliberately long dovetail (60 mm) over a small target (-5..5)
    // stays 60 mm; extending only ever grows it.
    let length = extended_length(60.0, -5.0, 5.0);
    assert!((length - 60.0).abs() < 1e-9, "{length}");
}

#[test]
fn extended_length_is_symmetric_about_the_tails_own_centre() {
    // The tail is always centred at local x = 0, so a symmetric target
    // needs exactly `2 * (half + margin)`.
    let length = extended_length(0.0, -10.0, 10.0);
    assert!((length - 22.0).abs() < 1e-9, "{length}");
}

#[test]
fn a_through_tail_that_reaches_the_margin_exits_the_piece() {
    // Half-length 11 clears a target at (-10, 10) by exactly the 1 mm
    // margin on both ends.
    assert!(exits_piece(22.0, -10.0, 10.0));
}

#[test]
fn a_short_tail_that_stays_inside_the_target_does_not_exit() {
    assert!(!exits_piece(10.0, -10.0, 10.0));
}

#[test]
fn a_tail_that_exits_only_on_one_end_still_exits() {
    // Half-length 11 clears the +x end (target hi = 10) but the target's
    // lo (-200) is far beyond the -x end: it still counts as exiting,
    // since it can slide out that one side.
    assert!(exits_piece(22.0, -200.0, 10.0));
}

#[test]
fn reextend_and_exits_piece_agree_a_length_extended_to_clear_always_exits() {
    // The contract `dovetail_tool::reextend` relies on: whatever
    // `extended_length` returns for a target span, `exits_piece` of that
    // same length against the same span is true (the tail that was just
    // extended to clear the piece does, in fact, clear it).
    for (lo, hi) in [(-3.0, 22.0), (-50.0, -40.0), (0.0, 0.0), (-1.0, 1.0)] {
        let length = extended_length(1.0, lo, hi);
        assert!(
            exits_piece(length, lo, hi),
            "length {length} should exit ({lo}, {hi})"
        );
    }
}
