//! The built-in parametric shapes (`parametric-shapes-brief.md`,
//! "Library additions"), generated in code like the parts library's
//! built-ins (D16). Solids sit on z = 0 and grow along +z, centred on the
//! z axis; hole-like shapes reach down from z = 0 (their axis the local
//! z), with a little extra above it so a cut is clean.

use std::sync::OnceLock;

use mf_geometry::Mesh;
use mf_spec::{OperationSpec, Params};

use crate::connector::ConnectorError;
use crate::connectors::hole_profile::{HoleProfile, hole_outline};
use crate::connectors::joint::{keyed_profile, n_gon, prism_z};
use crate::generator::{GenContext, Generator, ShapePart, TargetSlot};
use crate::shape::{P, ShapeRole, choice, float, int, refuse, spec_of};

/// A built-in shape: its id, name, default role, spec and generator.
struct Simple {
    id: &'static str,
    name: &'static str,
    role: ShapeRole,
    spec: fn() -> &'static OperationSpec,
    make: fn(&Params) -> Result<Mesh, ConnectorError>,
}

impl Generator for Simple {
    fn id(&self) -> &'static str {
        self.id
    }
    fn name(&self) -> &'static str {
        self.name
    }
    fn version(&self) -> u32 {
        1
    }
    fn spec(&self) -> &'static OperationSpec {
        (self.spec)()
    }
    fn role(&self) -> ShapeRole {
        self.role
    }
    fn is_shape(&self) -> bool {
        true
    }
    fn generate(
        &self,
        params: &Params,
        ctx: &GenContext,
    ) -> Result<Vec<ShapePart>, ConnectorError> {
        ctx.run
            .checkpoint()
            .map_err(|_| refuse("cancelled", "cancelled by the user"))?;
        Ok(vec![ShapePart::new(
            "shape",
            self.role,
            TargetSlot::Neither,
            (self.make)(params)?,
        )])
    }
}

macro_rules! spec {
    ($id:literal, $summary:literal, [$($p:expr),* $(,)?]) => {{
        static S: OnceLock<OperationSpec> = OnceLock::new();
        S.get_or_init(|| spec_of($id, $summary, vec![$($p),*]))
    }};
    ($id:literal, $summary:literal, $params:expr) => {{
        static S: OnceLock<OperationSpec> = OnceLock::new();
        S.get_or_init(|| spec_of($id, $summary, $params))
    }};
}

const SEGMENTS: P = P::Int(
    "segments",
    48,
    3,
    512,
    "Points around each round edge (more is smoother)",
);

fn positive(name: &str, v: f64) -> Result<f64, ConnectorError> {
    if v > 0.0 && v.is_finite() {
        Ok(v)
    } else {
        Err(refuse(
            "invalid_parameter",
            format!("{name} must be more than 0 (it's {v})"),
        ))
    }
}

fn make_box(p: &Params) -> Result<Mesh, ConnectorError> {
    let size = [
        positive("width", float(p, "width")?)?,
        positive("depth", float(p, "depth")?)?,
        positive("height", float(p, "height")?)?,
    ];
    let round =
        float(p, "corner_radius")?.clamp(0.0, size.iter().copied().fold(f64::MAX, f64::min) / 2.0);
    let n = if round > 0.0 { 12 } else { 1 };
    Ok(crate::builtin::rounded_box(size, round, n))
}

fn make_cylinder(p: &Params) -> Result<Mesh, ConnectorError> {
    let r = positive("diameter", float(p, "diameter")?)? / 2.0;
    let h = positive("height", float(p, "height")?)?;
    let n = int(p, "segments")? as u32;
    Ok(crate::builtin::revolve(
        &[(0.0, 0.0), (r, 0.0), (r, h), (0.0, h)],
        n,
    ))
}

fn make_sphere(p: &Params) -> Result<Mesh, ConnectorError> {
    use std::f64::consts::FRAC_PI_2;
    let r = positive("diameter", float(p, "diameter")?)? / 2.0;
    let n = int(p, "segments")? as u32;
    let rings = (n / 2).max(2);
    Ok(crate::builtin::revolve(
        &crate::builtin::arc(r, r, -FRAC_PI_2, FRAC_PI_2, rings),
        n,
    ))
}

