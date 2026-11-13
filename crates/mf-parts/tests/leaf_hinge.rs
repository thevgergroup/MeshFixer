#![allow(clippy::unwrap_used)] // tests may panic on failure

//! The hinge as three separate pieces (parametric-shapes brief,
//! "Hinges"): leaf A with the odd knuckles, leaf B with the even ones,
//! and the pin; gaps from the fit, measured exactly; and the swing check
//! that reports the first contact angle (UC3: the lid opens to 90° and
//! collides only at the clasp lip).

use std::collections::BTreeMap;

use glam::{DMat4, DVec3};
use mf_geometry::{Bvh, FaceId, Mesh};
use mf_parts::swing::{SwingBody, swing};
use mf_parts::{Generator, Side};
use mf_runtime::RunContext;

fn hinge() -> &'static dyn Generator {
    &mf_parts::connectors::leaf_hinge::LeafHinge
}

fn params(raw: &[(&str, &str)]) -> mf_spec::Params {
    let raw: BTreeMap<String, String> = raw
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    hinge().spec().validate(&raw).unwrap()
}

fn assert_closed_solid(mesh: &Mesh, label: &str) {
    let ctx = RunContext::none();
    let bvh = Bvh::build(mesh, &ctx).unwrap();
    let all: Vec<FaceId> = mesh.faces().map(|(f, _)| f).collect();
    mf_geometry::validate_solid(mesh, &all, &bvh, &ctx)
        .unwrap()
        .unwrap_or_else(|d| panic!("{label}: not a valid solid: {d}"));
}

/// The winding number of `mesh` around `p` (1 inside, 0 outside).
fn winding(mesh: &Mesh, p: DVec3) -> f64 {
    let (pos, tris) = mesh.to_arrays();
    let mut total = 0.0;
    for t in tris {
        let [a, b, c] = t.map(|i| DVec3::from(pos[i as usize]) - p);
        let (la, lb, lc) = (a.length(), b.length(), c.length());
        let num = a.dot(b.cross(c));
        let den = la * lb * lc + a.dot(b) * lc + b.dot(c) * la + c.dot(a) * lb;
        total += 2.0 * num.atan2(den);
    }
    total / (4.0 * std::f64::consts::PI)
}

fn distance(a: &Mesh, b: &Mesh) -> f64 {
    let ctx = RunContext::none();
    let bvh = Bvh::build(a, &ctx).unwrap();
    mf_geometry::minimum_distance(a, &bvh, b, &DMat4::IDENTITY, &ctx)
        .unwrap()
        .unwrap()
}

fn part<'a>(parts: &'a [mf_parts::ShapePart], role: &str) -> &'a mf_parts::ShapePart {
    parts.iter().find(|p| p.name == role).unwrap()
}

/// A hinge produces three separate pieces, leaf A with the odd knuckles
/// (1st, 3rd, 5th), leaf B with the even ones, and the pin.
#[test]
fn a_hinge_is_three_objects_with_alternating_knuckles() {
    let ctx = RunContext::none();
    for (knuckles, mode) in [("5", "captive"), ("4", "separate")] {
        let p = params(&[("knuckles", knuckles), ("pin_mode", mode)]);
        let parts = hinge()
            .generate(&p, &mf_parts::GenContext::new(&ctx))
            .unwrap();
        let roles: Vec<&str> = parts.iter().map(|p| p.name).collect();
        assert_eq!(roles, ["leaf_a", "leaf_b", "pin"]);
        assert_eq!(part(&parts, "leaf_a").attach_to, Side::A);
        assert_eq!(part(&parts, "leaf_b").attach_to, Side::B);
        assert_eq!(part(&parts, "pin").attach_to, Side::Neither);
        for q in &parts {
            assert_closed_solid(&q.mesh, q.name);
        }
        // A point in each knuckle's wall: inside exactly one leaf, A for
        // the odd knuckles and B for the even.
        let n: u32 = knuckles.parse().unwrap();
        let (length, gap, rk, rb) = (30.0, 0.3, 3.0, 1.25 + 0.3);
        let kl = (length - (n - 1) as f64 * gap) / n as f64;
        for k in 0..n {
            let x = k as f64 * (kl + gap) + kl / 2.0;
            let probe = DVec3::new(x, 0.0, rk + gap + (rk + rb) / 2.0);
            let in_a = winding(&part(&parts, "leaf_a").mesh, probe).round() as i64;
            let in_b = winding(&part(&parts, "leaf_b").mesh, probe).round() as i64;
            let want_a = k % 2 == 0;
            assert_eq!(
                (in_a == 1, in_b == 1),
                (want_a, !want_a),
                "{mode}: knuckle {}",
                k + 1
            );
        }
    }
}

