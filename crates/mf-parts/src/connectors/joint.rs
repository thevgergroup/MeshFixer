//! Connectors across a cut (the Split workflow's step 3, owner
//! 2026-11-11): `joint.dowel`, `joint.peg`, `joint.dovetail`,
//! `joint.magnet` and `joint.insert`, registered with the other built-in
//! connectors so `split.place_connectors` (and `connector.join`) build
//! them from the same registry.
//!
//! **Local frame** (every one of them): z is the cut's normal, from the
//! piece the connector was placed on (side A, "on") into the other piece
//! (side B, "other"); side A's cap is at `z = -gap/2`, side B's at
//! `z = +gap/2`; the connector's centre is at `x = y = 0`, and a
//! dovetail's tail runs along x. Placing a connector on a joint is a
//! rigid move of this frame (never a mirror), so the counterpart on the
//! other side is the same part seen from across the cut.
//!
//! - **dowel:** a hole in both pieces and a loose dowel (`Separate`).
//! - **peg:** a peg unioned onto A, a hole in B.
//! - **dovetail:** a tail unioned onto A, a groove through B.
//! - **magnet:** a pocket in both pieces.
//! - **insert:** a heat-set insert's pocket in A, the screw's clearance
//!   hole through B.
//!
//! Round features are polygons with their corners on the circle and a
//! chordal error of at most a tenth of the clearance, like the Split
//! alignment holes (D12).

use std::sync::OnceLock;

use mf_geometry::Mesh;
use mf_runtime::RunContext;
use mf_spec::{OperationSpec, Params};

use crate::connector::{Connector, ConnectorError, ConnectorPart, ConnectorParts, JoinKind, Side};

/// The flank angle of a dovetail, from the cut's normal (as the Split
/// alignment dovetail's, D12).
pub const DOVETAIL_ANGLE_DEGREES: f64 = 15.0;

/// Every joint connector's id, in the order the Split panel offers them.
pub const JOINT_CONNECTORS: &[&str] = &[
    "joint.dowel",
    "joint.peg",
    "joint.dovetail",
    "joint.magnet",
    "joint.insert",
];

fn refuse(code: &'static str, message: impl Into<String>) -> ConnectorError {
    ConnectorError::Failed {
        code,
        message: message.into(),
    }
}

fn float(params: &Params, name: &str) -> Result<f64, ConnectorError> {
    params
        .get_float(name)
        .map_err(|e| refuse("invalid_parameter", e.to_string()))
}

/// A regular polygon of radius `r` (corners on the circle), counter-
/// clockwise, with chordal error at most `tol` (at least 12 sides).
pub fn circle(r: f64, tol: f64) -> Vec<[f64; 2]> {
    let n = if r > 0.0 && tol > 0.0 && tol < r {
        ((std::f64::consts::PI / (1.0 - tol / r).acos()).ceil() as usize).clamp(12, 512)
    } else {
        12
    };
    (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            [r * a.cos(), r * a.sin()]
        })
        .collect()
}

/// A closed prism: the convex, counter-clockwise `profile` (in x, y)
/// from `z0` to `z1` (`z0 < z1`).
pub fn prism_z(profile: &[[f64; 2]], z0: f64, z1: f64) -> Mesh {
    let n = profile.len() as u32;
    let mut positions: Vec<[f64; 3]> = profile.iter().map(|p| [p[0], p[1], z0]).collect();
    positions.extend(profile.iter().map(|p| [p[0], p[1], z1]));
    let mut triangles = Vec::new();
    for i in 1..n - 1 {
        triangles.push([0, i + 1, i]);
        triangles.push([n, n + i, n + i + 1]);
    }
    for i in 0..n {
        let j = (i + 1) % n;
        triangles.push([i, j, n + j]);
        triangles.push([i, n + j, n + i]);
    }
    Mesh::new(positions, triangles).expect("a convex prism is a valid mesh")
}