/// How far a hole-like shape reaches above z = 0, so cutting it from a
/// surface it sits flush on leaves no sliver (mm).
pub const HOLE_OVERSHOOT: f64 = 1.0;

/// The thread sizes on the packet (metric coarse, `thread::COARSE_PITCH`).
const THREAD_SIZES: &[&str] = &[
    "M1", "M1.2", "M1.6", "M2", "M2.5", "M3", "M4", "M5", "M6", "M8", "M10", "M12",
];

/// The fit inputs every hole-like shape takes (parametric-shapes brief,
/// "Fit inputs"): the chosen Fit class and Machine profile by name (for
/// showing and re-picking them), and the values they resolved to when
/// chosen (what the mesh is built from, so replay never reads a
/// profile). The hole is `fit_clearance + machine_tolerance` wider than
/// its nominal size (diametral, as the joint connectors and threads).
fn fit_params() -> Vec<P> {
    vec![
        P::Text("fit_class", "Snug", "The Fit class chosen (design intent)"),
        P::Text(
            "machine_profile",
            crate::machine::UNCALIBRATED_NAME,
            "The Machine profile chosen (measured print tolerance)",
        ),
        P::Num(
            "fit_clearance",
            0.0,
            None,
            None,
            Some("mm"),
            "The Fit class's clearance: the hole is this much wider than its nominal size",
        ),
        P::Num(
            "machine_tolerance",
            0.0,
            None,
            None,
            Some("mm"),
            "The Machine profile's hole undersize compensation, added on top",
        ),
    ]
}

/// The hole-profile inputs (fix item 9, D24): the person's choice
/// (`auto` picks Teardrop when the hole's axis is within 45 degrees of
/// horizontal in the print orientation), what `auto` resolved to
/// (`profile`, 0 round / 1 teardrop / 2 flat-top), and which way the
/// point faces in the shape's own x/y (`up_x`, `up_y`: world up seen
/// from the hole), all recorded so replay is Exact.
fn profile_params(default: &'static str) -> Vec<P> {
    vec![
        P::Choice(
            "hole_profile",
            default,
            &["auto", "round", "teardrop", "flat_top"],
            "The hole's cross-section: auto (teardrop when it prints sideways), round, teardrop or flat-top",
        ),
        P::Num(
            "profile",
            0.0,
            Some(0.0),
            Some(2.0),
            None,
            "What auto resolved to for the current placement: 0 round, 1 teardrop, 2 flat-top",
        ),
        P::Num(
            "up_x",
            0.0,
            Some(-1.0),
            Some(1.0),
            None,
            "Which way the teardrop's point faces, in the shape's own x/y (x part)",
        ),
        P::Num(
            "up_y",
            1.0,
            Some(-1.0),
            Some(1.0),
            None,
            "Which way the teardrop's point faces, in the shape's own x/y (y part)",
        ),
        P::Num(
            "overhang",
            45.0,
            Some(1.0),
            Some(89.0),
            Some("deg"),
            "The steepest overhang the printer handles, from vertical",
        ),
    ]
}

/// The profile a hole-like shape's params ask for: the explicit choice,
/// or what `auto` resolved to.
pub fn chosen_profile(params: &Params) -> HoleProfile {
    match params.get_choice("hole_profile").unwrap_or("round") {
        "teardrop" => HoleProfile::Teardrop,
        "flat_top" => HoleProfile::FlatTop,
        "round" => HoleProfile::Round,
        _ => HoleProfile::from_code(params.get_float("profile").unwrap_or(0.0)),
    }
}

/// A hole's outline of radius `r` with the profile its params ask for.
fn hole_section(params: &Params, r: f64) -> Result<Vec<[f64; 2]>, ConnectorError> {
    let up = [float(params, "up_x")?, float(params, "up_y")?];
    let overhang = float(params, "overhang")?;
    let tol = (r / 500.0).max(1e-4);
    Ok(hole_outline(chosen_profile(params), r, up, overhang, tol))
}