/// The gap between the two leaves, and around the pin, is the fit's
/// clearance, measured exactly (`minimum_distance`), never assumed.
#[test]
fn the_gaps_equal_the_fit_clearance() {
    let ctx = RunContext::none();
    for (gap, clearance) in [(0.3, 0.3), (0.45, 0.2)] {
        let p = params(&[
            ("gap", &gap.to_string()),
            ("pin_clearance", &clearance.to_string()),
        ]);
        let parts = hinge()
            .generate(&p, &mf_parts::GenContext::new(&ctx))
            .unwrap();
        let (a, b, pin) = (
            &part(&parts, "leaf_a").mesh,
            &part(&parts, "leaf_b").mesh,
            &part(&parts, "pin").mesh,
        );
        let leaves = distance(a, b);
        assert!(
            (leaves - gap).abs() < 1e-6,
            "leaf to leaf {leaves}, want {gap}"
        );
        let around = distance(a, pin).min(distance(b, pin));
        // The bore and the pin are both 32-sided polygons, so the
        // radial clearance measures between clearance · cos(π/32) (flat
        // to flat) and the clearance itself; the heads sit `gap` clear
        // of the end knuckles.
        let flat = clearance * (std::f64::consts::PI / 32.0).cos();
        assert!(
            around >= flat.min(gap) - 1e-6 && around <= clearance.min(gap) + 1e-6,
            "pin {around}, want about {clearance} (gap {gap})"
        );
    }
}

#[test]
fn no_clearance_is_refused_by_name() {
    let p = params(&[("gap", "0")]);
    let Err(err) = hinge().generate(&p, &mf_parts::GenContext::new(&RunContext::none())) else {
        panic!("a hinge with no gap is refused");
    };
    assert!(err.to_string().contains("fused"), "{err}");
}

fn cuboid(lo: [f64; 3], hi: [f64; 3]) -> Mesh {
    let s = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let unit = mf_fixtures::cube(1.0);
    let (p, t) = unit.to_arrays();
    let p = p
        .into_iter()
        .map(|q| std::array::from_fn(|k| lo[k] + q[k] * s[k]))
        .collect();
    Mesh::new(p, t).unwrap()
}

fn body(mesh: Mesh) -> SwingBody {
    SwingBody {
        mesh,
        to_world: DMat4::IDENTITY,
    }
}

/// UC3's hinged box, in the hinge's own frame: the box (A) and its lid
/// (B) meet across y = 0 with the hinge on their shared back face (z =
/// 0); a snap clasp on the front face, its hook hanging from the lid
/// with a lip under the box's catch. The lid opens to 90° and collides
/// only at the clasp lip; without the clasp it swings the whole 180°
/// clear.
#[test]
fn the_swing_reports_the_first_contact_at_the_clasp_lip() {
    let ctx = RunContext::none();
    let p = params(&[]);
    let parts = hinge()
        .generate(&p, &mf_parts::GenContext::new(&ctx))
        .unwrap();
    let (origin, dir) = mf_parts::connectors::leaf_hinge::axis(&p).unwrap();
    let (origin, dir) = (DVec3::from(origin), DVec3::from(dir));
    let seam = 0.3; // the box and lid are a gap apart
    let box_a = cuboid([-10.0, -40.0, -50.0], [40.0, -seam / 2.0, 0.0]);
    let lid_b = cuboid([-10.0, seam / 2.0, -50.0], [40.0, 15.0, 0.0]);
    // The clasp: a catch ridge on the box's front face, and the lid's
    // hook hanging down past the seam with its lip just under the catch.
    let clasp = ([10.0, -9.0, -56.0], [20.0, 1.0, -49.0]);
    let catch = cuboid([10.0, -6.0, -53.0], [20.0, -3.0, -49.5]);
    let hook = cuboid([10.0, -6.4, -55.5], [20.0, 5.0, -53.4]);
    let lip = cuboid([10.0, -8.0, -55.5], [20.0, -6.4, -51.0]);
    let fixed = vec![
        body(box_a),
        body(part(&parts, "leaf_a").mesh.clone()),
        body(part(&parts, "pin").mesh.clone()),
        body(catch),
    ];
    let moving = vec![
        body(lid_b),
        body(part(&parts, "leaf_b").mesh.clone()),
        body(hook),
        body(lip),
    ];
    let report = swing(&fixed, &moving, origin, dir, 90.0, 1.0, &ctx).unwrap();
    let first = report.first_contact_deg.expect("the clasp lip catches");
    assert!(first > 0.0 && first <= 10.0, "first contact at {first}°");
    for c in &report.contacts {
        let [x, y, z] = c.centroid;
        let inside = (clasp.0[0]..=clasp.1[0]).contains(&x)
            && (clasp.0[1]..=clasp.1[1] + 10.0).contains(&y)
            && (clasp.0[2] - 10.0..=clasp.1[2] + 10.0).contains(&z);
        assert!(
            inside && c.fixed == 3,
            "a contact outside the clasp lip at {}°: {:?} (fixed {}, moving {})",
            c.angle_deg,
            c.centroid,
            c.fixed,
            c.moving
        );
    }
    // Without the clasp the lid swings its whole 180° clear: the hinge's
    // own leaves, knuckles and pin never meet.
    let report = swing(&fixed[..3], &moving[..2], origin, dir, 180.0, 2.0, &ctx).unwrap();
    assert_eq!(
        report.first_contact_deg,
        None,
        "{:?}",
        report.contacts.first()
    );
}