/// A closed prism along x: the convex, counter-clockwise `section` in
/// (y, z), from `x0` to `x1`.
fn prism_x(section: &[[f64; 2]], x0: f64, x1: f64) -> Mesh {
    // A prism along z of (y, z) -> (x, y, z) = (z', y, ...): build it
    // along z, then turn (x, y, z) -> (z, x, y), a rotation (never a
    // mirror), so the winding stays outward.
    let along_z = prism_z(section, x0, x1);
    let (p, t) = along_z.to_arrays();
    let turned: Vec<[f64; 3]> = p.into_iter().map(|[a, b, c]| [c, a, b]).collect();
    Mesh::new(turned, t).expect("a turned prism is a valid mesh")
}

/// The chord tolerance for round features of diameter `d` with
/// `clearance` (diametral).
fn chord_tol(d: f64, clearance: f64) -> f64 {
    (clearance.abs() / 10.0).max(d / 1000.0).max(1e-4)
}

/// A dovetail's section, in (y, s) where `s` is the depth into side B
/// from its cap: `width` across at `s = 0`, widening by tan 15° per side
/// with depth, from `s0` to `s1`.
fn dovetail_section(width: f64, s0: f64, s1: f64, cap_b: f64) -> Vec<[f64; 2]> {
    let t = DOVETAIL_ANGLE_DEGREES.to_radians().tan();
    let w = |s: f64| width + 2.0 * s * t;
    // Counter-clockwise in (y, z): z = cap_b + s.
    vec![
        [-w(s0) / 2.0, cap_b + s0],
        [w(s0) / 2.0, cap_b + s0],
        [w(s1) / 2.0, cap_b + s1],
        [-w(s1) / 2.0, cap_b + s1],
    ]
}

/// What each kind's parameters say, read once.
struct Sizes {
    d: f64,
    depth_on: f64,
    depth_other: f64,
    clearance: f64,
    allowance: f64,
    gap: f64,
}

fn sizes(params: &Params, with_allowance: bool) -> Result<Sizes, ConnectorError> {
    let s = Sizes {
        d: float(params, "diameter")?,
        depth_on: float(params, "depth_on")?,
        depth_other: float(params, "depth_other")?,
        clearance: float(params, "clearance")?,
        allowance: if with_allowance {
            float(params, "length_allowance")?
        } else {
            0.0
        },
        gap: float(params, "gap")?,
    };
    if s.clearance >= s.d {
        return Err(refuse(
            "clearance_too_wide",
            format!(
                "The clearance ({:.2} mm) is as wide as the hole ({:.2} mm), so there'd be \
                 nothing left; make the hole wider or the clearance smaller.",
                s.clearance, s.d
            ),
        ));
    }
    Ok(s)
}

fn part(role: &'static str, mesh: Mesh, side: Side, join: JoinKind) -> ConnectorPart {
    ConnectorPart {
        role,
        mesh,
        side,
        join,
    }
}

/// The dowel's length: `dowel_length`, or (0) the two depths plus the
/// gap less the allowance.
pub fn dowel_length(params: &Params) -> Result<f64, ConnectorError> {
    let s = sizes(params, true)?;
    let set = float(params, "dowel_length")?;
    Ok(if set > 0.0 {
        set
    } else {
        s.depth_on + s.depth_other + s.gap - s.allowance
    })
}

/// The longest a dowel can be and still seat: both holes' depths plus
/// the gap.
pub fn dowel_room(params: &Params) -> Result<f64, ConnectorError> {
    let s = sizes(params, true)?;
    Ok(s.depth_on + s.depth_other + s.gap)
}