/// The diametral widening a hole-like shape's fit params ask for.
fn fit_widening(params: &Params) -> Result<f64, ConnectorError> {
    Ok(float(params, "fit_clearance")? + float(params, "machine_tolerance")?)
}

fn segments(p: &Params) -> Result<u32, ConnectorError> {
    Ok(int(p, "segments")? as u32)
}

fn make_cone(p: &Params) -> Result<Mesh, ConnectorError> {
    let r0 = positive("bottom_diameter", float(p, "bottom_diameter")?)? / 2.0;
    let r1 = float(p, "top_diameter")?.max(0.0) / 2.0;
    let h = positive("height", float(p, "height")?)?;
    let profile = if r1 > 0.0 {
        vec![(0.0, 0.0), (r0, 0.0), (r1, h), (0.0, h)]
    } else {
        vec![(0.0, 0.0), (r0, 0.0), (0.0, h)]
    };
    Ok(crate::builtin::revolve(&profile, segments(p)?))
}

fn make_torus(p: &Params) -> Result<Mesh, ConnectorError> {
    let ring = positive("diameter", float(p, "diameter")?)? / 2.0;
    let tube = positive("tube_diameter", float(p, "tube_diameter")?)? / 2.0;
    if tube >= ring {
        return Err(refuse(
            "tube_too_thick",
            format!(
                "The tube ({:.2} mm across) is as wide as the ring itself; make the ring wider or the tube thinner.",
                2.0 * tube
            ),
        ));
    }
    Ok(crate::builtin::torus(ring, tube))
}

fn make_capsule(p: &Params) -> Result<Mesh, ConnectorError> {
    let r = positive("diameter", float(p, "diameter")?)? / 2.0;
    let h = positive("height", float(p, "height")?)?;
    if h < 2.0 * r {
        return Err(refuse(
            "capsule_too_short",
            format!(
                "A capsule {:.2} mm across needs to be at least that tall; it's {h:.2} mm.",
                2.0 * r
            ),
        ));
    }
    Ok(crate::builtin::capsule(r, h - 2.0 * r))
}

fn make_prism(p: &Params) -> Result<Mesh, ConnectorError> {
    let n = int(p, "sides")? as u32;
    let r = positive("diameter", float(p, "diameter")?)? / 2.0;
    let h = positive("height", float(p, "height")?)?;
    Ok(prism_z(&n_gon(n, r), 0.0, h))
}

/// A trapezoid prism: `width` across its narrow (bottom) face, widening
/// by tan(`angle`) per side up to `height`, `length` long along y.
fn make_dovetail(p: &Params) -> Result<Mesh, ConnectorError> {
    let w = positive("width", float(p, "width")?)?;
    let h = positive("height", float(p, "height")?)?;
    let l = positive("length", float(p, "length")?)?;
    let t = float(p, "angle")?.to_radians().tan();
    let top = w + 2.0 * h * t;
    // The section in (x, z), counter-clockwise, extruded along w; then
    // (x, z, w) -> (x, -w, z), a rotation, so the winding stays outward.
    let section = [
        [-w / 2.0, 0.0],
        [w / 2.0, 0.0],
        [top / 2.0, h],
        [-top / 2.0, h],
    ];
    let along = prism_z(&section, -l / 2.0, l / 2.0);
    let (positions, triangles) = along.to_arrays();
    let turned = positions.into_iter().map(|[x, z, w]| [x, -w, z]).collect();
    Mesh::new(turned, triangles).map_err(|e| refuse("invalid_mesh", e.to_string()))
}

fn make_keyed_peg(p: &Params) -> Result<Mesh, ConnectorError> {
    let r = positive("diameter", float(p, "diameter")?)? / 2.0;
    let h = positive("height", float(p, "height")?)?;
    let flat = float(p, "flat_ratio")?.clamp(0.1, 0.95) * r;
    Ok(prism_z(
        &keyed_profile(r, flat, (r / 500.0).max(1e-4)),
        0.0,
        h,
    ))
}

fn thread_size(p: &Params) -> Result<(f64, f64), ConnectorError> {
    let size = choice(p, "size")?;
    crate::connectors::thread::lookup(size).ok_or_else(|| {
        refuse(
            "unknown_thread_size",
            format!("'{size}' isn't a metric coarse size on the packet (M1 to M12)"),
        )
    })
}

fn make_threaded_rod(p: &Params) -> Result<Mesh, ConnectorError> {
    let (d, pitch) = thread_size(p)?;
    let l = positive("length", float(p, "length")?)?;
    Ok(crate::connectors::thread::thread_surface(d, pitch, l, 0.0, true, 0.0).0)
}

fn make_nut(p: &Params) -> Result<Mesh, ConnectorError> {
    let (d, pitch) = thread_size(p)?;
    let radial = fit_widening(p)?.max(0.0) / 2.0;
    Ok(crate::connectors::thread::nut(d, pitch, radial))
}

/// The negative of a tapped hole: a real internal thread, `depth` deep
/// below z = 0. Tapping cuts its own thread, so only the machine's hole
/// undersize widens it (as the `thread` connector's `threaded_hole`).
fn make_threaded_hole(p: &Params) -> Result<Mesh, ConnectorError> {
    let (d, pitch) = thread_size(p)?;
    let depth = positive("depth", float(p, "depth")?)?;
    let radial = float(p, "machine_tolerance")?.max(0.0) / 2.0;
    Ok(crate::connectors::thread::thread_surface(
        d,
        pitch,
        depth + HOLE_OVERSHOOT,
        -depth,
        false,
        radial,
    )
    .0)
}

/// A round (or teardrop) hole tool `diameter` across before its fit,
/// `depth` deep below z = 0.
fn round_hole(p: &Params, diameter: &str) -> Result<Mesh, ConnectorError> {
    let d = positive(diameter, float(p, diameter)?)?;
    let depth = positive("depth", float(p, "depth")?)?;
    let r = (d + fit_widening(p)?) / 2.0;
    if r <= 0.0 {
        return Err(refuse(
            "clearance_too_tight",
            "The fit leaves no hole at all; pick a looser Fit class.",
        ));
    }
    Ok(prism_z(&hole_section(p, r)?, -depth, HOLE_OVERSHOOT))
}

fn make_teardrop_cylinder(p: &Params) -> Result<Mesh, ConnectorError> {
    round_hole(p, "diameter")
}

fn make_magnet_pocket(p: &Params) -> Result<Mesh, ConnectorError> {
    round_hole(p, "magnet_diameter")
}

fn make_insert_pocket(p: &Params) -> Result<Mesh, ConnectorError> {
    round_hole(p, "pocket_diameter")
}