fn generate(id: &str, params: &Params) -> Result<ConnectorParts, ConnectorError> {
    match id {
        "joint.dowel" => {
            let s = sizes(params, true)?;
            let tol = chord_tol(s.d, s.clearance);
            let (a, b) = (-s.gap / 2.0, s.gap / 2.0);
            let over = s.d;
            let hole = circle(s.d / 2.0, tol);
            let len = dowel_length(params)?;
            if len <= 0.0 {
                return Err(refuse(
                    "dowel_too_short",
                    "The holes are too shallow for a dowel after the length allowance; make \
                     them deeper.",
                ));
            }
            let mid = (s.depth_other - s.depth_on) / 2.0;
            Ok(ConnectorParts {
                parts: vec![
                    part(
                        "hole",
                        prism_z(&hole, a - s.depth_on, a + over),
                        Side::A,
                        JoinKind::Subtract,
                    ),
                    part(
                        "counterpart hole",
                        prism_z(&hole, b - over, b + s.depth_other),
                        Side::B,
                        JoinKind::Subtract,
                    ),
                    part(
                        "dowel",
                        prism_z(
                            &circle((s.d - s.clearance) / 2.0, tol),
                            mid - len / 2.0,
                            mid + len / 2.0,
                        ),
                        Side::Neither,
                        JoinKind::Separate,
                    ),
                ],
            })
        }
        "joint.peg" => {
            let s = sizes(params, true)?;
            let tol = chord_tol(s.d, s.clearance);
            let (a, b) = (-s.gap / 2.0, s.gap / 2.0);
            let stick = s.depth_other - s.allowance;
            if stick <= 0.0 {
                return Err(refuse(
                    "peg_too_short",
                    "The hole is too shallow for a peg after the length allowance; make it \
                     deeper.",
                ));
            }
            Ok(ConnectorParts {
                parts: vec![
                    part(
                        "peg",
                        prism_z(
                            &circle((s.d - s.clearance) / 2.0, tol),
                            a - s.depth_on,
                            b + stick,
                        ),
                        Side::A,
                        JoinKind::Union,
                    ),
                    part(
                        "counterpart hole",
                        prism_z(&circle(s.d / 2.0, tol), b - s.d, b + s.depth_other),
                        Side::B,
                        JoinKind::Subtract,
                    ),
                ],
            })
        }
        "joint.dovetail" => {
            let s = sizes(params, true)?;
            let length = float(params, "length")?;
            let groove_length = float(params, "groove_length")?.max(length);
            let (a, b) = (-s.gap / 2.0, s.gap / 2.0);
            let stick = s.depth_other - s.allowance;
            if stick <= 0.0 {
                return Err(refuse(
                    "groove_too_shallow",
                    "The groove is too shallow for a tail after the length allowance; make it \
                     deeper.",
                ));
            }
            let t = DOVETAIL_ANGLE_DEGREES.to_radians().tan();
            // The groove starts a little outside side B (only B is cut);
            // the tail's root is sunk into side A, narrowing with the
            // flanks, never below half its width at the cut.
            let groove = dovetail_section(s.d, -s.d / 4.0, s.depth_other, b);
            let root = (b - a + s.depth_on).min(0.25 * (s.d - s.clearance) / t);
            let tail = dovetail_section(s.d - s.clearance, -root, stick, b);
            Ok(ConnectorParts {
                parts: vec![
                    part(
                        "tail",
                        prism_x(&tail, -length / 2.0, length / 2.0),
                        Side::A,
                        JoinKind::Union,
                    ),
                    part(
                        "counterpart groove",
                        prism_x(&groove, -groove_length / 2.0, groove_length / 2.0),
                        Side::B,
                        JoinKind::Subtract,
                    ),
                ],
            })
        }
        "joint.magnet" => {
            let s = sizes(params, false)?;
            let tol = chord_tol(s.d, s.clearance);
            let (a, b) = (-s.gap / 2.0, s.gap / 2.0);
            let pocket = circle((s.d + s.clearance) / 2.0, tol);
            Ok(ConnectorParts {
                parts: vec![
                    part(
                        "pocket",
                        prism_z(&pocket, a - s.depth_on, a + s.d),
                        Side::A,
                        JoinKind::Subtract,
                    ),
                    part(
                        "counterpart pocket",
                        prism_z(&pocket, b - s.d, b + s.depth_other),
                        Side::B,
                        JoinKind::Subtract,
                    ),
                ],
            })
        }
        "joint.insert" => {
            let s = sizes(params, false)?;
            let screw = float(params, "screw_diameter")?;
            let tol = chord_tol(s.d, s.clearance);
            let (a, b) = (-s.gap / 2.0, s.gap / 2.0);
            Ok(ConnectorParts {
                parts: vec![
                    part(
                        "insert pocket",
                        prism_z(&circle(s.d / 2.0, tol), a - s.depth_on, a + s.d),
                        Side::A,
                        JoinKind::Subtract,
                    ),
                    part(
                        "counterpart screw hole",
                        prism_z(
                            &circle((screw + s.clearance) / 2.0, tol),
                            b - s.d,
                            b + s.depth_other,
                        ),
                        Side::B,
                        JoinKind::Subtract,
                    ),
                ],
            })
        }
        _ => Err(refuse(
            "unknown_connector",
            format!("'{id}' isn't a joint connector"),
        )),
    }
}