/// Every built-in shape, in library order.
pub fn builtin_shapes() -> Vec<&'static dyn Generator> {
    static ALL: OnceLock<Vec<Simple>> = OnceLock::new();
    ALL.get_or_init(|| {
        vec![
            Simple {
                id: "shape.box",
                name: "Box",
                role: ShapeRole::Solid,
                spec: || {
                    spec!(
                        "shape.box",
                        "A box, optionally with rounded corners.",
                        [
                            P::Mm("width", 10.0, 0.01, None, "Size along x"),
                            P::Mm("depth", 10.0, 0.01, None, "Size along y"),
                            P::Mm("height", 10.0, 0.01, None, "Size along z"),
                            P::Mm(
                                "corner_radius",
                                0.0,
                                0.0,
                                None,
                                "Radius of the rounded corners and edges (0: sharp)"
                            ),
                        ]
                    )
                },
                make: make_box,
            },
            Simple {
                id: "shape.cylinder",
                name: "Cylinder",
                role: ShapeRole::Solid,
                spec: || {
                    spec!(
                        "shape.cylinder",
                        "A round cylinder standing on its base.",
                        [
                            P::Mm("diameter", 10.0, 0.01, None, "Diameter"),
                            P::Mm("height", 10.0, 0.01, None, "Height"),
                            SEGMENTS,
                        ]
                    )
                },
                make: make_cylinder,
            },
            Simple {
                id: "shape.sphere",
                name: "Sphere",
                role: ShapeRole::Solid,
                spec: || {
                    spec!(
                        "shape.sphere",
                        "A sphere resting on the plate.",
                        [P::Mm("diameter", 10.0, 0.01, None, "Diameter"), SEGMENTS]
                    )
                },
                make: make_sphere,
            },
            Simple {
                id: "shape.cone",
                name: "Cone",
                role: ShapeRole::Solid,
                spec: || {
                    spec!(
                        "shape.cone",
                        "A cone, or a frustum with a flat top.",
                        [
                            P::Mm("bottom_diameter", 10.0, 0.01, None, "Diameter at the base"),
                            P::Mm(
                                "top_diameter",
                                0.0,
                                0.0,
                                None,
                                "Diameter at the top (0: a point)"
                            ),
                            P::Mm("height", 10.0, 0.01, None, "Height"),
                            SEGMENTS,
                        ]
                    )
                },
                make: make_cone,
            },
            Simple {
                id: "shape.torus",
                name: "Torus",
                role: ShapeRole::Solid,
                spec: || {
                    spec!(
                        "shape.torus",
                        "A ring (doughnut) lying on the plate.",
                        [
                            P::Mm(
                                "diameter",
                                12.0,
                                0.02,
                                None,
                                "Diameter of the ring, through the tube's centre"
                            ),
                            P::Mm("tube_diameter", 4.0, 0.01, None, "Thickness of the tube"),
                        ]
                    )
                },
                make: make_torus,
            },
            Simple {
                id: "shape.capsule",
                name: "Capsule",
                role: ShapeRole::Solid,
                spec: || {
                    spec!(
                        "shape.capsule",
                        "A cylinder with round ends, standing up.",
                        [
                            P::Mm("diameter", 8.0, 0.01, None, "Diameter"),
                            P::Mm(
                                "height",
                                16.0,
                                0.02,
                                None,
                                "Overall height, ends included (at least the diameter)"
                            ),
                        ]
                    )
                },
                make: make_capsule,
            },
            Simple {
                id: "shape.prism",
                name: "N-sided prism",
                role: ShapeRole::Solid,
                spec: || {
                    spec!(
                        "shape.prism",
                        "A regular N-sided prism (a hexagon by default).",
                        [
                            P::Int("sides", 6, 3, 64, "Number of sides"),
                            P::Mm(
                                "diameter",
                                10.0,
                                0.01,
                                None,
                                "Diameter across the corners"
                            ),
                            P::Mm("height", 10.0, 0.01, None, "Height"),
                        ]
                    )
                },
                make: make_prism,
            },
            Simple {
                id: "shape.dovetail",
                name: "Dovetail prism",
                role: ShapeRole::Solid,
                spec: || {
                    spec!(
                        "shape.dovetail",
                        "A dovetail: a trapezoid prism, narrow at its base, for sliding joints.",
                        [
                            P::Mm("width", 8.0, 0.01, None, "Width at the narrow (bottom) face"),
                            P::Mm("height", 5.0, 0.01, None, "Height"),
                            P::Mm("length", 20.0, 0.01, None, "Length along y"),
                            P::Num(
                                "angle",
                                15.0,
                                Some(0.0),
                                Some(60.0),
                                Some("deg"),
                                "How far each flank leans out from vertical"
                            ),
                        ]
                    )
                },
                make: make_dovetail,
            },
            Simple {
                id: "shape.keyed_peg",
                name: "Keyed peg",
                role: ShapeRole::Solid,
                spec: || {
                    spec!(
                        "shape.keyed_peg",
                        "A round peg with one flat (a D), so it can't turn in its hole.",
                        [
                            P::Mm("diameter", 6.0, 0.01, None, "Diameter"),
                            P::Mm("height", 10.0, 0.01, None, "Height"),
                            P::Num(
                                "flat_ratio",
                                0.6,
                                Some(0.1),
                                Some(0.95),
                                None,
                                "Where the flat is, as a fraction of the radius from the centre"
                            ),
                        ]
                    )
                },
                make: make_keyed_peg,
            },
            Simple {
                id: "shape.threaded_rod",
                name: "Threaded rod",
                role: ShapeRole::Solid,
                spec: || {
                    spec!(
                        "shape.threaded_rod",
                        "A length of metric coarse threaded rod (a real helix).",
                        [
                            P::Choice(
                                "size",
                                "M6",
                                THREAD_SIZES,
                                "Metric coarse size on the packet"
                            ),
                            P::Mm("length", 20.0, 1.0, None, "Length"),
                        ]
                    )
                },
                make: make_threaded_rod,
            },
            Simple {
                id: "shape.nut",
                name: "Nut",
                role: ShapeRole::Solid,
                spec: || {
                    spec!("shape.nut", "A metric coarse hex nut with a real internal thread.", {
                        let mut p = vec![P::Choice(
                            "size",
                            "M6",
                            THREAD_SIZES,
                            "Metric coarse size on the packet",
                        )];
                        p.extend(fit_params());
                        p
                    })
                },
                make: make_nut,
            },
            Simple {
                id: "shape.threaded_hole",
                name: "Threaded hole",
                role: ShapeRole::Hole,
                spec: || {
                    spec!(
                        "shape.threaded_hole",
                        "A tool that cuts a tapped (threaded) hole into what it's combined with.",
                        {
                            let mut p = vec![
                                P::Choice(
                                    "size",
                                    "M6",
                                    THREAD_SIZES,
                                    "Metric coarse size on the packet",
                                ),
                                P::Mm("depth", 12.0, 0.5, None, "How deep the hole goes"),
                            ];
                            p.extend(fit_params());
                            p
                        }
                    )
                },
                make: make_threaded_hole,
            },
            Simple {
                id: "shape.teardrop_cylinder",
                name: "Teardrop cylinder",
                role: ShapeRole::Hole,
                spec: || {
                    spec!(
                        "shape.teardrop_cylinder",
                        "A hole tool with a teardrop cross-section, so a sideways hole prints without support.",
                        {
                            let mut p = vec![
                                P::Mm("diameter", 6.0, 0.01, None, "Diameter of the round part (before the fit)"),
                                P::Mm("depth", 20.0, 0.01, None, "How deep the hole goes"),
                            ];
                            p.extend(fit_params());
                            p.extend(profile_params("teardrop"));
                            p
                        }
                    )
                },
                make: make_teardrop_cylinder,
            },
            Simple {
                id: "shape.magnet_pocket",
                name: "Magnet pocket",
                role: ShapeRole::Hole,
                spec: || {
                    spec!(
                        "shape.magnet_pocket",
                        "A pocket that holds a round magnet, sized from the magnet and the fit.",
                        {
                            let mut p = vec![
                                P::Mm("magnet_diameter", 6.0, 0.1, None, "The magnet's diameter"),
                                P::Mm("depth", 3.2, 0.1, None, "How deep the pocket goes (the magnet's thickness plus room)"),
                            ];
                            p.extend(fit_params());
                            p.extend(profile_params("auto"));
                            p
                        }
                    )
                },
                make: make_magnet_pocket,
            },
            Simple {
                id: "shape.insert_pocket",
                name: "Heat-set insert pocket",
                role: ShapeRole::Hole,
                spec: || {
                    spec!(
                        "shape.insert_pocket",
                        "A pocket for a heat-set threaded insert (M3 by default).",
                        {
                            let mut p = vec![
                                P::Mm("pocket_diameter", 4.0, 0.1, None, "The pocket's diameter, from the insert's datasheet"),
                                P::Mm("depth", 5.7, 0.1, None, "How deep the pocket goes (the insert's length plus room)"),
                            ];
                            p.extend(fit_params());
                            p.extend(profile_params("auto"));
                            p
                        }
                    )
                },
                make: make_insert_pocket,
            },
        ]
    })
    .iter()
    .map(|s| s as &dyn Generator)
    .collect()
}