/// The connector's outline on side A's cap (local x, y), for drawing it
/// and for the edge-distance check. A dovetail's is its tail's
/// footprint (the groove runs out of the piece on purpose).
pub fn footprint(id: &str, params: &Params) -> Result<Vec<[f64; 2]>, ConnectorError> {
    let d = float(params, "diameter")?;
    let c = float(params, "clearance")?;
    Ok(match id {
        "joint.dovetail" => {
            let l = float(params, "length")? / 2.0;
            let h = d / 2.0;
            vec![[-l, -h], [l, -h], [l, h], [-l, h]]
        }
        "joint.magnet" => circle((d + c) / 2.0, chord_tol(d, c)),
        "joint.insert" => {
            let screw = float(params, "screw_diameter")?;
            circle((d / 2.0).max((screw + c) / 2.0), chord_tol(d, c))
        }
        _ => circle(d / 2.0, chord_tol(d, c)),
    })
}

/// The mating pair the fit check measures (the exact BVH minimum
/// distance, `crate::check_fit`): the hole (or groove) and the dowel,
/// peg or tail that goes in it, each trimmed back from the hole's mouth
/// and floor so only the flanks are compared (a probe flush with the
/// mouth would touch there whatever the clearance). `None` for a
/// connector with no mating part of its own (magnet, insert: their fit
/// is against a bought part).
pub fn fit_probe(id: &str, params: &Params) -> Result<Option<(Mesh, Mesh)>, ConnectorError> {
    let parts = match id {
        "joint.dowel" | "joint.peg" | "joint.dovetail" => generate(id, params)?,
        _ => return Ok(None),
    };
    let s = sizes(params, true)?;
    let b = s.gap / 2.0;
    let margin = s.clearance.abs().max(0.1);
    let hole = parts
        .parts
        .iter()
        .find(|p| p.side == Side::B && p.join == JoinKind::Subtract)
        .map(|p| p.mesh.clone());
    let Some(hole) = hole else { return Ok(None) };
    // Inside side B's hole: from its mouth to short of its floor.
    let (z0, z1) = (
        b + margin,
        b + s.depth_other - s.allowance.max(margin) - margin,
    );
    if z1 <= z0 {
        return Ok(None);
    }
    let tol = chord_tol(s.d, s.clearance);
    let pin = match id {
        "joint.dovetail" => {
            let length = float(params, "length")?;
            let tail = dovetail_section(s.d - s.clearance, z0 - b, z1 - b, b);
            prism_x(&tail, -length / 2.0 + margin, length / 2.0 - margin)
        }
        _ => prism_z(&circle((s.d - s.clearance) / 2.0, tol), z0, z1),
    };
    Ok(Some((hole, pin)))
}

macro_rules! joint_connector {
    ($name:ident, $id:literal, $file:literal) => {
        pub struct $name;

        impl Connector for $name {
            fn id(&self) -> &'static str {
                $id
            }

            fn version(&self) -> u32 {
                1
            }

            fn spec(&self) -> &'static OperationSpec {
                static SPEC: OnceLock<OperationSpec> = OnceLock::new();
                SPEC.get_or_init(|| {
                    serde_saphyr::from_str(include_str!(concat!(
                        "../../../../specs/connectors/",
                        $file
                    )))
                    .expect(concat!("connectors/", $file, " is a valid spec"))
                })
            }

            fn generate(
                &self,
                params: &Params,
                _ctx: &RunContext,
            ) -> Result<ConnectorParts, ConnectorError> {
                generate($id, params)
            }
        }
    };
}

joint_connector!(Dowel, "joint.dowel", "joint.dowel.yaml");
joint_connector!(Peg, "joint.peg", "joint.peg.yaml");
joint_connector!(Dovetail, "joint.dovetail", "joint.dovetail.yaml");
joint_connector!(Magnet, "joint.magnet", "joint.magnet.yaml");
joint_connector!(Insert, "joint.insert", "joint.insert.yaml");
